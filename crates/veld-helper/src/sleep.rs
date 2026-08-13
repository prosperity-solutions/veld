//! Battery lid-closed sleep — the one thing an unprivileged process cannot hold off.
//!
//! The daemon's own keep-awake (`veld-daemon`'s `caffeinate` module) covers idle
//! sleep on both power sources and lid-closed sleep on AC. What it cannot cover
//! is **a closed lid on battery**: macOS honours `PreventSystemSleep` on AC power
//! only, and there is no unprivileged API for the battery case at all. The only
//! lever is `pmset -b disablesleep`, which needs root — so it lives here.
//!
//! **This lever is not an assertion, and that is the whole design problem.**
//! Every other sleep control in veld is a process holding something: kill the
//! process and the machine can sleep again. `disablesleep` is a durable power
//! management setting. It survives the process that set it, the logout, and the
//! reboot. Set it and walk away and the user has a Mac that never sleeps, with
//! nothing left running to tell them why.
//!
//! So it is held on a **lease that the caller must keep renewing**:
//!
//! - [`SleepManager::hold`] sets it and arms a wall-clock deadline. The daemon
//!   re-issues it well inside that deadline for as long as its own session lives.
//! - [`SleepManager::watchdog_tick`] runs on the helper's watchdog interval and
//!   reverts once the deadline passes with no renewal. A daemon that is killed,
//!   wedged, updated or uninstalled stops renewing, and the setting comes off by
//!   itself.
//! - [`SleepManager::clear_on_startup`] reverts unconditionally, so a helper that
//!   was itself killed mid-lease starts from "the machine can sleep" rather than
//!   inheriting a setting nothing is tracking. The daemon re-establishes within
//!   one renewal if it is still there, which is the right way round: the state
//!   that survives a crash should be the safe one.
//! - The shutdown path reverts too, which is what covers `veld uninstall`.
//!
//! Every one of those paths lands on the same place. The lease deadline is
//! **wall clock, not monotonic**, deliberately: `Instant` on macOS does not
//! advance across a system sleep, and a machine that suspended anyway (because
//! this was never armed, or came off) would otherwise resume with a lease that
//! believes no time passed.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

/// Absolute path rather than a `PATH` lookup: this runs as root, and a `pmset`
/// earlier on some inherited `PATH` would be a root-executed binary of somebody
/// else's choosing.
const PMSET: &str = "/usr/bin/pmset";

/// Longest lease a caller may ask for in one request.
///
/// The point of the lease is that a caller which stops renewing loses the
/// setting, so the ceiling is what bounds "how long can a dead daemon keep this
/// Mac awake". Ten minutes is far above the daemon's renewal interval (so a slow
/// or briefly-wedged daemon never flaps) and far below anything a user would
/// notice as "it stayed on after I killed it".
///
/// An unlimited keep-awake is expressed as *renewing forever*, never as one long
/// lease — a caller that could ask for a year would defeat the watchdog entirely.
pub const MAX_LEASE_SECS: u64 = 600;

/// Clamp a requested lease to [`MAX_LEASE_SECS`].
///
/// Clamped rather than refused: a caller asking for too much still *wants* the
/// machine awake, and refusing would leave it with no hold at all — the wrong
/// direction to fail for a request that is only over-eager. Its own function so
/// the ceiling is testable without executing `pmset`, which a test must never do
/// (run as root it would disable a developer's sleep for real, and the setting
/// is durable).
fn clamp_lease(secs: u64) -> u64 {
    secs.min(MAX_LEASE_SECS)
}

/// Whether a lease has lapsed and the setting must go back.
///
/// Split out from [`SleepManager::watchdog_tick`] so the decision — the one that
/// gives a user's Mac its sleep back — is testable on its own. The tick around
/// it cannot be: reverting runs `pmset`, and a test may never do that.
///
/// `None` is **not** expired: there is nothing to revert, and treating it as
/// expired would have every watchdog tick on an idle machine shell out to
/// `pmset` and log about a lease that was never taken.
fn is_expired(lease: Option<SystemTime>, now: SystemTime) -> bool {
    lease.is_some_and(|deadline| now >= deadline)
}

