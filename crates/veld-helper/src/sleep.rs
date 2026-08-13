//! Battery lid-closed sleep — the one thing an unprivileged process cannot hold off.
//!
//! The daemon's own keep-awake (`veld-daemon`'s `caffeinate` module) covers idle
//! sleep on both power sources and lid-closed sleep on AC. What it cannot cover
//! is **a closed lid on battery**: macOS honours `PreventSystemSleep` on AC power
//! only, and there is no unprivileged API for the battery case at all. The only
//! lever is `pmset disablesleep`, which needs root — so it lives here.
//!
//! **This lever is not an assertion, and that is the whole design problem.**
//! Every other sleep control in veld is a process holding something: kill the
//! process and the machine can sleep again. `disablesleep` is a durable power
//! management setting. It survives the process that set it, the logout, and the
//! reboot. Set it and walk away and the user has a Mac that never sleeps, with
//! nothing left running to tell them why.
//!
//! Two independent rules follow, and they are easy to confuse:
//!
//! **1. veld only ever reverts what veld set.** `disablesleep` is a machine-wide
//! setting with other legitimate owners — a developer who ran `pmset` by hand for
//! a clamshell desk setup, or another tool. A helper that "cleaned up" on every
//! start would silently switch that off, on every `veld update`, with nothing
//! connecting the effect back to veld. So arming writes a **durable marker**
//! recording that veld took the setting and what it was beforehand
//! ([`Marker`]); nothing here touches `pmset` unless that marker says veld owns
//! the value. No marker, no write — including on the startup path, which is
//! deliberately not a blanket "clear it".
//!
//! **2. What veld set, veld gives back — on a lease.** [`SleepManager::hold`]
//! arms a wall-clock deadline the daemon must keep renewing.
//! [`SleepManager::watchdog_tick`] reverts once that deadline passes, so a daemon
//! that is killed, wedged, updated or uninstalled loses the setting by itself.
//! [`SleepManager::clear_on_startup`] hands back a marked setting left by a
//! helper that died mid-lease, and the exit path reverts too, which is what
//! covers `veld uninstall`. A revert that *fails* keeps the lease armed on a
//! short deadline so the watchdog retries — clearing the lease first would strand
//! the setting with nothing left to notice, which is the exact failure this
//! module exists to prevent.
//!
//! The lease deadline is **wall clock, not monotonic**, deliberately: `Instant`
//! on macOS does not advance across a system sleep, and a machine that suspended
//! anyway (because this was never armed, or came off) would otherwise resume with
//! a lease that believes no time passed.
//!
//! Note on `pmset -b`: **`disablesleep` is system-wide, not per power profile.**
//! `pmset -g custom` lists it under neither "Battery Power" nor "AC Power";
//! `pmset -g` reports it under "System-wide power settings". The `-b` that the
//! obvious incantation carries is inert for this key, so it is not passed here —
//! writing it would suggest a scoping that does not exist. Nothing about the
//! safety of this module rests on power-profile scoping; it rests on the marker
//! and the lease above.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
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

/// How soon the watchdog retries after a revert that failed.
///
/// Short, because until it succeeds the machine is held awake by a lease nobody
/// is renewing — the state this module refuses to leave behind.
const REVERT_RETRY: Duration = Duration::from_secs(15);

/// Clamp a requested lease to [`MAX_LEASE_SECS`].
///
/// Clamped rather than refused: a caller asking for too much still *wants* the
/// machine awake, and refusing would leave it with no hold at all — the wrong
/// direction to fail for a request that is only over-eager.
fn clamp_lease(secs: u64) -> u64 {
    secs.min(MAX_LEASE_SECS)
}

/// Whether a lease has lapsed and the setting must go back.
///
/// `None` is **not** expired: there is nothing to revert, and treating it as
/// expired would have every watchdog tick on an idle machine try to hand back a
/// setting that was never taken.
fn is_expired(lease: Option<SystemTime>, now: SystemTime) -> bool {
    lease.is_some_and(|deadline| now >= deadline)
}

