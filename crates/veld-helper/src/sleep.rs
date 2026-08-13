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
//! recording that veld took the setting and what it read beforehand
//! ([`Marker`]); nothing here touches `pmset` unless that marker says veld owns
//! the value.
//!
//! Ownership is not frozen at the first take. If the marker says somebody else
//! owned the value and a later renewal finds it *off*, they let it go and veld
//! takes ownership — rewriting the marker — because the alternative is veld
//! re-asserting a value it has recorded a rule against ever reverting, which
//! ends with veld as the sole author of a permanent pin nobody owns. A live
//! `false` can never be veld's own hold (veld only ever writes `true`), which is
//! what makes that safe.
//!
//! **The marker, not the in-memory lease, is the ownership oracle.** That
//! distinction is the whole correctness argument and it is easy to get backwards:
//! deciding "am I taking this for the first time?" from the lease means a helper
//! that restarted, or whose startup revert failed, sees no lease, reads the live
//! value — `true`, *because it is veld's own hold* — and rewrites the marker to
//! say somebody else owned it all along. From that moment every path takes the
//! "leave it alone" branch and the machine is durably pinned awake by a record
//! asserting veld must not touch it. Exactly the outcome this module exists to
//! prevent, reached through the code meant to prevent it.
//!
//! **2. What veld set, veld gives back — on a lease.** [`SleepManager::hold`]
//! arms a deadline the daemon must keep renewing.
//! [`SleepManager::watchdog_tick`] reverts once it passes, so a daemon that is
//! killed, wedged, updated or uninstalled loses the setting by itself. A revert
//! that *fails* re-arms a short deadline so the watchdog retries; clearing the
//! lease first would strand the setting with nothing left to notice.
//!
//! [`SleepManager::reconcile_on_startup`] handles the helper that died *without*
//! running its exit path (SIGKILL, panic, power loss). It does not revert on the
//! spot — it **adopts** the marker onto a short grace lease and lets the ordinary
//! watchdog decide. A daemon still holding the session renews inside the grace and
//! nothing visibly happened; a daemon that is gone lets it lapse and the watchdog
//! hands the setting back through the single revert path. One revert path, not
//! two, and no boot-time `pmset` failure that nothing retries.
//!
//! That grace is **bounded across restarts, not granted per restart** — the stamp
//! lives in the marker. The helper runs under launchd `KeepAlive` with a ~10s
//! relaunch, so a crash loop would otherwise mint a fresh window several times a
//! minute and hold the setting forever with no daemon anywhere, which is strictly
//! worse than the revert-on-startup adoption replaced.
//!
//! **Every mutating path takes one lock for its whole duration.** `main.rs` spawns
//! a task per accepted connection, so two commands genuinely race — and the
//! damaging interleaving is cheap to reach: changing a session's duration issues a
//! release and a hold back to back, and a `hold` already on the wire cannot be
//! recalled by the daemon aborting its renewal task. Read-modify-write of
//! (marker, `pmset`, lease) must be atomic or the marker can be rewritten from a
//! value veld itself just set.
//!
//! Deadlines are held as **both** wall clock and monotonic, and expire on
//! whichever comes first. `Instant` alone does not advance across a macOS suspend,
//! so a suspended machine would resume believing no time had passed; `SystemTime`
//! alone moves with an NTP correction or a hand-set clock, and a backwards step
//! would push the only thing that reverts an unrenewed lease into the future.
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
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Mutex;

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
///
/// Defined in `veld-core` so the daemon that asks and the helper that grants read
/// one number; the helper clamps silently, so a drift here is invisible.
pub const MAX_LEASE_SECS: u64 = veld_core::helper::MAX_SLEEP_LEASE_SECS;

/// How soon the watchdog retries after a revert that failed.
///
/// Short, because until it succeeds the machine is held awake by a lease nobody
/// is renewing — the state this module refuses to leave behind.
const REVERT_RETRY: Duration = Duration::from_secs(15);

/// How long an adopted marker is held before the watchdog hands it back.
///
/// Long enough that a daemon still holding the session renews inside it — the
/// daemon renews every 30s — and short enough that a machine whose daemon is gone
/// is not pinned awake for long after a helper crash. Deliberately the same order
/// as the lease the daemon asks for, since it is standing in for one.
const ADOPTION_GRACE: Duration = Duration::from_secs(veld_core::helper::SLEEP_ADOPTION_GRACE_SECS);

