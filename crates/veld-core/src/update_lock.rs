//! Single-flight for `veld update`, and the progress feed that falls out of it.
//!
//! Two problems, one artifact. An update replaces `veld`, `veld-daemon`,
//! `veld-helper` and `caddy` in place and then restarts both services, so two of
//! them at once is not a slow update — it is two processes swapping the same
//! files and bouncing the same daemons in an interleaving nobody designed. And
//! for the 1–4 minutes one takes, everything else about the machine looks
//! unexplained: a `veld start` fails against a daemon that is mid-restart, and a
//! Veld Desktop launched from the Dock reconnects to nothing.
//!
//! So the lock is not just a flag. Whoever holds it publishes **what it is
//! doing** into the same file, and every other reader — a second `veld update`,
//! `veld doctor`, the command gate in `main.rs`, the Electron app at startup —
//! gets the answer from one `read()` of one small JSON file.
//!
//! ## Why a file, and not the obvious alternatives
//!
//! - **Not the daemon.** It is the arbiter that gets restarted halfway through
//!   the thing it would be arbitrating, and the daemon that comes back is a
//!   different binary version. Any mutex living inside a process the update
//!   restarts is disqualified before its merits are weighed.
//! - **Not the SQLite DB**, even though `update.last_check` already lives there.
//!   The update migrates that database, and a binary refuses a `user_version`
//!   newer than it supports (`DbError::NewerSchema`) — so the lock holder can be
//!   locked out of its own lock by the update it is running. Using the artifact
//!   under migration as the mutex for the migration is a layering inversion.
//! - **Not an advisory `flock`/socket bind.** Kernel-held liveness is tidier and
//!   would need no timeout, but it has no answer for the case this exists to
//!   cover: a run **alive** and blocked forever on a `sudo` password nobody
//!   typed. It is also the one shape the Electron app and a shell script cannot
//!   read cheaply, and there are four readers here, not one.
//!
//! ## Staleness: two independent conditions
//!
//! A held lock is ignored — and stolen — when **either** holds:
//!
//! 1. **The holder is gone.** `kill(pid, 0)` says `ESRCH`. Covers a crash, a
//!    `SIGKILL`, a closed terminal window. Instant, no waiting.
//! 2. **The holder has not moved in [`PHASE_TIMEOUT`].** Covers the alive-but-
//!    stuck run: a `sudo` prompt in a window the user walked away from, a
//!    download against a black-holed connection. The timestamp is refreshed on
//!    every phase change rather than only at start, so a legitimately slow phase
//!    (a 113 MB download on a bad line) never expires while it is progressing.
//!
//! Neither condition alone is enough, which is why both are here: a pid check
//! cannot see a hung process, and a start-time timeout cannot tell a crash from
//! a slow success.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How long a holder may sit in one phase before it is treated as abandoned.
///
/// Generous on purpose, and the number is bounded from below by the slowest
/// *legitimate* phase rather than by how long a user is willing to wait: the app
/// bundle alone is 113 MB and one measured download of it took 71 s at ~1.6 MB/s,
/// so a phone-tethered connection an order of magnitude slower is still well
/// inside this. What it is short enough for is the failure it exists for — a
/// password prompt nobody answered blocks the *next* update for at most this
/// long, and `--force` shortens that to nothing.
pub const PHASE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Where an update is in its run.
///
/// Ordered as they happen, and each one is a thing a user recognises rather than
/// a function name — this string is rendered in `veld doctor`, in a blocked
/// command's error, and in the Electron app's "an update is in progress" dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Holding the lock, nothing done yet.
    Starting,
    /// Waiting for Veld Desktop's pid to exit before touching its bundle.
    WaitingForApp,
    /// Asking GitHub (or trusting `--target-version`) which release to install.
    Checking,
    /// `install.sh` is running: download, checksum, binary swap.
    Installing,
    /// Restarting `veld-helper` and `veld-daemon` onto the new binaries.
    RestartingServices,
    /// Installing the Veld Desktop half.
    UpdatingApp,
    /// Everything is installed; reopening the app / tidying up.
    Finishing,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::WaitingForApp => "waiting-for-app",
            Phase::Checking => "checking",
            Phase::Installing => "installing",
            Phase::RestartingServices => "restarting-services",
            Phase::UpdatingApp => "updating-app",
            Phase::Finishing => "finishing",
        }
    }

    /// One human clause, lowercase, to drop into a sentence.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Starting => "starting up",
            Phase::WaitingForApp => "waiting for Veld Desktop to quit",
            Phase::Checking => "checking which release to install",
            Phase::Installing => "downloading and installing",
            Phase::RestartingServices => "restarting the daemon and helper",
            Phase::UpdatingApp => "installing Veld Desktop",
            Phase::Finishing => "finishing up",
        }
    }
}