/// Root's half of the keep-awake switch: `pmset -b disablesleep`, on a lease.
pub struct SleepManager {
    /// Wall-clock deadline of the current lease, or `None` when nothing is held.
    ///
    /// A plain `std::sync::Mutex` and never held across an `await` — every lock
    /// below reads or writes the deadline and drops before touching `pmset`.
    lease: Mutex<Option<SystemTime>>,
}

impl Default for SleepManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SleepManager {
    pub fn new() -> Self {
        Self {
            lease: Mutex::new(None),
        }
    }

    /// Whether a lease is currently armed (for `status`).
    pub fn held(&self) -> bool {
        self.lease.lock().map(|l| l.is_some()).unwrap_or(false)
    }

    /// Disable battery sleep and arm (or renew) the lease.
    ///
    /// `pmset` is re-run on every renewal rather than only on the first call.
    /// It is one cheap exec per renewal interval, and it makes the hold
    /// self-healing: anything else on the machine that resets the setting — the
    /// user's own `pmset`, a management profile — is undone at the next renewal
    /// instead of silently ending the guarantee.
    pub async fn hold(&self, lease_secs: u64) -> Result<()> {
        if !cfg!(target_os = "macos") {
            // Not a failure the caller should paper over — on Linux the
            // unprivileged `handle-lid-switch` inhibitor already covers battery,
            // so a caller reaching here has the wrong idea about the platform.
            bail!("battery lid-closed sleep is a macOS-only concern");
        }
        let secs = clamp_lease(lease_secs);
        let was_held = self.held();
        set_disable_sleep(true).await?;
        {
            let mut lease = self.lease.lock().expect("sleep lease mutex poisoned");
            *lease = Some(SystemTime::now() + Duration::from_secs(secs));
        }
        if !was_held {
            info!(lease_secs = secs, "battery sleep disabled (lease armed)");
        }
        Ok(())
    }

    /// Re-enable battery sleep and drop the lease. Idempotent.
    pub async fn release(&self) -> Result<()> {
        let was_held = {
            let mut lease = self.lease.lock().expect("sleep lease mutex poisoned");
            lease.take().is_some()
        };
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        set_disable_sleep(false).await?;
        if was_held {
            info!("battery sleep re-enabled (lease released)");
        }
        Ok(())
    }

    /// One watchdog iteration: revert once the lease has expired.
    ///
    /// This is the path that runs when the daemon dies, hangs, or is replaced —
    /// nothing tells the helper that happened, so an elapsed deadline is the
    /// signal.
    pub async fn watchdog_tick(&self) {
        let expired = {
            let lease = self.lease.lock().expect("sleep lease mutex poisoned");
            is_expired(*lease, SystemTime::now())
        };
        if !expired {
            return;
        }
        warn!("keep-awake lease expired without renewal — re-enabling battery sleep");
        if let Err(e) = self.release().await {
            warn!(error = %format!("{e:#}"), "could not re-enable battery sleep after lease expiry");
        }
    }

    /// Startup reconcile: revert unconditionally.
    ///
    /// Deliberately *not* "restore whatever was set before we died". A helper
    /// that comes back has no idea whether the daemon that armed the lease still
    /// exists, and the safe answer to not knowing is that the machine can sleep.
    /// A live daemon re-arms within one renewal interval.
    pub async fn clear_on_startup(&self) {
        if !cfg!(target_os = "macos") {
            return;
        }
        match currently_disabled().await {
            Ok(true) => {
                warn!("battery sleep was left disabled by a previous run — re-enabling it");
                if let Err(e) = set_disable_sleep(false).await {
                    warn!(error = %format!("{e:#}"), "could not clear a stale battery sleep disable");
                }
            }
            Ok(false) => {}
            // Worth one line rather than silence: on a Mac with no battery
            // profile this is where that shows up, and it is also how a broken
            // `pmset` would first announce itself.
            Err(e) => info!(error = %format!("{e:#}"), "could not read the current sleep setting"),
        }
    }
}

/// `pmset -b disablesleep <0|1>` — the battery power profile only.
///
/// Never the AC profile: `caffeinate` already holds AC lid-closed sleep off from
/// inside the daemon, where it cannot outlive the process. Writing a durable
/// setting for a case an assertion already covers would trade the safe mechanism
/// for the dangerous one.
async fn set_disable_sleep(disabled: bool) -> Result<()> {
    let value = if disabled { "1" } else { "0" };
    let output = tokio::process::Command::new(PMSET)
        .args(["-b", "disablesleep", value])
        .output()
        .await
        .with_context(|| format!("failed to run {PMSET}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{PMSET} -b disablesleep {value} failed: {}", stderr.trim());
    }
    Ok(())
}