/// Clamp a requested lease to [`MAX_LEASE_SECS`].
///
/// Clamped rather than refused: a caller asking for too much still *wants* the
/// machine awake, and refusing would leave it with no hold at all — the wrong
/// direction to fail for a request that is only over-eager.
fn clamp_lease(secs: u64) -> u64 {
    secs.min(MAX_LEASE_SECS)
}

/// A lease deadline, kept on both clocks. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Deadline {
    wall: SystemTime,
    mono: Instant,
}

impl Deadline {
    fn in_secs(secs: u64) -> Self {
        let d = Duration::from_secs(secs);
        Self {
            wall: SystemTime::now() + d,
            mono: Instant::now() + d,
        }
    }

    fn after(d: Duration) -> Self {
        Self {
            wall: SystemTime::now() + d,
            mono: Instant::now() + d,
        }
    }
}

/// Whether a lease has lapsed and the setting must go back.
///
/// Expires on whichever clock says so **first**, which is the safe direction for
/// both failure modes: a suspend freezes the monotonic clock, and a backwards
/// wall-clock step pushes the wall deadline out.
///
/// `None` is **not** expired: there is nothing to revert, and treating it as
/// expired would have every watchdog tick on an idle machine try to hand back a
/// setting that was never taken.
fn is_expired(lease: Option<Deadline>, wall_now: SystemTime, mono_now: Instant) -> bool {
    lease.is_some_and(|d| wall_now >= d.wall || mono_now >= d.mono)
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
    /// Unix seconds at which a helper first *adopted* this marker without a
    /// daemon renewing it — see [`SleepManager::reconcile_on_startup`].
    ///
    /// **The adoption grace is bounded across restarts, not per restart.** The
    /// helper runs under launchd `KeepAlive` with the default ~10s relaunch, so a
    /// crash-looping root helper restarts several times inside one grace window;
    /// minting a fresh grace on each start would mean the lease never lapses and
    /// the watchdog never reverts — a machine pinned indefinitely with no daemon,
    /// which is strictly worse than the revert-on-startup it replaced. Stamped on
    /// the first adoption, inherited by every later one, and cleared the moment a
    /// daemon renews.
    ///
    /// `serde(default)` so a marker written before this field existed still
    /// parses; `None` simply means "not adopted yet".
    #[serde(default)]
    adopted_at: Option<u64>,
}

/// Wall-clock seconds since the epoch, for [`Marker::adopted_at`].
///
/// Saturating rather than panicking on a clock before 1970: a nonsense clock must
/// not take out the helper, and `0` makes an adoption look ancient, which expires
/// it — the safe direction.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

/// Root's half of the keep-awake switch: `pmset disablesleep`, on a lease, and
/// only ever over a value veld itself took.
pub struct SleepManager<S: SleepSetter = Pmset> {
    /// Guards the **whole** take/give-back critical section — marker read, the
    /// `pmset` write, and the lease update — not merely the deadline field. A
    /// task per connection means these genuinely race; see the module docs.
    lease: Mutex<Option<Deadline>>,
    marker_path: PathBuf,
    setter: S,
}

/// Where the ownership marker lives: a **root-only** directory.
///
/// Not beside the helper's Caddy state, and not anywhere under `lib_dir()`. That
/// tree is user-owned (`~/.local/lib/veld` is `drwxr-xr-x <user>`), so a local
/// process could rename the directory and substitute a forged marker —
/// `{"prior_disabled":true}` makes root refuse to ever revert, which is a
/// permanent pin that survives a reboot, and deleting it has the same effect by
/// the other route. The non-adversarial version is just as real: `rm -rf
/// ~/.local/lib/veld` during a botched reinstall, or re-running `setup
/// privileged` with a different Caddy location, orphans the marker.
///
/// The path itself comes from `veld-core` because `veld uninstall` has to sweep
/// the same directory, and two literals would drift.
pub fn marker_dir() -> PathBuf {
    veld_core::paths::sleep_marker_dir()
}