/// Who started the update, so a second caller can be told where to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// Typed into a shell.
    Cli,
    /// Handed over by Veld Desktop, running headless.
    Desktop,
    /// Handed over by Veld Desktop into a terminal window it opened.
    Console,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Cli => "cli",
            Origin::Desktop => "desktop",
            Origin::Console => "console",
        }
    }

    /// How to name the source in a sentence aimed at a user.
    pub fn label(self) -> &'static str {
        match self {
            Origin::Cli => "a terminal",
            Origin::Desktop => "Veld Desktop",
            Origin::Console => "Veld Desktop, in a terminal window",
        }
    }
}

/// What the holder published about itself, last time it moved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    /// The holding process. The liveness half of staleness reads this.
    pub pid: u32,
    pub origin: Origin,
    /// The release being installed, once it is known.
    pub version: Option<String>,
    pub started_at: DateTime<Utc>,
    pub phase: Phase,
    /// When [`UpdateState::phase`] was last set — the timeout half of staleness
    /// reads this, **not** `started_at`. See the module docs.
    pub phase_at: DateTime<Utc>,
    /// Where a question would appear, when the holder has a terminal. Rendered
    /// as "answer it in <tty>" by anyone that has to tell a user why nothing is
    /// moving.
    pub tty: Option<String>,
}

impl UpdateState {
    /// Why this lock should be ignored, if it should be.
    pub fn stale_reason(&self, now: DateTime<Utc>) -> Option<StaleReason> {
        if !crate::process::is_alive(self.pid) {
            return Some(StaleReason::HolderGone);
        }
        let idle = now.signed_duration_since(self.phase_at);
        // `to_std` fails on a negative duration, which is a clock that went
        // backwards (NTP step, a state file written on another machine). Not
        // stale — a clock we do not trust is not evidence of abandonment, and the
        // liveness check above is the one that matters in that case.
        match idle.to_std() {
            Ok(idle) if idle > PHASE_TIMEOUT => Some(StaleReason::Stalled),
            _ => None,
        }
    }

    /// How long the holder has been going, floored at zero.
    pub fn age(&self, now: DateTime<Utc>) -> Duration {
        now.signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or_default()
    }

    /// One sentence naming who is updating and what they are doing, for the
    /// several places that have to say exactly that.
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        let version = self
            .version
            .as_deref()
            .map(|v| format!(" to {v}"))
            .unwrap_or_default();
        format!(
            "An update{version} started {} ago from {} (pid {}) — {}.",
            humanise(self.age(now)),
            self.origin.label(),
            self.pid,
            self.phase.label()
        )
    }
}

/// Why a lock on disk does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// The process that took it no longer exists.
    HolderGone,
    /// It exists but has not changed phase in [`PHASE_TIMEOUT`] — the hung-at-a-
    /// password case.
    Stalled,
    /// There is a lock directory but no readable state in it. Treated as stale
    /// rather than as a permanent block: a lock nobody can read is a lock nobody
    /// can wait out.
    Unreadable,
}