/// The durable record that **veld** armed `disablesleep`, and what it was before.
///
/// Its presence is the only thing that authorises a write back. Written before
/// the setting is changed, on purpose: a crash between the write and the change
/// leaves a marker for a setting veld did not actually alter, whose worst
/// outcome is one redundant write of the value that was already there. The
/// reverse order risks the thing that actually matters — a changed setting with
/// no record that veld changed it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
struct Marker {
    /// What `disablesleep` read as before veld touched it. When `true`, somebody
    /// else already had the machine pinned awake and veld must hand the value
    /// back untouched rather than "restoring" it to `false`.
    prior_disabled: bool,
}

/// The two `pmset` operations, behind a trait so the lease state machine can be
/// tested without ever executing `pmset`.
///
/// That indirection is not ceremony. A test that ran the real thing as root would
/// leave a developer's Mac unable to sleep — the setting is durable — so before
/// this existed the arm/renew/release/expire machine had no test at all, and the
/// two worst defects in this module's review were both in exactly that machine.
pub trait SleepSetter: Send + Sync + 'static {
    /// Write `disablesleep`.
    fn set(&self, disabled: bool) -> impl Future<Output = Result<()>> + Send;
    /// Read the live `disablesleep` value.
    fn read(&self) -> impl Future<Output = Result<bool>> + Send;
}

/// The real thing: `/usr/bin/pmset`.
pub struct Pmset;

impl SleepSetter for Pmset {
    async fn set(&self, disabled: bool) -> Result<()> {
        let value = if disabled { "1" } else { "0" };
        let output = tokio::process::Command::new(PMSET)
            .args(["disablesleep", value])
            .output()
            .await
            .with_context(|| format!("failed to run {PMSET}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("{PMSET} disablesleep {value} failed: {}", stderr.trim());
        }
        Ok(())
    }

    async fn read(&self) -> Result<bool> {
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
}

/// `pmset -g` prints one `SleepDisabled <0|1>` line among many settings.
///
/// Matching the key exactly rather than with a `contains`, so a future
/// `SleepDisabledUntilCharge` cannot be read as this one.
fn parse_sleep_disabled(text: &str) -> bool {
    text.lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let key = parts.next()?;
            (key == "SleepDisabled").then(|| parts.next().unwrap_or("0") == "1")
        })
        .unwrap_or(false)
}

/// Where the ownership marker lives. Beside the helper's other durable state.
fn default_marker_path() -> PathBuf {
    veld_core::paths::lib_dir().join("sleep-lease.json")
}

/// Root's half of the keep-awake switch: `pmset disablesleep`, on a lease, and
/// only ever over a value veld itself took.
pub struct SleepManager<S: SleepSetter = Pmset> {
    /// Wall-clock deadline of the current lease, or `None` when nothing is held.
    ///
    /// A plain `std::sync::Mutex` and never held across an `await` — every lock
    /// below reads or writes the deadline and drops before touching `pmset`.
    lease: Mutex<Option<SystemTime>>,
    marker_path: PathBuf,
    setter: S,
}

impl Default for SleepManager<Pmset> {
    fn default() -> Self {
        Self::new()
    }
}

impl SleepManager<Pmset> {
    pub fn new() -> Self {
        Self::with_parts(default_marker_path(), Pmset)
    }
}

impl<S: SleepSetter> SleepManager<S> {
    fn with_parts(marker_path: PathBuf, setter: S) -> Self {
        Self {
            lease: Mutex::new(None),
            marker_path,
            setter,
        }
    }

    /// Whether a lease is currently armed (for `status`).
    pub fn held(&self) -> bool {
        self.lease.lock().map(|l| l.is_some()).unwrap_or(false)
    }

    fn set_lease(&self, deadline: Option<SystemTime>) {
        *self.lease.lock().expect("sleep lease mutex poisoned") = deadline;
    }

    fn read_marker(&self) -> Option<Marker> {
        let raw = std::fs::read_to_string(&self.marker_path).ok()?;
        match serde_json::from_str(&raw) {
            Ok(m) => Some(m),
            Err(e) => {
                // Unreadable is *not* "absent": something wrote this file and it
                // most likely was us. Say so loudly — the consequence of guessing
                // "absent" is a machine left pinned awake with no record.
                warn!(error = %e, path = %self.marker_path.display(),
                      "the keep-awake ownership marker is unreadable; \
                       assuming veld owns the sleep setting");
                Some(Marker {
                    prior_disabled: false,
                })
            }
        }
    }