impl SleepManager<Pmset> {
    pub fn new() -> Self {
        Self::with_parts(marker_dir().join("sleep-lease.json"), Pmset)
    }
}

impl Default for SleepManager<Pmset> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: SleepSetter> SleepManager<S> {
    pub(crate) fn with_parts(marker_path: PathBuf, setter: S) -> Self {
        Self {
            lease: Mutex::new(None),
            marker_path,
            setter,
        }
    }

    /// Whether veld is holding this machine's sleep setting, for `status`.
    ///
    /// Marker **or** live lease, not just the lease. The marker-without-lease
    /// state is precisely the persistent one — a helper that crashed, or a revert
    /// that failed — and it is the one somebody diagnoses hours later from a
    /// support transcript with the IDE closed. Reporting only the in-memory lease
    /// would hide the single case that survives long enough to be asked about.
    pub async fn held(&self) -> bool {
        self.lease.lock().await.is_some() || self.read_marker().is_some()
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
                    adopted_at: None,
                })
            }
        }
    }

    /// Write the marker **atomically**: temp file, fsync, rename.
    ///
    /// The thing it guards is durable — `pmset disablesleep 1` survives the power
    /// going out — so a marker lost in the writeback window leaves the setting on
    /// with no record that veld owes it back, the module's stated worst case. A
    /// *truncated* marker is survivable (`read_marker` treats an unparseable file
    /// as veld's, the safe direction); total loss is not.
    fn write_marker(&self, marker: Marker) -> Result<()> {
        if let Some(parent) = self.marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
            // Owner-only, so the authority for a root write is not readable or
            // replaceable by anything else on the machine. Best-effort: a
            // pre-existing directory keeps its mode, and the root-only *parent*
            // is what the guarantee actually rests on.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        let tmp = self.marker_path.with_extension("json.tmp");
        let body = serde_json::to_vec(&marker).expect("marker serialisation cannot fail");
        {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("failed to create {}", tmp.display()))?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.marker_path)
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
        // Held across everything below: marker read, `pmset`, lease write.
        let mut lease = self.lease.lock().await;

        // **The marker decides, not the lease.** A restarted helper, or one whose
        // startup revert failed, holds the marker with no lease — and reading the
        // live value there returns veld's own `true`, which written back as
        // `prior_disabled` would disown the setting permanently. See the module
        // docs.
        let existing = self.read_marker();
        if existing.is_none() {
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
                adopted_at: None,
            })?;
            // The marker is written first (see [`Marker`]) — so if the write that
            // follows fails, take it back. Otherwise a helper that cannot set the
            // value at all (an unprivileged one: `pmset -g` reads fine as the
            // user, `pmset disablesleep` does not) leaves a marker claiming veld
            // holds a setting it never touched, which `status` and `veld doctor`
            // would then report as a live hold.
            if let Err(e) = self.setter.set(true).await {
                self.delete_marker();
                return Err(e);
            }
            *lease = Some(Deadline::in_secs(secs));
            info!(lease_secs = secs, "sleep disabled (lease armed)");
            return Ok(());
        }

        let mut marker = existing.expect("checked just above");
        if marker.prior_disabled {
            // Somebody else owned this setting when veld arrived, so veld has
            // recorded that it will never revert it. **Then veld must not write
            // it either** — re-asserting `true` unconditionally would undo their
            // switch-off, and since release leaves this marker's value alone the
            // machine would end up durably pinned with nobody owning it and
            // nothing to explain why.
            //
            // So ask who owns it now. A live `false` means they let it go, and
            // veld may take ownership: from here on it is veld's to re-assert and
            // veld's to give back. That cannot recreate the disown bug the marker
            // exists to prevent, because a live `false` can never be veld's own
            // hold — veld only ever writes `true`.
            if self.setter.read().await? {
                *lease = Some(Deadline::in_secs(secs));
                return Ok(());
            }
            marker.prior_disabled = false;
            self.write_marker(marker)?;
            info!("the previous owner released the sleep setting; veld owns it now");
        }

        // A daemon is renewing again, so this is no longer an unattended
        // adoption. Written only on the transition, not on every renewal.
        if marker.adopted_at.is_some() {
            self.write_marker(Marker {
                adopted_at: None,
                ..marker
            })?;
            info!("a daemon resumed renewing an adopted keep-awake");
        }
        self.setter.set(true).await?;
        *lease = Some(Deadline::in_secs(secs));
        Ok(())
    }

    /// Hand the setting back and drop the lease. Idempotent.
    ///
    /// On a failed revert the lease is **re-armed** on a short deadline instead of
    /// cleared, so [`Self::watchdog_tick`] tries again. Clearing it would leave
    /// `disablesleep` set with nothing tracking it.
    pub async fn release(&self) -> Result<()> {
        let mut lease = self.lease.lock().await;
        self.release_locked(&mut lease).await
    }

    /// The body of [`Self::release`], for callers already holding the lock.
    async fn release_locked(&self, lease: &mut Option<Deadline>) -> Result<()> {
        let Some(marker) = self.read_marker() else {
            // veld never took this setting, so veld does not give it back. This
            // is the branch that stops an exit or a restart from switching off a
            // keep-awake the machine's owner set for themselves.
            *lease = None;
            return Ok(());
        };
        // No non-macOS guard below: `hold` refuses outright there, so a marker
        // cannot exist for this to have taken, and the branch above already
        // returned. A guard here would be a branch that can never fire.
        if marker.prior_disabled {
            // Somebody else already had it on when veld arrived. Drop the claim,
            // leave the value.
            self.delete_marker();
            *lease = None;
            info!("dropped the keep-awake claim; sleep was already disabled before veld");
            return Ok(());
        }
        if let Err(e) = self.setter.set(false).await {
            *lease = Some(Deadline::after(REVERT_RETRY));
            return Err(e.context("could not re-enable sleep; keeping the lease armed to retry"));
        }
        self.delete_marker();
        *lease = None;
        info!("sleep re-enabled (lease released)");
        Ok(())
    }

    /// One watchdog iteration: hand the setting back once the lease has expired.
    ///
    /// This is the path that runs when the daemon dies, hangs, or is replaced —
    /// nothing tells the helper that happened, so an elapsed deadline is the
    /// signal. It is also the *only* place a revert is initiated on a timer, so
    /// startup adoption and a failed revert both converge here.
    pub async fn watchdog_tick(&self) {
        let mut lease = self.lease.lock().await;
        if !is_expired(*lease, SystemTime::now(), Instant::now()) {
            return;
        }
        warn!("keep-awake lease expired without renewal — re-enabling sleep");
        if let Err(e) = self.release_locked(&mut lease).await {
            // `release_locked` re-armed the lease, so this repeats next tick.
            warn!(error = %format!("{e:#}"), "could not re-enable sleep after lease expiry");
        }
    }

    /// Startup reconcile: adopt a marker left by a helper that died without
    /// running its exit path.
    ///
    /// **Adopts rather than reverts.** Arming a short grace lease and letting the
    /// ordinary watchdog decide is better than reverting here on all three counts
    /// that matter: a daemon still holding the session renews inside the grace, so
    /// a helper crash costs the user nothing visible; a daemon that is gone lets it
    /// lapse and the setting goes back through the single revert path; and a
    /// `pmset` that fails at boot is retried by the watchdog instead of being
    /// logged once and abandoned with the machine pinned awake.
    ///
    /// With no marker this does nothing and reads nothing — the machine's sleep
    /// setting is none of veld's business unless veld took it.
    pub async fn reconcile_on_startup(&self) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let mut lease = self.lease.lock().await;
        let Some(marker) = self.read_marker() else {
            return;
        };
        if marker.prior_disabled {
            self.delete_marker();
            info!("dropped a stale keep-awake claim; sleep was already disabled before veld");
            return;
        }

        // **One grace across every restart, not one per restart.** Stamped on the
        // first adoption and inherited afterwards, so a helper crash-looping under
        // launchd's ~10s relaunch cannot keep minting fresh windows and pin the
        // machine indefinitely with no daemon anywhere — which would be strictly
        // worse than the revert-on-startup this replaced.
        let now = unix_now();
        let adopted_at = match marker.adopted_at {
            Some(stamp) => stamp,
            None => {
                if let Err(e) = self.write_marker(Marker {
                    adopted_at: Some(now),
                    ..marker
                }) {
                    // The stamp is what bounds this; without it the grace would
                    // renew forever. Expire immediately rather than adopt blind.
                    warn!(error = %format!("{e:#}"), "could not stamp the keep-awake adoption");
                    *lease = Some(Deadline::after(Duration::ZERO));
                    return;
                }
                now
            }
        };

        // A wall clock reading *behind* the stamp is not "no time has passed" —
        // `saturating_sub` would say zero elapsed and hand out a fresh full window
        // on every restart for as long as the skew lasts, which is the very
        // defect the stamp exists to prevent, reached by NTP or a VM resume
        // instead of by a crash loop. Treat it as exhausted: the machine has
        // already been held without a daemon for an unknown length of time, and
        // the safe reading of "unknown" is "long enough".
        let remaining = match now.checked_sub(adopted_at) {
            Some(elapsed) => ADOPTION_GRACE.saturating_sub(Duration::from_secs(elapsed)),
            None => {
                warn!(
                    "the clock moved behind the keep-awake adoption stamp; \
                     treating the grace as spent"
                );
                Duration::ZERO
            }
        };
        *lease = Some(Deadline::after(remaining));
        warn!(
            grace_secs = remaining.as_secs(),
            "adopted a keep-awake left by a previous run; \
             re-enabling sleep unless the daemon renews it"
        );
    }
}