impl StaleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StaleReason::HolderGone => "holder-gone",
            StaleReason::Stalled => "stalled",
            StaleReason::Unreadable => "unreadable",
        }
    }
}

/// The outcome of trying to become the one update.
pub enum Acquired {
    /// It is yours. Hold the guard for the whole run.
    Ours(UpdateGuard),
    /// Somebody else's, and theirs is still good.
    Busy(Box<UpdateState>),
}

/// `~/.veld/update.lock` — a **directory**, because `mkdir` is the create-or-fail
/// primitive that exists on every filesystem veld runs on, and it is already the
/// idiom `install.sh` uses for the app-bundle swap.
pub fn lock_dir() -> Option<PathBuf> {
    Some(veld_dir()?.join("update.lock"))
}

fn veld_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("VELD_UPDATE_LOCK_DIR") {
        // Test seam. Deliberately not a documented user knob: two veld processes
        // that disagree about where the lock is are two processes with no lock.
        return Some(PathBuf::from(dir));
    }
    Some(dirs::home_dir()?.join(".veld"))
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

/// Read whoever holds the lock, and whether they still count.
///
/// `None` means there is no lock directory at all. `Some((state, None))` is a
/// live update; `Some((state, Some(reason)))` is one the next `acquire` will
/// steal. Readers that only care about "is an update happening" want
/// [`current`].
///
/// **Never mutates.** A `veld status` that garbage-collects somebody's lock as a
/// side effect of being run is how a race gets introduced by a read.
pub fn peek() -> Option<(UpdateState, Option<StaleReason>)> {
    let dir = lock_dir()?;
    if !dir.exists() {
        return None;
    }
    match read_state(&dir) {
        Some(state) => {
            let reason = state.stale_reason(Utc::now());
            Some((state, reason))
        }
        None => {
            // A lock directory with no readable state. Synthesise enough for a
            // caller to say something true about it; pid 0 can never be alive, so
            // this can only ever read as stale.
            let now = Utc::now();
            Some((
                UpdateState {
                    pid: 0,
                    origin: Origin::Cli,
                    version: None,
                    started_at: now,
                    phase: Phase::Starting,
                    phase_at: now,
                    tty: None,
                },
                Some(StaleReason::Unreadable),
            ))
        }
    }
}

/// The update that is actually running, if one is.
///
/// This is what every gate should use: a stale lock is reported as no update,
/// because a lock nobody is honouring must not lock anybody out.
pub fn current() -> Option<UpdateState> {
    match peek() {
        Some((state, None)) => Some(state),
        _ => None,
    }
}