    fn write_marker(&self, marker: Marker) -> Result<()> {
        if let Some(parent) = self.marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = serde_json::to_vec(&marker).expect("marker serialisation cannot fail");
        std::fs::write(&self.marker_path, body)
            .with_context(|| format!("failed to write {}", self.marker_path.display()))
    }

    fn delete_marker(&self) {
        if let Err(e) = std::fs::remove_file(&self.marker_path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(error = %e, path = %self.marker_path.display(),
                      "could not remove the keep-awake ownership marker");
            }
        }
    }

    /// Disable sleep and arm (or renew) the lease.
    ///
    /// `pmset` is re-run on every renewal rather than only on the first call. It
    /// is one cheap exec per renewal interval, and it makes the hold
    /// self-healing: anything else that resets the setting while veld holds it is
    /// undone at the next renewal instead of silently ending the guarantee.
    pub async fn hold(&self, lease_secs: u64) -> Result<()> {
        if !cfg!(target_os = "macos") {
            // Not a failure the caller should paper over — on Linux the
            // unprivileged `handle-lid-switch` inhibitor already covers battery,
            // so a caller reaching here has the wrong idea about the platform.
            bail!("battery lid-closed sleep is a macOS-only concern");
        }
        let secs = clamp_lease(lease_secs);
        let first = !self.held();
        if first {
            // Refuse rather than guess. Without knowing the prior value there is
            // no safe answer at release time: assume `false` and veld may switch
            // off somebody else's setting, assume `true` and it strands its own.
            // The caller treats this half as a bonus and degrades cleanly.
            let prior =
                self.setter.read().await.context(
                    "refusing to take the sleep setting without knowing its current value",
                )?;
            self.write_marker(Marker {
                prior_disabled: prior,
            })?;
        }
        self.setter.set(true).await?;
        self.set_lease(Some(SystemTime::now() + Duration::from_secs(secs)));
        if first {
            info!(lease_secs = secs, "sleep disabled (lease armed)");
        }
        Ok(())
    }

    /// Hand the setting back and drop the lease. Idempotent.
    ///
    /// On a failed revert the lease is **re-armed** on a short deadline instead of
    /// cleared, so [`Self::watchdog_tick`] tries again. Clearing it would leave
    /// `disablesleep` set with nothing tracking it.
    pub async fn release(&self) -> Result<()> {
        let Some(marker) = self.read_marker() else {
            // veld never took this setting, so veld does not give it back. This
            // is the branch that stops an exit or a restart from switching off a
            // keep-awake the machine's owner set for themselves.
            self.set_lease(None);
            return Ok(());
        };
        if !cfg!(target_os = "macos") {
            self.set_lease(None);
            return Ok(());
        }
        if marker.prior_disabled {
            // Somebody else already had it on when veld arrived. Drop the claim,
            // leave the value.
            self.delete_marker();
            self.set_lease(None);
            info!("dropped the keep-awake claim; sleep was already disabled before veld");
            return Ok(());
        }
        if let Err(e) = self.setter.set(false).await {
            self.set_lease(Some(SystemTime::now() + REVERT_RETRY));
            return Err(e.context("could not re-enable sleep; keeping the lease armed to retry"));
        }
        self.delete_marker();
        self.set_lease(None);
        info!("sleep re-enabled (lease released)");
        Ok(())
    }

    /// One watchdog iteration: hand the setting back once the lease has expired.
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
        warn!("keep-awake lease expired without renewal — re-enabling sleep");
        if let Err(e) = self.release().await {
            // `release` re-armed the lease, so this repeats next tick.
            warn!(error = %format!("{e:#}"), "could not re-enable sleep after lease expiry");
        }
    }

    /// Startup reconcile: hand back a setting a previous run left marked.
    ///
    /// **Not** a blanket clear. With no marker this does nothing at all and reads
    /// nothing — the machine's sleep setting is none of veld's business unless
    /// veld took it. With a marker, the helper died mid-lease and there is no
    /// live daemon claim to honour, so the value goes back before anything else
    /// happens.
    pub async fn clear_on_startup(&self) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let Some(marker) = self.read_marker() else {
            return;
        };
        if marker.prior_disabled {
            self.delete_marker();
            info!("dropped a stale keep-awake claim; sleep was already disabled before veld");
            return;
        }
        warn!("sleep was left disabled by a previous run — re-enabling it");
        match self.setter.set(false).await {
            Ok(()) => self.delete_marker(),
            // Marker deliberately kept: it is the only record that veld owes this
            // setting back, and the next start must try again.
            Err(e) => {
                warn!(error = %format!("{e:#}"), "could not clear a stale sleep disable")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    use anyhow::{Result, bail};

    use super::{
        MAX_LEASE_SECS, Marker, SleepManager, SleepSetter, clamp_lease, is_expired,
        parse_sleep_disabled,
    };

    /// Records what would have been written, and never touches the machine.
    #[derive(Clone, Default)]
    struct Fake {
        /// The value `pmset` would report.
        value: Arc<Mutex<bool>>,
        /// Every `set` in order.
        writes: Arc<Mutex<Vec<bool>>>,
        /// When true, `set` fails — the failed-revert path.
        fail_set: Arc<Mutex<bool>>,
        /// When true, `read` fails — the unknown-prior-value path.
        fail_read: Arc<Mutex<bool>>,
    }

    impl Fake {
        fn writes(&self) -> Vec<bool> {
            self.writes.lock().unwrap().clone()
        }
    }

    impl SleepSetter for Fake {
        async fn set(&self, disabled: bool) -> Result<()> {
            if *self.fail_set.lock().unwrap() {
                bail!("pmset refused");
            }
            self.writes.lock().unwrap().push(disabled);
            *self.value.lock().unwrap() = disabled;
            Ok(())
        }
        async fn read(&self) -> Result<bool> {
            if *self.fail_read.lock().unwrap() {
                bail!("pmset -g refused");
            }
            Ok(*self.value.lock().unwrap())
        }
    }

    fn manager() -> (SleepManager<Fake>, Fake, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let mgr = SleepManager::with_parts(dir.path().join("sleep-lease.json"), fake.clone());
        (mgr, fake, dir)
    }

    #[test]
    fn an_over_long_lease_is_clamped_rather_than_honoured() {
        assert_eq!(clamp_lease(u64::MAX), MAX_LEASE_SECS);
        assert_eq!(clamp_lease(MAX_LEASE_SECS + 1), MAX_LEASE_SECS);
        // Under the ceiling passes through untouched — a clamp that rounded every
        // lease up would quietly extend the window a dead daemon keeps the
        // machine awake for.
        assert_eq!(clamp_lease(90), 90);
    }

    #[test]
    fn a_lease_expires_at_its_deadline_and_no_lease_never_expires() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(is_expired(Some(now - Duration::from_secs(1)), now));
        // Exactly at the deadline counts as expired: the alternative leaves a
        // lease that only lapses on the *next* tick, silently adding the watchdog
        // interval to every ceiling in this module.
        assert!(is_expired(Some(now), now));
        assert!(!is_expired(Some(now + Duration::from_secs(1)), now));
        assert!(!is_expired(None, now));
    }

    #[test]
    fn the_sleep_disabled_line_is_read_out_of_a_full_pmset_dump() {
        let dump = " standby              1\n SleepDisabled        1\n hibernatemode 3\n";
        assert!(parse_sleep_disabled(dump));
        assert!(!parse_sleep_disabled(
            &dump.replace("SleepDisabled        1", "SleepDisabled 0")
        ));
        // Absent means not disabled — a `pmset` that stops printing the key must
        // not be read as "the machine is pinned awake".
        assert!(!parse_sleep_disabled("standby 1\n"));
        // A key that merely starts the same is a different setting.
        assert!(!parse_sleep_disabled(" SleepDisabledUntilCharge 1\n"));
    }

    // -- the lease state machine, none of which executes `pmset` ---------------

    #[tokio::test]
    async fn arming_records_what_it_took_over_from_and_renewing_re_asserts_it() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        assert!(mgr.held());
        assert_eq!(fake.writes(), vec![true]);

        // A renewal re-asserts the value — that is what makes the hold
        // self-healing against anything else resetting it — but must not rewrite
        // the marker, or a renewal would record `prior_disabled: true` (the value
        // veld itself just set) and veld would then never give the setting back.
        mgr.hold(90).await.unwrap();
        assert_eq!(fake.writes(), vec![true, true]);
        let raw = std::fs::read_to_string(&mgr.marker_path).unwrap();
        let marker: Marker = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            marker,
            Marker {
                prior_disabled: false
            }
        );
    }

    #[tokio::test]
    async fn releasing_hands_the_setting_back_and_drops_the_marker() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        mgr.release().await.unwrap();
        assert_eq!(fake.writes(), vec![true, false]);
        assert!(!mgr.held());
        assert!(!mgr.marker_path.exists());
    }

    /// The finding that three review angles hit independently: veld must never
    /// revert a setting it did not set.
    #[tokio::test]
    async fn a_setting_veld_never_took_is_never_written() {
        let (mgr, fake, _dir) = manager();
        // Somebody else pinned the machine awake.
        *fake.value.lock().unwrap() = true;

        // Every path that used to clear unconditionally.
        mgr.release().await.unwrap();
        mgr.clear_on_startup().await;
        mgr.watchdog_tick().await;

        assert!(
            fake.writes().is_empty(),
            "veld wrote a setting it does not own"
        );
        assert!(
            *fake.value.lock().unwrap(),
            "somebody else's keep-awake was cleared"
        );
    }

    /// And the same when veld arrives while it is already on: veld may hold its
    /// own lease over it, but must hand it back untouched.
    #[tokio::test]
    async fn a_lease_taken_over_an_existing_disable_leaves_that_value_alone() {
        let (mgr, fake, _dir) = manager();
        *fake.value.lock().unwrap() = true;
        mgr.hold(90).await.unwrap();
        mgr.release().await.unwrap();
        // `hold` re-asserted `true`, which is a no-op against a value already
        // `true`; what matters is that the release never wrote `false`.
        assert!(!fake.writes().contains(&false));
        assert!(*fake.value.lock().unwrap());
        assert!(!mgr.marker_path.exists());
    }

    /// A revert that fails must keep the lease armed. Clearing it would strand
    /// `disablesleep` with `is_expired(None) == false` — nothing would ever retry.
    #[tokio::test]
    async fn a_failed_revert_keeps_the_lease_so_the_watchdog_retries() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        *fake.fail_set.lock().unwrap() = true;

        assert!(mgr.release().await.is_err());
        assert!(mgr.held(), "a failed revert dropped the lease");
        assert!(
            mgr.marker_path.exists(),
            "a failed revert dropped the ownership record"
        );

        // The retry deadline is short, so the next tick picks it up.
        *fake.fail_set.lock().unwrap() = false;
        mgr.set_lease(Some(SystemTime::now() - Duration::from_secs(1)));
        mgr.watchdog_tick().await;
        assert!(!mgr.held());
        assert_eq!(fake.writes(), vec![true, false]);
    }

    #[tokio::test]
    async fn an_expired_lease_is_reverted_by_the_watchdog() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        mgr.set_lease(Some(SystemTime::now() - Duration::from_secs(1)));
        mgr.watchdog_tick().await;
        assert!(!mgr.held());
        assert_eq!(fake.writes(), vec![true, false]);
    }

    /// A helper that died mid-lease left a marker; the next start hands it back.
    #[tokio::test]
    async fn a_marker_left_by_a_dead_helper_is_honoured_on_startup() {
        let (mgr, fake, dir) = manager();
        mgr.hold(90).await.unwrap();

        // A fresh manager over the same marker file is what a restart looks like.
        let restarted = SleepManager::with_parts(dir.path().join("sleep-lease.json"), fake.clone());
        assert!(
            !restarted.held(),
            "a restart must not inherit the in-memory lease"
        );
        restarted.clear_on_startup().await;
        assert_eq!(fake.writes(), vec![true, false]);
        assert!(!restarted.marker_path.exists());
    }

    /// Without a readable prior value there is no safe release, so arming is
    /// refused outright rather than guessing one.
    #[tokio::test]
    async fn arming_is_refused_when_the_prior_value_cannot_be_read() {
        let (mgr, fake, _dir) = manager();
        *fake.fail_read.lock().unwrap() = true;
        assert!(mgr.hold(90).await.is_err());
        assert!(!mgr.held());
        assert!(fake.writes().is_empty());
        assert!(!mgr.marker_path.exists());
    }
}