#[cfg(test)]
impl<S: SleepSetter> SleepManager<S> {
    /// Force the lease deadline, so a test can reach the expiry path without
    /// sleeping for it.
    async fn force_deadline(&self, deadline: Option<Deadline>) {
        *self.lease.lock().await = deadline;
    }

    async fn lease_is_armed(&self) -> bool {
        self.lease.lock().await.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    use anyhow::{Result, bail};

    use super::{
        Deadline, MAX_LEASE_SECS, Marker, SleepManager, SleepSetter, clamp_lease, is_expired,
        parse_sleep_disabled,
    };

    /// Records what would have been written, and never touches the machine.
    #[derive(Clone, Default)]
    struct Fake {
        value: Arc<Mutex<bool>>,
        writes: Arc<Mutex<Vec<bool>>>,
        fail_set: Arc<Mutex<bool>>,
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

    fn past() -> Option<Deadline> {
        Some(Deadline {
            wall: SystemTime::now() - Duration::from_secs(1),
            mono: Instant::now() - Duration::from_secs(1),
        })
    }

    #[test]
    fn an_over_long_lease_is_clamped_rather_than_honoured() {
        assert_eq!(clamp_lease(u64::MAX), MAX_LEASE_SECS);
        assert_eq!(clamp_lease(MAX_LEASE_SECS + 1), MAX_LEASE_SECS);
        assert_eq!(clamp_lease(90), 90);
    }

    // No test that the daemon's ask fits under this ceiling: `caffeinate.rs`
    // carries `const _: () = assert!(HELPER_LEASE_SECS <= MAX_SLEEP_LEASE_SECS)`,
    // which fails the *build* rather than a test run. A runtime assertion over
    // two constants is strictly weaker and clippy rightly calls it out.

    /// Expiry takes whichever clock fires first, because each covers the other's
    /// blind spot: monotonic stops across a macOS suspend, and wall clock can step
    /// backwards under NTP.
    #[test]
    fn a_lease_expires_on_whichever_clock_lapses_first() {
        let wall = SystemTime::now();
        let mono = Instant::now();
        let ahead = Duration::from_secs(60);

        let live = Deadline {
            wall: wall + ahead,
            mono: mono + ahead,
        };
        assert!(!is_expired(Some(live), wall, mono));

        // Wall clock lapsed, monotonic frozen (the suspend case).
        let wall_gone = Deadline {
            wall: wall - Duration::from_secs(1),
            mono: mono + ahead,
        };
        assert!(is_expired(Some(wall_gone), wall, mono));

        // Monotonic lapsed, wall clock stepped backwards (the NTP case).
        let mono_gone = Deadline {
            wall: wall + Duration::from_secs(86_400),
            mono: mono - Duration::from_secs(1),
        };
        assert!(is_expired(Some(mono_gone), wall, mono));

        assert!(!is_expired(None, wall, mono));
    }

    #[test]
    fn the_sleep_disabled_line_is_read_out_of_a_full_pmset_dump() {
        let dump = " standby              1\n SleepDisabled        1\n hibernatemode 3\n";
        assert!(parse_sleep_disabled(dump));
        assert!(!parse_sleep_disabled(
            &dump.replace("SleepDisabled        1", "SleepDisabled 0")
        ));
        assert!(!parse_sleep_disabled("standby 1\n"));
        assert!(!parse_sleep_disabled(" SleepDisabledUntilCharge 1\n"));
    }

    // -- the lease state machine, none of which executes `pmset` ---------------

    #[tokio::test]
    async fn arming_records_what_it_took_over_from_and_renewing_re_asserts_it() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        assert!(mgr.held().await);
        assert_eq!(fake.writes(), vec![true]);

        mgr.hold(90).await.unwrap();
        assert_eq!(fake.writes(), vec![true, true]);
        let raw = std::fs::read_to_string(&mgr.marker_path).unwrap();
        let marker: Marker = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            marker,
            Marker {
                prior_disabled: false,
                adopted_at: None
            }
        );
    }