fn read_state(dir: &Path) -> Option<UpdateState> {
    let raw = fs::read_to_string(state_path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Become the one update, or find out who already is.
///
/// `force` skips straight to stealing whatever is there. It is the escape hatch
/// for the case the timeout is too patient for — a user who knows the other run
/// is dead and does not want to wait [`PHASE_TIMEOUT`] to prove it.
pub fn acquire(origin: Origin, version: Option<String>, force: bool) -> io::Result<Acquired> {
    let Some(dir) = lock_dir() else {
        return Err(io::Error::other(
            "no home directory to hold the update lock",
        ));
    };
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }

    // Bounded rather than unbounded: each pass either wins, loses to a live
    // holder, or steals exactly one corpse. Three is room for a steal that races
    // another stealer and still converges, without a loop that could spin.
    let mut last_seen: Option<UpdateState> = None;
    for _ in 0..3 {
        match fs::create_dir(&dir) {
            Ok(()) => {
                let now = Utc::now();
                let state = UpdateState {
                    pid: std::process::id(),
                    origin,
                    version,
                    started_at: now,
                    phase: Phase::Starting,
                    phase_at: now,
                    tty: current_tty(),
                };
                write_state(&dir, &state)?;
                return Ok(Acquired::Ours(UpdateGuard { dir, state }));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let state = read_state(&dir);
                let stale = match &state {
                    Some(s) => s.stale_reason(Utc::now()).is_some(),
                    // Unreadable: either a corpse from a crash between `mkdir`
                    // and the first write, or a state file we cannot parse.
                    // Either way nobody can wait it out, so it is stealable.
                    None => true,
                };
                if !stale && !force {
                    return Ok(Acquired::Busy(Box::new(
                        state.expect("a lock that is not stale was read successfully"),
                    )));
                }
                last_seen = state;
                steal(&dir)?;
            }
            Err(e) => return Err(e),
        }
    }

    // Three passes and still not ours: somebody is winning the race repeatedly,
    // which is a live contender however the state file reads.
    Ok(Acquired::Busy(Box::new(last_seen.unwrap_or_else(|| {
        let now = Utc::now();
        UpdateState {
            pid: 0,
            origin: Origin::Cli,
            version: None,
            started_at: now,
            phase: Phase::Starting,
            phase_at: now,
            tty: None,
        }
    }))))
}

/// Take a stale lock out of the way, atomically enough that two stealers cannot
/// both then create it.
///
/// The rename is the serialisation point: exactly one racing stealer moves the
/// directory, the other gets `ENOENT` and loops round to `create_dir`, where the
/// kernel picks a single winner. Deleting in place instead would let both stealers
/// "succeed" and both proceed to create — which is the bug the lock exists to
/// prevent, reintroduced inside its own implementation.
fn steal(dir: &Path) -> io::Result<()> {
    let doomed = dir.with_file_name(format!("update.lock.stale.{}", std::process::id()));
    // Anything left at the target from a previous steal by this same pid would
    // fail the rename on some platforms; clear it first.
    let _ = fs::remove_dir_all(&doomed);
    match fs::rename(dir, &doomed) {
        Ok(()) => {
            let _ = fs::remove_dir_all(&doomed);
            Ok(())
        }
        // Somebody else stole it first. Not an error — the next `create_dir` is
        // where this is resolved.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Write the state file whole, by rename, so a reader can never see half of it.
fn write_state(dir: &Path, state: &UpdateState) -> io::Result<()> {
    let path = state_path(dir);
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    fs::write(&tmp, body)?;
    let _ = crate::paths::set_owner_only(&tmp);
    fs::rename(&tmp, &path)
}

/// The controlling terminal's name, when there is one.
///
/// Only used to tell a user *where* to go answer a question. Best-effort by
/// definition: no tty is the normal case for the handed-off route, and is
/// exactly the fact worth reporting there.
fn current_tty() -> Option<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let out = std::process::Command::new("tty").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Ownership of the one-update-at-a-time slot, plus the right to publish
/// progress into it.
///
/// Released on `Drop`, which covers every normal exit including `?`. It does not
/// cover `SIGKILL` or a power cut — which is what the liveness half of staleness
/// is for, and why this deliberately does not also install a signal handler:
/// there would then be two release paths racing to `remove_dir_all` the same
/// directory, and a stale lock is already a solved problem.
pub struct UpdateGuard {
    dir: PathBuf,
    state: UpdateState,
}

impl UpdateGuard {
    /// Publish a new phase, and reset the stall clock.
    ///
    /// Best-effort: a state file that cannot be written must never fail an update
    /// that is otherwise going fine. The cost of a silent failure here is that
    /// the run looks stalled to observers and its lock is stealable early — bad,
    /// but strictly better than aborting a half-installed release.
    pub fn set_phase(&mut self, phase: Phase) {
        self.state.phase = phase;
        self.state.phase_at = Utc::now();
        let _ = write_state(&self.dir, &self.state);
    }

    /// Record the release once it is known, so observers can name it.
    pub fn set_version(&mut self, version: &str) {
        self.state.version = Some(version.to_string());
        let _ = write_state(&self.dir, &self.state);
    }

    /// Give the lock up early.
    ///
    /// The one caller that needs this is the end of `veld update`: it reopens
    /// Veld Desktop, and the app quits itself on startup when an update is in
    /// progress. Reopening while still holding would have the update close the
    /// very window it exists to give back.
    pub fn release(self) {
        drop(self);
    }

    pub fn state(&self) -> &UpdateState {
        &self.state
    }
}

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        // Only if it is still ours. Somebody may have judged us stale and stolen
        // it — in which case the directory now belongs to them and removing it
        // would delete a *live* lock.
        if read_state(&self.dir).map(|s| s.pid) == Some(self.state.pid) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// "3s" / "2m" / "1h 4m", for a duration a human is reading in a sentence.
fn humanise(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    format!("{}h {}m", mins / 60, mins % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// `VELD_UPDATE_LOCK_DIR` is process-wide, so these tests cannot overlap.
    /// Taken **once** per test and bound with `let _guard =` — see the
    /// `env_lock` convention: a second acquisition in the same test deadlocks.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn isolated(dir: &Path) -> MutexGuard<'static, ()> {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("VELD_UPDATE_LOCK_DIR", dir) };
        guard
    }

    fn state_at(pid: u32, phase_at: DateTime<Utc>) -> UpdateState {
        UpdateState {
            pid,
            origin: Origin::Cli,
            version: Some("1.2.3".into()),
            started_at: phase_at,
            phase: Phase::Installing,
            phase_at,
            tty: None,
        }
    }

    #[test]
    fn a_second_acquire_is_refused_while_the_first_is_live() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        let first = acquire(Origin::Cli, Some("1.0.0".into()), false).unwrap();
        assert!(matches!(first, Acquired::Ours(_)));

        match acquire(Origin::Desktop, None, false).unwrap() {
            Acquired::Busy(state) => assert_eq!(state.pid, std::process::id()),
            Acquired::Ours(_) => panic!("two concurrent updates acquired the lock"),
        }
    }

    #[test]
    fn releasing_the_guard_frees_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        match acquire(Origin::Cli, None, false).unwrap() {
            Acquired::Ours(g) => g.release(),
            Acquired::Busy(_) => panic!("nothing held the lock"),
        }
        assert!(current().is_none());
        assert!(matches!(
            acquire(Origin::Cli, None, false).unwrap(),
            Acquired::Ours(_)
        ));
    }

    #[test]
    fn a_dead_holder_is_stolen_without_waiting_out_the_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        let dir = lock_dir().unwrap();
        fs::create_dir_all(&dir).unwrap();
        // Pid 0 is never alive (`is_alive` rejects it outright), and the phase is
        // stamped *now* — so only the liveness condition can free this.
        write_state(&dir, &state_at(0, Utc::now())).unwrap();

        assert!(current().is_none(), "a dead holder is not a live update");
        assert!(matches!(
            acquire(Origin::Cli, None, false).unwrap(),
            Acquired::Ours(_)
        ));
    }

    #[test]
    fn a_live_holder_that_stopped_moving_goes_stale_on_the_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        let dir = lock_dir().unwrap();
        fs::create_dir_all(&dir).unwrap();
        // Our own pid: unambiguously alive, so the *only* thing that can free
        // this is the phase timeout. This is the hung-at-a-sudo-prompt case.
        let old = Utc::now()
            - chrono::Duration::from_std(PHASE_TIMEOUT).unwrap()
            - chrono::Duration::seconds(1);
        write_state(&dir, &state_at(std::process::id(), old)).unwrap();

        let (state, reason) = peek().unwrap();
        assert!(crate::process::is_alive(state.pid), "holder must be alive");
        assert_eq!(reason, Some(StaleReason::Stalled));
        assert!(matches!(
            acquire(Origin::Cli, None, false).unwrap(),
            Acquired::Ours(_)
        ));
    }

    #[test]
    fn a_live_holder_inside_the_timeout_is_not_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        let dir = lock_dir().unwrap();
        fs::create_dir_all(&dir).unwrap();
        let recent = Utc::now() - chrono::Duration::from_std(PHASE_TIMEOUT).unwrap()
            + chrono::Duration::seconds(60);
        write_state(&dir, &state_at(std::process::id(), recent)).unwrap();

        assert!(peek().unwrap().1.is_none());
        assert!(current().is_some());
        assert!(matches!(
            acquire(Origin::Cli, None, false).unwrap(),
            Acquired::Busy(_)
        ));
    }

    #[test]
    fn set_phase_resets_the_stall_clock() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        let dir = lock_dir().unwrap();
        fs::create_dir_all(&dir).unwrap();
        let old = Utc::now()
            - chrono::Duration::from_std(PHASE_TIMEOUT).unwrap()
            - chrono::Duration::seconds(1);
        write_state(&dir, &state_at(std::process::id(), old)).unwrap();
        assert_eq!(peek().unwrap().1, Some(StaleReason::Stalled));

        // Reconstruct a guard over that same state, the way a long-running phase
        // would, and move it on.
        let mut guard = UpdateGuard {
            dir: dir.clone(),
            state: state_at(std::process::id(), old),
        };
        guard.set_phase(Phase::RestartingServices);

        assert_eq!(peek().unwrap().1, None, "a phase change un-stales the lock");
        assert_eq!(peek().unwrap().0.phase, Phase::RestartingServices);
    }

    #[test]
    fn force_takes_a_lock_that_is_not_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        let dir = lock_dir().unwrap();
        fs::create_dir_all(&dir).unwrap();
        write_state(&dir, &state_at(std::process::id(), Utc::now())).unwrap();
        assert!(current().is_some(), "precondition: the lock is live");

        assert!(matches!(
            acquire(Origin::Cli, None, true).unwrap(),
            Acquired::Ours(_)
        ));
    }

    #[test]
    fn an_unreadable_lock_never_blocks_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        // A crash between `mkdir` and the first write leaves exactly this.
        fs::create_dir_all(lock_dir().unwrap()).unwrap();

        assert_eq!(peek().unwrap().1, Some(StaleReason::Unreadable));
        assert!(current().is_none());
        assert!(matches!(
            acquire(Origin::Cli, None, false).unwrap(),
            Acquired::Ours(_)
        ));
    }

    #[test]
    fn dropping_a_guard_whose_lock_was_stolen_leaves_the_new_owner_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = isolated(tmp.path());

        let dir = lock_dir().unwrap();
        let guard = match acquire(Origin::Cli, None, false).unwrap() {
            Acquired::Ours(g) => g,
            Acquired::Busy(_) => panic!("nothing held the lock"),
        };
        // Somebody judged us stale and took over: same directory, different pid.
        write_state(&dir, &state_at(std::process::id() + 1, Utc::now())).unwrap();

        drop(guard);

        let (state, _) = peek().expect("the new owner's lock must survive our Drop");
        assert_eq!(state.pid, std::process::id() + 1);
    }

    #[test]
    fn a_backwards_clock_is_not_read_as_abandonment() {
        // An NTP step (or a state file copied off another machine) can put
        // `phase_at` in the future. That is a clock we do not trust, not evidence
        // the holder is gone.
        let future = Utc::now() + chrono::Duration::hours(2);
        let state = state_at(std::process::id(), future);
        assert_eq!(state.stale_reason(Utc::now()), None);
    }

    #[test]
    fn humanise_reads_like_a_sentence() {
        assert_eq!(humanise(Duration::from_secs(3)), "3s");
        assert_eq!(humanise(Duration::from_secs(59)), "59s");
        assert_eq!(humanise(Duration::from_secs(60)), "1m");
        assert_eq!(humanise(Duration::from_secs(59 * 60)), "59m");
        assert_eq!(humanise(Duration::from_secs(64 * 60)), "1h 4m");
    }
}