/// Read the live `SleepDisabled` value out of `pmset -g`.
async fn currently_disabled() -> Result<bool> {
    let output = tokio::process::Command::new(PMSET)
        .arg("-g")
        .output()
        .await
        .with_context(|| format!("failed to run {PMSET} -g"))?;
    if !output.status.success() {
        bail!("{PMSET} -g failed");
    }
    Ok(parse_sleep_disabled(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// `pmset -g` prints one `SleepDisabled <0|1>` line among many settings.
///
/// Split out so the parse is testable without a Mac: the surrounding lines are
/// whitespace-aligned key/value pairs, and matching the key loosely (a
/// `contains`) would also match a future `SleepDisabledSomething`.
fn parse_sleep_disabled(text: &str) -> bool {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let key = parts.next()?;
            (key == "SleepDisabled").then(|| parts.next().unwrap_or("0") == "1")
        })
        .next()
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{MAX_LEASE_SECS, SleepManager, clamp_lease, is_expired, parse_sleep_disabled};

    #[test]
    fn the_sleep_disabled_line_is_read_out_of_a_full_pmset_dump() {
        let dump = "System-wide power settings:\n\
                    Currently in use:\n \
                    standby              1\n \
                    Sleep On Power Button 1\n \
                    SleepDisabled        1\n \
                    hibernatemode        3\n";
        assert!(parse_sleep_disabled(dump));
        assert!(!parse_sleep_disabled(
            &dump.replace("SleepDisabled        1", "SleepDisabled        0")
        ));
        // Absent means not disabled — a `pmset` that stops printing the key must
        // not be read as "the machine is pinned awake", which would have the
        // startup reconcile shout about a leftover on every boot.
        assert!(!parse_sleep_disabled("standby 1\nhibernatemode 3\n"));
    }

    /// A key that merely *starts with* the one we want is a different setting.
    #[test]
    fn a_longer_key_that_starts_the_same_is_not_the_sleep_setting() {
        assert!(!parse_sleep_disabled(" SleepDisabledUntilCharge 1\n"));
    }

    /// The ceiling is what bounds how long a dead daemon can keep the machine
    /// awake, so an over-long ask must not survive as one.
    ///
    /// Note what this test does *not* do: call `hold`. That would execute
    /// `pmset` — harmless as an unprivileged developer, and a durable
    /// sleep-disable left on the machine if anyone ever runs the suite as root.
    #[test]
    fn an_over_long_lease_is_clamped_rather_than_honoured() {
        assert_eq!(clamp_lease(u64::MAX), MAX_LEASE_SECS);
        assert_eq!(clamp_lease(MAX_LEASE_SECS + 1), MAX_LEASE_SECS);
        // Under the ceiling passes through untouched — a clamp that rounded
        // every lease up to the ceiling would quietly extend the window a dead
        // daemon keeps the machine awake for.
        assert_eq!(clamp_lease(90), 90);
        assert_eq!(clamp_lease(MAX_LEASE_SECS), MAX_LEASE_SECS);
    }

    /// A fresh manager holds nothing, so `status` cannot report a lease that was
    /// never armed and the startup reconcile has nothing to preserve.
    #[test]
    fn a_new_manager_holds_no_lease() {
        assert!(!SleepManager::new().held());
    }

    /// The expiry decision is the one that gives a Mac its sleep back, so it is
    /// pinned directly rather than inferred from a tick that cannot be run in a
    /// test.
    #[test]
    fn a_lease_expires_at_its_deadline_and_no_lease_never_expires() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let past = now - Duration::from_secs(1);
        let future = now + Duration::from_secs(1);

        assert!(is_expired(Some(past), now));
        // Exactly at the deadline counts as expired: the alternative leaves a
        // lease that only ever lapses on the *next* tick, silently adding the
        // watchdog interval to every ceiling in this module.
        assert!(is_expired(Some(now), now));
        assert!(!is_expired(Some(future), now));

        // No lease is not an expired lease. Reading it as one would have every
        // tick on an idle machine run `pmset` and warn about a lease nobody took.
        assert!(!is_expired(None, now));
    }
}