    /// **The marker, not the lease, decides whether this is a first take.**
    ///
    /// The regression this pins was introduced by a review fix and is the worst
    /// state the module can reach. A helper that restarted holds the marker with
    /// no lease; deriving "first" from the lease makes the next renewal read the
    /// live value — `true`, because it is veld's own hold — and write it back as
    /// `prior_disabled`. Every path then takes "somebody else owns it", and the
    /// machine is durably pinned awake by a record telling veld to keep its hands
    /// off.
    #[tokio::test]
    async fn a_renewal_after_the_lease_is_lost_does_not_disown_the_setting() {
        let (mgr, fake, dir) = manager();
        mgr.hold(90).await.unwrap();

        // A restarted helper: same marker on disk, no in-memory lease.
        let restarted = SleepManager::with_parts(dir.path().join("sleep-lease.json"), fake.clone());
        assert!(!restarted.lease_is_armed().await);
        restarted.hold(90).await.unwrap();

        let raw = std::fs::read_to_string(&restarted.marker_path).unwrap();
        let marker: Marker = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            marker,
            Marker {
                prior_disabled: false,
                adopted_at: None
            },
            "the renewal rewrote the marker from veld's own value"
        );

        // And it can still be given back.
        restarted.release().await.unwrap();
        assert!(!*fake.value.lock().unwrap());
        assert!(!restarted.marker_path.exists());
    }

    #[tokio::test]
    async fn releasing_hands_the_setting_back_and_drops_the_marker() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        mgr.release().await.unwrap();
        assert_eq!(fake.writes(), vec![true, false]);
        assert!(!mgr.held().await);
        assert!(!mgr.marker_path.exists());
    }

    /// The finding three review angles hit independently: veld must never revert
    /// a setting it did not set.
    #[tokio::test]
    async fn a_setting_veld_never_took_is_never_written() {
        let (mgr, fake, _dir) = manager();
        *fake.value.lock().unwrap() = true;

        mgr.release().await.unwrap();
        mgr.reconcile_on_startup().await;
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

    #[tokio::test]
    async fn a_lease_taken_over_an_existing_disable_leaves_that_value_alone() {
        let (mgr, fake, _dir) = manager();
        *fake.value.lock().unwrap() = true;
        mgr.hold(90).await.unwrap();
        mgr.release().await.unwrap();
        assert!(!fake.writes().contains(&false));
        assert!(*fake.value.lock().unwrap());
        assert!(!mgr.marker_path.exists());
    }

    #[tokio::test]
    async fn a_failed_revert_keeps_the_lease_so_the_watchdog_retries() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        *fake.fail_set.lock().unwrap() = true;

        assert!(mgr.release().await.is_err());
        assert!(
            mgr.lease_is_armed().await,
            "a failed revert dropped the lease"
        );
        assert!(
            mgr.marker_path.exists(),
            "a failed revert dropped the ownership record"
        );

        *fake.fail_set.lock().unwrap() = false;
        mgr.force_deadline(past()).await;
        mgr.watchdog_tick().await;
        assert!(!mgr.held().await);
        assert_eq!(fake.writes(), vec![true, false]);
    }

    #[tokio::test]
    async fn an_expired_lease_is_reverted_by_the_watchdog() {
        let (mgr, fake, _dir) = manager();
        mgr.hold(90).await.unwrap();
        mgr.force_deadline(past()).await;
        mgr.watchdog_tick().await;
        assert!(!mgr.held().await);
        assert_eq!(fake.writes(), vec![true, false]);
    }

    /// **Startup adopts; it does not revert on the spot.**
    ///
    /// The helper's own self-restart path exits from a spawned task and never runs
    /// the release at the tail, so a marker surviving a restart is routine rather
    /// than exceptional. Reverting here would drop the hold of a daemon that is
    /// still perfectly alive, and a `pmset` failure at boot would strand the
    /// setting with no lease for the watchdog to act on.
    #[tokio::test]
    async fn startup_adopts_a_left_over_marker_instead_of_reverting_it() {
        let (mgr, fake, dir) = manager();
        mgr.hold(90).await.unwrap();

        let restarted = SleepManager::with_parts(dir.path().join("sleep-lease.json"), fake.clone());
        restarted.reconcile_on_startup().await;

        // Nothing reverted, and a lease is armed for the watchdog to act on.
        assert_eq!(fake.writes(), vec![true], "startup reverted a live hold");
        assert!(restarted.lease_is_armed().await);

        // A daemon still holding the session renews inside the grace.
        restarted.hold(90).await.unwrap();
        restarted.watchdog_tick().await;
        assert!(
            *fake.value.lock().unwrap(),
            "a renewed hold was reverted anyway"
        );

        // A daemon that is gone lets it lapse, and the single revert path runs.
        restarted.force_deadline(past()).await;
        restarted.watchdog_tick().await;
        assert!(!*fake.value.lock().unwrap());
        assert!(!restarted.marker_path.exists());
    }

    /// `status` must report the persistent case, which is the one somebody
    /// diagnoses hours later with the IDE closed.
    #[tokio::test]
    async fn a_marker_without_a_live_lease_still_reports_as_held() {
        let (mgr, fake, dir) = manager();
        mgr.hold(90).await.unwrap();
        let restarted = SleepManager::with_parts(dir.path().join("sleep-lease.json"), fake.clone());
        assert!(!restarted.lease_is_armed().await);
        assert!(
            restarted.held().await,
            "the one state that survives to be asked about reported as not held"
        );
    }

    /// A helper that can read but not write — the unprivileged one, where
    /// `pmset -g` works as the user and `pmset disablesleep` does not — must not
    /// leave a marker behind claiming a hold it never took. `status` and
    /// `veld doctor` read that marker.
    #[tokio::test]
    async fn a_first_take_that_cannot_write_leaves_no_ownership_claim() {
        let (mgr, fake, _dir) = manager();
        *fake.fail_set.lock().unwrap() = true;
        assert!(mgr.hold(90).await.is_err());
        assert!(
            !mgr.marker_path.exists(),
            "a failed first take left a claim behind"
        );
        assert!(!mgr.held().await);
    }

    /// Somebody else owns the setting, then lets it go while veld holds a lease.
    ///
    /// Veld must take ownership at that point rather than re-asserting a value it
    /// has recorded a rule against reverting — otherwise it becomes the sole
    /// author of a permanent pin nobody owns.
    #[tokio::test]
    async fn veld_takes_ownership_when_the_previous_owner_lets_go() {
        let (mgr, fake, _dir) = manager();
        *fake.value.lock().unwrap() = true;
        mgr.hold(90).await.unwrap();

        // The other owner switches it off.
        *fake.value.lock().unwrap() = false;
        fake.writes.lock().unwrap().clear();

        mgr.hold(90).await.unwrap();
        assert_eq!(
            fake.writes(),
            vec![true],
            "veld did not re-take the setting"
        );

        // And now it is veld's to give back.
        mgr.release().await.unwrap();
        assert!(
            !*fake.value.lock().unwrap(),
            "veld pinned a setting nobody owns"
        );
        assert!(!mgr.marker_path.exists());
    }

    /// While the other owner still holds it, veld must not write at all.
    #[tokio::test]
    async fn veld_never_re_asserts_a_value_it_promised_not_to_revert() {
        let (mgr, fake, _dir) = manager();
        *fake.value.lock().unwrap() = true;
        mgr.hold(90).await.unwrap();
        fake.writes.lock().unwrap().clear();
        mgr.hold(90).await.unwrap();
        assert!(
            fake.writes().is_empty(),
            "veld wrote a value it will never revert"
        );
    }

    /// The adoption grace is bounded across restarts, not minted afresh by each.
    ///
    /// A helper crash-looping under launchd's ~10s relaunch would otherwise never
    /// let the lease lapse, pinning the machine with no daemon anywhere — strictly
    /// worse than the revert-on-startup that adoption replaced.
    #[tokio::test]
    async fn repeated_restarts_share_one_adoption_grace() {
        let (mgr, fake, dir) = manager();
        mgr.hold(90).await.unwrap();
        let path = dir.path().join("sleep-lease.json");

        // First adoption stamps the marker.
        let first = SleepManager::with_parts(path.clone(), fake.clone());
        first.reconcile_on_startup().await;
        let stamped: Marker =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(stamped.adopted_at.is_some(), "the adoption was not stamped");

        // Backdate it past the grace: that is what a run of restarts looks like.
        std::fs::write(
            &path,
            serde_json::to_vec(&Marker {
                prior_disabled: false,
                adopted_at: Some(super::unix_now() - super::ADOPTION_GRACE.as_secs() - 1),
            })
            .unwrap(),
        )
        .unwrap();

        let later = SleepManager::with_parts(path.clone(), fake.clone());
        later.reconcile_on_startup().await;
        // Already lapsed, so the very next tick hands the setting back rather
        // than granting another full window.
        later.watchdog_tick().await;
        assert!(
            !*fake.value.lock().unwrap(),
            "a restart minted a fresh grace"
        );
        assert!(!path.exists());
    }

    /// A clock that reads behind the stamp must not hand out a fresh window.
    ///
    /// `saturating_sub` reports zero elapsed for a backward step, which would
    /// grant the full grace on every restart for as long as the skew lasted —
    /// the same unbounded-adoption defect the stamp exists to prevent, reached
    /// by NTP instead of by a crash loop.
    #[tokio::test]
    async fn a_clock_behind_the_adoption_stamp_spends_the_grace_rather_than_renewing_it() {
        let (mgr, fake, dir) = manager();
        mgr.hold(90).await.unwrap();
        let path = dir.path().join("sleep-lease.json");

        // A stamp from the future is what a backward clock step looks like.
        std::fs::write(
            &path,
            serde_json::to_vec(&Marker {
                prior_disabled: false,
                adopted_at: Some(super::unix_now() + 3_600),
            })
            .unwrap(),
        )
        .unwrap();

        let restarted = SleepManager::with_parts(path.clone(), fake.clone());
        restarted.reconcile_on_startup().await;
        restarted.watchdog_tick().await;
        assert!(
            !*fake.value.lock().unwrap(),
            "a clock behind the stamp minted a fresh grace"
        );
        assert!(!path.exists());
    }

    /// A daemon that resumes renewing clears the adoption stamp, so a *later*
    /// crash gets a full grace again rather than inheriting an ancient one.
    #[tokio::test]
    async fn a_resumed_daemon_clears_the_adoption_stamp() {
        let (mgr, fake, dir) = manager();
        mgr.hold(90).await.unwrap();
        let path = dir.path().join("sleep-lease.json");

        let restarted = SleepManager::with_parts(path.clone(), fake.clone());
        restarted.reconcile_on_startup().await;
        restarted.hold(90).await.unwrap();

        let marker: Marker =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            marker.adopted_at, None,
            "the stamp outlived the daemon's return"
        );
    }

    #[tokio::test]
    async fn arming_is_refused_when_the_prior_value_cannot_be_read() {
        let (mgr, fake, _dir) = manager();
        *fake.fail_read.lock().unwrap() = true;
        assert!(mgr.hold(90).await.is_err());
        assert!(!mgr.held().await);
        assert!(fake.writes().is_empty());
        assert!(!mgr.marker_path.exists());
    }
}
