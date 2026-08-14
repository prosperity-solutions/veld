//! `GET`/`POST`/`DELETE /api/caffeinate` — keep this machine awake while veld works.
//!
//! A run that takes an hour, an agent left to grind through a task, a build
//! started before lunch: all of them die quietly when the laptop suspends. This
//! is the switch that says "not for the next four hours", and it is deliberately
//! machine-wide rather than per-run — the thing being kept awake is the machine,
//! and a run is only the reason.
//!
//! **How the inhibition is held.** Both platforms have a supported way to ask,
//! and both take the same shape — a process that holds the inhibition for
//! exactly as long as the utility it wraps keeps running:
//!
//! | Platform | Program | What it blocks |
//! |---|---|---|
//! | macOS | `caffeinate -s -i` | System sleep (on AC power) and idle sleep |
//! | Linux | `systemd-inhibit --what=handle-lid-switch:sleep:idle --mode=block` | logind's lid-close handling, suspend, and idle |
//!
//! Each has a **narrower form** that holds idle sleep only and leaves a shut lid
//! alone — `caffeinate -i`, and `--what=idle`. See the sharing section below for
//! which holds get which, and why that is the difference between a default that
//! is defensible and one that is not.
//!
//! The wrapped utility is `cat`, with its stdin held open by this daemon and
//! nothing ever written to it. That one choice is what makes the whole lifecycle
//! honest: **the inhibition cannot outlive the daemon**. Turning it off closes
//! the pipe, `cat` reads EOF and exits, and the inhibitor exits with it — and if
//! this process is SIGKILLed instead, the kernel closes the same pipe and the
//! same thing happens. There is no signal to send, no orphan to reap on the next
//! start, and no persisted state that could pin a machine awake forever after a
//! reboot nobody connected to a button they pressed last week. The failure
//! direction is always "the machine can sleep again".
//!
//! For the same reason the child is **not** given its own process group: every
//! other long-lived child in this tree calls `process_group(0)` precisely so it
//! survives the daemon, and that is the opposite of what this one wants.
//!
//! **The battery half needs root, so it is optional and lives elsewhere.** On
//! macOS, `-s` is documented as valid on AC power only, and there is no
//! unprivileged API for lid-closed sleep on *battery* at all. The single lever
//! is `pmset -b disablesleep`, which is why that half sits in `veld-helper`'s
//! `sleep` module and only exists on a machine set up with the privileged
//! helper. Linux has no such split — `handle-lid-switch` is a real logind
//! inhibitor and holds on battery too — so nothing here asks for it there.
//!
//! This module treats that half as a **bonus, never a requirement**: the
//! `caffeinate` hold is already in place before the helper is asked, and every
//! failure to get a lease (no privileged helper, an older helper that does not
//! know the command, a `pmset` that refuses) downgrades the coverage reported to
//! the UI rather than failing the request. `covers_battery` in the status is
//! read from the flag the renewal task owns, so it stops claiming coverage once
//! the lease has actually lapsed — renewal blips *inside* the granted window are
//! tolerated, because the setting is genuinely still held through them. A status
//! that lies in the optimistic direction is worse than no status, because the
//! user planned around it; one that panics on the first hiccup is worse than
//! useless for the same reason.
//!
//! The lease itself is the helper's safety property, not this module's; see
//! `veld-helper`'s `sleep` module for why a durable `pmset` setting may only be
//! held on something that expires.
//!
//! # Sharing arms this too, and that changes what may be held
//!
//! A share is only useful while the machine serving it is up, so a live share is
//! a reason to hold — see [`decide`] for the state machine, which is pure and
//! carries the rules. What belongs *here* is the consequence for this module's
//! central promise. An automatic hold has **no press behind it**, and the
//! privileged half writes a durable system setting, so:
//!
//! > **An automatic hold never asks the privileged helper for anything, on either
//! > power source.**
//!
//! That is what keeps "veld never simply sets `disablesleep`" true after this
//! feature rather than approximately true. The cost is only on battery, and only
//! for a lid: on **mains** the widest hold is free (`caffeinate -s` is documented
//! as valid on AC power only, and Linux's lid inhibitor is unprivileged on either
//! source), so an automatic hold on mains covers a shut lid exactly like a manual
//! one. On battery it covers idle sleep and nothing else, and one click on a
//! duration in the menu buys the rest.
//!
//! Which power source that is, is measured rather than assumed — see [`power`].

mod decide;
mod power;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime};

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};
use veld_core::db::KeepAwakePrefs;
use veld_core::helper::HelperClient;

use self::decide::{Coverage, LidGap, Plan, ShareFacts, State};
use self::power::Power;
use super::management::check_csrf;

/// Shortest timed session. Below a minute the round trip costs more than the
/// sleep it prevents, and a stray `0` would read as "no limit" to a caller that
/// did not send `null`.
const MIN_DURATION_SECS: u64 = 60;

/// Longest timed session (7 days). Not a safety limit — "no time limit" is a
/// supported answer — but a bound on what a *typo* can mean, since a request
/// asking for a million hours would otherwise be indistinguishable from one
/// asking for eight.
const MAX_DURATION_SECS: u64 = 7 * 24 * 60 * 60;

/// How often the expiry task re-checks the wall clock.
///
/// A single `sleep(duration)` would be wrong for the one case that matters: the
/// machine *can* still be suspended while this is on (macOS, battery, lid shut),
/// and tokio's timer runs on a monotonic clock that does not advance across a
/// suspend — so a four-hour session slept through for one hour would run for
/// five. Comparing against the wall clock instead means a suspend shortens the
/// remaining time exactly as the user's watch says it should.
const EXPIRY_TICK: Duration = Duration::from_secs(30);

/// The lease this daemon asks the privileged helper for, and how often it
/// renews.
///
/// The gap between them is the slack: three renewals may fail — a helper being
/// restarted by `veld update`, a moment of load — before the machine loses the
/// battery half. Renewing far inside the lease is what keeps a healthy setup
/// from flapping; the lease being *short* is what bounds how long a dead daemon
/// keeps somebody's Mac awake. Both halves matter, so change neither alone.
const HELPER_LEASE_SECS: u64 = 90;
const HELPER_RENEW_EVERY: Duration = Duration::from_secs(30);

/// The helper clamps a lease to its own ceiling and answers a bare `ok` carrying
/// no granted duration — so asking for more than it will grant is shortened with
/// no signal on either side. A compile error is the only thing that makes the two
/// numbers impossible to drift apart.
const _: () = assert!(HELPER_LEASE_SECS <= veld_core::helper::MAX_SLEEP_LEASE_SECS);
const _: () = assert!(HELPER_RENEW_EVERY.as_secs() * 2 < HELPER_LEASE_SECS);
/// The coupling that actually bites, and the reason it is a compile error rather
/// than prose: after the helper restarts it adopts the hold for
/// `SLEEP_ADOPTION_GRACE_SECS` and then hands it back. Renew less often than that
/// and every helper restart silently drops a live hold — nothing fails, nothing
/// logs, the machine just starts sleeping again mid-session.
const _: () =
    assert!(HELPER_RENEW_EVERY.as_secs() * 2 < veld_core::helper::SLEEP_ADOPTION_GRACE_SECS);

/// How long a "is there a privileged helper" answer is reused.
///
/// The probe is a real round trip with a 3s ceiling, and the idle status is
/// polled by every open client on window focus — so caching is not an
/// optimisation, it is what stops a tab switch costing a socket connection. The
/// answer changes only when somebody runs `veld setup privileged`, and being a
/// minute stale about that shows up as one menu that has not caught up yet.
const PRIVILEGED_PROBE_TTL: Duration = Duration::from_secs(60);

/// How long a power-source reading is reused.
///
/// Shorter than the privileged probe's, because unlike "is a helper installed"
/// this genuinely changes while somebody is looking at it — plugging a laptop in
/// should widen the hold and lengthen its cap within a moment, not within a
/// minute. Half the supervising tick, so a tick never re-uses the reading the
/// previous one took.
const POWER_TTL: Duration = Duration::from_secs(15);

/// The coupling the paragraph above states, as the compile error every other
/// coupled pair in this file already is: a reading older than the cadence that
/// consumes it would notice a charger a whole tick late for no reason.
const _: () = assert!(POWER_TTL.as_secs() * 2 <= EXPIRY_TICK.as_secs());

/// How many inhibitors may die on their own before this stops re-spawning them.
///
/// Two rather than one, because a single death is also what a `veld update`
/// racing a spawn looks like, and giving up on the first would turn a transient
/// into a session-long outage.
const IMMEDIATE_DEATHS_BEFORE_GIVING_UP: u32 = 2;

/// Ceiling on a helper round trip made while the session lock is held.
///
/// `HelperClient` bounds its own sends at 15s, which is right for a `veld start`
/// that cannot proceed without a route and wrong for this: taking and dropping
/// the lease both happen under the session lock, so a wedged helper would stall
/// the button *and* every status poll behind it for the whole of that budget.
/// This half is a bonus — the machine is already being held awake by the time it
/// runs — so it is better to give up on the battery half quickly than to make
/// the whole control feel broken. Losing the race just means the lease is not
/// taken, or is released a lease-length later by the helper's own watchdog.
///
/// **It bounds one call, not the critical section.** A `start` that replaces a
/// live session can spend this twice (the predecessor's release, then the new
/// hold) plus two 3s connection probes plus the child reap — so the honest worst
/// case under `ACTIVE` is on the order of fifteen seconds against a wedged
/// helper, not five. Every number in that sum is bounded, which is the property
/// that matters; do not read the constant as the total.
const HELPER_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// The live inhibition, if there is one. At most one per machine.
struct Session {
    child: Child,
    /// The write end of the wrapped `cat`'s stdin. Holding it is the inhibition;
    /// dropping it is the whole shutdown sequence.
    stdin: Option<ChildStdin>,
    started_at: DateTime<Utc>,
    /// What this process was spawned to hold. A session whose coverage no longer
    /// matches the plan — the power source changed, or a manual reason arrived
    /// beside an automatic one — is replaced rather than adjusted, because the
    /// coverage lives in the child's argv.
    coverage: Coverage,
    /// Whether the privileged half — battery, lid closed — is in force.
    ///
    /// Shared with the renewal task rather than a plain `bool`, so a lease that
    /// starts failing downgrades what the status claims instead of leaving a
    /// promise nothing is keeping. It doubles as the renewal loop's stop flag:
    /// clearing it before releasing is what stops a renewal landing after the
    /// release and re-arming a lease nobody wants.
    battery: Arc<AtomicBool>,
    /// Whether a lease was *wanted*, which is what decides whether this session
    /// has to be respawned when the plan changes its mind about one.
    wanted_lease: bool,
    /// Whether this session's coverage **depends** on that lease. Only this one
    /// makes a missing lease a fault worth reporting. See `decide::Plan`.
    lease_required: bool,
    /// The privileged helper this session's lease lives on, and the task
    /// renewing it. Both `None` when there is no privileged half.
    helper: Option<HelperClient>,
    renew: Option<tokio::task::JoinHandle<()>>,
}

/// Everything behind the one lock.
///
/// The remembered [`State`] outlives any session — an episode's opt-out has to
/// survive the hold it ended — so it cannot live *in* `Session`. Keeping both
/// under a single mutex is what makes the reconciler's decision and its action
/// one critical section, which in turn is what lets it be fired from a detached
/// task with no sequence number: two concurrent runs cannot interleave a decision
/// taken from one set of facts with an action taken for another.
#[derive(Default)]
struct Machine {
    state: State,
    session: Option<Session>,
}

static ACTIVE: LazyLock<Mutex<Machine>> = LazyLock::new(|| Mutex::new(Machine::default()));

/// What the share manager last saw, written **absolutely** rather than as a delta.
///
/// A `fetch_add`/`fetch_sub` pair would be the obvious shape and is a trap: a
/// `DELETE /api/shares/{id}` for an id that is already gone reaches the manager
/// and bails without removing anything, so a decrement placed beside the lock
/// rather than inside the removal underflows — after which the count never
/// reaches zero again, the episode never ends, and the machine is held awake for
/// the daemon's life. Storing the map's length cannot have that bug.
///
/// A `std::sync::Mutex`, not the async one: it is written from inside the share
/// manager's own lock, where there is nothing to await, and held for a move.
static SHARES: LazyLock<std::sync::Mutex<ShareFacts>> =
    LazyLock::new(|| std::sync::Mutex::new(ShareFacts::default()));

/// Cached power reading. See [`POWER_TTL`].
static POWER: LazyLock<Mutex<Option<(Instant, Power)>>> = LazyLock::new(|| Mutex::new(None));

/// Runs the supervising tick at most once per process.
static SUPERVISOR: std::sync::Once = std::sync::Once::new();

/// Cached answer to "is a privileged helper reachable", with the time it was
/// learned. See [`PRIVILEGED_PROBE_TTL`].
static PRIVILEGED: LazyLock<Mutex<Option<(Instant, bool)>>> = LazyLock::new(|| Mutex::new(None));

/// Sticky: this helper answered `unknown command`, so it predates the feature.
///
/// Separate from the cache above because that one is a *reachability* probe on a
/// 60s TTL — it would expire this answer and flip back to "capable", swapping the
/// actionable "run `veld setup privileged`" line for an unactionable fault report
/// one minute into a session that may run all night. A helper only gains the
/// command by being replaced, and that replaces this process's view with a
/// restart.
static NO_BATTERY_HALF: AtomicBool = AtomicBool::new(false);

pub fn routes() -> Router {
    Router::new().route(
        "/api/caffeinate",
        get(get_state).post(post_start).delete(delete_stop),
    )
}

// ---------------------------------------------------------------------------
// The platform's inhibitor
// ---------------------------------------------------------------------------

/// The program this platform holds sleep off with, or why it cannot.
fn program_name() -> Result<&'static str, String> {
    if cfg!(target_os = "macos") {
        Ok("caffeinate")
    } else if cfg!(target_os = "linux") {
        Ok("systemd-inhibit")
    } else {
        Err("Keeping this machine awake isn’t supported on this operating system.".to_owned())
    }
}

/// The full argv, resolved against `PATH`.
///
/// Resolved rather than assumed so the UI can say *why* the control is dead
/// instead of offering a button that fails on click — a Linux box without
/// systemd is a real configuration, not a broken one.
///
/// `PATH` here is deliberately the daemon's own, not the user's login-shell
/// `PATH`: these are system binaries in system locations, and injecting the
/// user's `PATH` would let a `~/bin/caffeinate` decide what "keep awake" means.
/// The AGENTS.md rule it looks like it should follow is about *user-supplied*
/// commands; nothing about this argv comes from a config or a request.
fn inhibitor_argv(coverage: Coverage) -> Result<Vec<String>, String> {
    let name = program_name()?;
    let path = which_on_path(name).ok_or_else(|| {
        format!("Keeping this machine awake needs `{name}`, which isn’t on this machine.")
    })?;

    let mut argv = vec![path.to_string_lossy().into_owned()];
    if cfg!(target_os = "macos") {
        // -s: no system sleep — this is the one that survives a closed lid, and
        //     it is honoured on AC power only (see the module docs). Asked for
        //     only when the plan wants the lid covered; on battery it would be a
        //     no-op anyway, so leaving it off there costs nothing and keeps the
        //     argv an honest statement of what is being held.
        // -i: no idle sleep — the one that keeps a locked screen from putting
        //     the machine under while a build runs. Always asked for: it is the
        //     whole of an automatic hold on battery.
        // Display sleep is deliberately *not* held: the screen may blank and
        // lock, which is what somebody walking away from the machine wants.
        if coverage == Coverage::LidToo {
            argv.push("-s".to_owned());
        }
        argv.push("-i".to_owned());
    } else {
        // `handle-lid-switch` is what makes a closed lid a no-op; `sleep` covers
        // an explicit suspend request and `idle` covers logind's idle action.
        //
        // Unlike macOS the lid half here holds on battery too and needs no root,
        // which is exactly why it must be *asked for* rather than taken: an
        // automatic hold that pinned a discharging laptop open in somebody's bag
        // is the outcome the narrower `--what` exists to prevent.
        match coverage {
            Coverage::LidToo => argv.push("--what=handle-lid-switch:sleep:idle".to_owned()),
            Coverage::IdleOnly => argv.push("--what=idle".to_owned()),
        }
        argv.push("--who=Veld".to_owned());
        argv.push("--why=Veld is keeping this machine awake".to_owned());
        argv.push("--mode=block".to_owned());
    }
    // The utility both programs wrap. `cat` blocks reading a pipe nobody writes
    // to and exits the instant that pipe closes, which is how this daemon's
    // death — by any means — releases the inhibition. See the module docs.
    argv.push("cat".to_owned());
    Ok(argv)
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        let ok = std::fs::metadata(&candidate)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        ok.then_some(candidate)
    })
}

// ---------------------------------------------------------------------------
// The privileged half: battery, lid closed
// ---------------------------------------------------------------------------

/// Whether the battery half is even possible here: macOS, with a privileged
/// helper reachable.
///
/// Only macOS asks. On Linux the unprivileged `handle-lid-switch` inhibitor
/// already holds on battery, so probing would spend a round trip to learn
/// something that changes nothing.
///
/// Deliberately probes the **system socket only** (`connect_privileged`), never
/// `HelperClient::connect`, which falls back to the user-socket helper. That
/// helper runs as the user, `pmset` would refuse it, and the fallback would turn
/// "this machine cannot do that" into an error that reads like a bug.
async fn battery_capable() -> bool {
    if !cfg!(target_os = "macos") || NO_BATTERY_HALF.load(Ordering::Relaxed) {
        return false;
    }
    // **Single-flighted: the lock is held across the probe**, not just around the
    // cache read. `get_state` is a `GET` with no CSRF gate, so a cross-origin page
    // can drive it (it cannot read the reply, but the side effect still runs) —
    // and the side effect here is opening a connection to the *root* helper
    // socket. `settings.rs:62-77` writes the rule down for this exact shape and
    // fixes it the same way: a hundred concurrent callers produce one probe and
    // ninety-nine clones of its answer.
    let mut cache = PRIVILEGED.lock().await;
    if let Some((learned, answer)) = *cache {
        if learned.elapsed() < PRIVILEGED_PROBE_TTL {
            return answer;
        }
    }
    let answer = HelperClient::connect_privileged().await.is_ok();
    *cache = Some((Instant::now(), answer));
    answer
}

/// Force the capability cache to "no privileged half".
///
/// Called when the helper answers `unknown command`: the socket is reachable, so
/// the probe says capable, but this helper predates the feature and never will
/// take a lease. Without this the menu shows "the privileged helper didn't take
/// the lease" — a fault with no user action — instead of the actionable line
/// naming `veld setup privileged`.
async fn mark_not_battery_capable() {
    *PRIVILEGED.lock().await = Some((Instant::now(), false));
    NO_BATTERY_HALF.store(true, Ordering::Relaxed);
}

/// Take the first lease, returning the helper to renew it on.
///
/// Best-effort by design: this is the *bonus* half. The `caffeinate` hold is
/// already in place by the time this runs, so every failure here downgrades the
/// coverage rather than failing the request — a machine with no privileged
/// helper still gets everything an unprivileged process can hold.
async fn acquire_battery_lease() -> Option<HelperClient> {
    if !battery_capable().await {
        return None;
    }
    let client = HelperClient::connect_privileged().await.ok()?;
    match tokio::time::timeout(
        HELPER_CALL_TIMEOUT,
        client.hold_sleep_disabled(HELPER_LEASE_SECS),
    )
    .await
    {
        Ok(Ok(_)) => {
            info!("battery lid-closed sleep held via the privileged helper");
            Some(client)
        }
        // Includes the version-skew case: a helper older than this feature
        // answers `unknown command`, which is exactly "no battery coverage
        // here" and not a reason to fail the keep-awake.
        Ok(Err(e)) => {
            // An `unknown command` reply is a *capability* answer, not a fault:
            // this helper predates the feature. Record it so the menu offers the
            // actionable line instead of reporting a failure the user cannot act
            // on. Matched on the helper's own wording (`handler.rs`'s fallback
            // arm); a miss only costs the better message, never correctness.
            let message = e.to_string();
            if message.contains("unknown command") {
                mark_not_battery_capable().await;
                info!("this helper predates the keep-awake lease; mains power only");
            } else {
                warn!(error = %message, "could not hold battery sleep; keeping the machine awake on mains power only");
            }
            None
        }
        Err(_) => {
            warn!(
                "the privileged helper did not answer in time; keeping the machine awake on mains power only"
            );
            None
        }
    }
}

/// Renew the lease until the session ends or the helper stops answering.
async fn renew_battery_lease(client: HelperClient, battery: Arc<AtomicBool>) {
    // The whole point of asking for 90s and renewing at 30s is that renewals may
    // fail and the hold survives — `veld update` restarting the helper is the
    // routine example. So a failure retries; only the lease *actually lapsing*
    // ends the loop and drops the claim. (Giving up on the first error, which is
    // what this did before review, made the 90/30 ratio buy nothing.)
    // Wall clock, matching the helper's own deadline. `Instant` does not advance
    // across a macOS suspend, so after a resume the daemon would under-count the
    // gap and keep claiming coverage for a lease the helper already reverted.
    let mut last_ok = SystemTime::now();
    loop {
        tokio::time::sleep(HELPER_RENEW_EVERY).await;
        // Checked before every renewal so a teardown that has already cleared
        // the flag cannot be followed by a renewal that re-arms the lease.
        if !battery.load(Ordering::Relaxed) {
            return;
        }
        // Bounded like the other two helper calls: the default 15s send timeout
        // is longer than the renewal interval, so a wedged helper would stack
        // renewals on top of each other.
        match tokio::time::timeout(
            HELPER_CALL_TIMEOUT,
            client.hold_sleep_disabled(HELPER_LEASE_SECS),
        )
        .await
        {
            Ok(Ok(_)) => last_ok = SystemTime::now(),
            failed => {
                // Still inside the granted window: the setting is genuinely still
                // held, so the status must keep saying so rather than downgrading
                // on a blip.
                let slack = last_ok
                    .elapsed()
                    .unwrap_or(Duration::from_secs(HELPER_LEASE_SECS));
                if slack < Duration::from_secs(HELPER_LEASE_SECS) {
                    warn!(?failed, "battery sleep lease renewal failed; retrying");
                    continue;
                }
                warn!("battery sleep lease lapsed; the machine is held awake on mains power only");
                battery.store(false, Ordering::Relaxed);
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

/// Release a session's inhibition and reap its process.
///
/// Does **not** touch `timer` — the caller owns that, because the expiry task is
/// itself a caller and a task that aborts its own handle mid-teardown never
/// finishes reaping the child.
async fn stop_session(mut session: Session) {
    // The privileged half first, and in this order: clear the flag, stop the
    // renewal task, then release. Releasing before stopping renewals would let
    // an in-flight one re-arm the lease behind the release.
    //
    // The release is a fast path, not the guarantee — if it fails, or if a
    // renewal already past its flag check lands after it, the helper's own
    // watchdog reverts within the lease. That is the property worth keeping:
    // nothing here has to succeed for the machine to sleep again.
    session.battery.store(false, Ordering::Relaxed);
    if let Some(renew) = session.renew.take() {
        renew.abort();
    }
    if let Some(client) = session.helper.take() {
        match tokio::time::timeout(HELPER_CALL_TIMEOUT, client.release_sleep_disabled()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                warn!(error = %e, "could not release the battery sleep lease; it will expire on its own")
            }
            Err(_) => warn!(
                "the privileged helper did not answer the release in time; the lease will expire on its own"
            ),
        }
    }

    // Closing the pipe is the entire shutdown: `cat` reads EOF and exits, and
    // the inhibitor exits with it. No signal, so an explicit "off" and this
    // daemon dying take exactly the same path through the kernel.
    drop(session.stdin.take());
    match tokio::time::timeout(Duration::from_secs(2), session.child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => warn!("could not reap the keep-awake process: {e}"),
        Err(_) => {
            // Nothing observed does this — both programs exit as soon as their
            // utility does — but an unreaped child would keep a zombie around
            // for the daemon's whole life, so say so and insist.
            warn!("keep-awake process still running after its pipe closed; killing it");
            let _ = session.child.kill().await;
        }
    }
}

/// Drop a session whose inhibitor process has already exited by itself.
///
/// Nothing else notices that: `active` and `covers_battery` are derived from the
/// session existing, so an inhibitor that dies a millisecond after spawn — polkit
/// refusing a `handle-lid-switch` inhibitor to a daemon with no active login
/// session, D-Bus unavailable — would leave the UI reporting "keeping this
/// machine awake" forever with nothing held. This module's own docs call that the
/// worst outcome available ("a status that lies in the optimistic direction is
/// worse than no status"), so the status path checks before answering.
async fn reap_if_dead(machine: &mut Machine) {
    let dead = match machine.session.as_mut() {
        Some(session) => match session.child.try_wait() {
            Ok(Some(status)) => {
                // The one log line that explains a keep-awake which silently did
                // nothing. `stderr` is `Stdio::null()`, so the exit status is all
                // there is — enough to tell "refused immediately" from "never
                // started".
                warn!(%status, "the keep-awake process exited on its own; nothing is being held");
                true
            }
            Ok(None) => false,
            Err(e) => {
                warn!(error = %e, "could not check whether the keep-awake process is alive");
                false
            }
        },
        None => false,
    };
    // Still running, having survived to a *later* reconcile than the one that
    // spawned it: whatever killed the previous inhibitor was transient. Reset
    // here rather than in `spawn_session`, where the first version of this put
    // it — every death is followed immediately by a respawn inside the same
    // `reconcile`, so resetting on spawn made the count alternate 1→0 and the
    // give-up arm below unreachable, which is the whole of what it is for.
    if !dead && machine.session.is_some() {
        machine.state.deaths = 0;
    }
    if dead {
        take_and_stop(machine).await;
        // The reasons stay, so the caller re-plans and a hold whose inhibitor
        // died is respawned rather than silently lost.
        //
        // What bounds the respawn is **not** `reconcile`'s failure path — an
        // inhibitor that starts cleanly and then exits (polkit refusing a
        // `handle-lid-switch` inhibitor to a daemon with no active login session
        // is the documented case) returns `Ok`, so that path never fires. It is
        // this counter: a child that dies immediately, twice running, is a
        // machine that cannot hold the inhibition, and re-exec'ing it twice a
        // minute for the daemon's life would achieve nothing but noise in the
        // log it is already writing.
        machine.state.deaths += 1;
        if machine.state.deaths >= IMMEDIATE_DEATHS_BEFORE_GIVING_UP {
            warn!(
                "the keep-awake process keeps exiting on its own; giving up until sharing changes"
            );
            machine.state.spawn_failed = true;
            machine.state.reasons = decide::Reasons::default();
        }
    }
}

/// Stop whatever session is in `slot`.
async fn take_and_stop(machine: &mut Machine) {
    if let Some(session) = machine.session.take() {
        stop_session(session).await;
    }
    // A stale count from an earlier hold would make the *next*, unrelated one
    // give up on its first death — exactly what a threshold of two exists to
    // prevent.
    machine.state.deaths = 0;
}

/// Bring the held inhibition into line with the machine's state.
///
/// **Takes no lock of its own** — it runs inside the caller's, which is the whole
/// point. The naive shape, where a reconciler locks and then calls a `start` that
/// locks again, deadlocks on the first share after boot (`tokio::sync::Mutex` is
/// not reentrant) and takes the coffee cup, its two mutating routes and every
/// status poll down with it, permanently. Splitting the lock-free half out is
/// what makes decision and action one critical section.
async fn apply(
    machine: &mut Machine,
    prefs: &KeepAwakePrefs,
    power: Power,
) -> Result<(), (StatusCode, String)> {
    let Some(plan) = machine.state.plan(prefs, power) else {
        if machine.session.is_some() {
            // The counterpart of the line above. The old per-session expiry task
            // logged this and nothing replaced it when that task went, so a hold
            // ending left no trace at all in the daemon log — which is the one
            // place somebody reconstructs "why did this machine sleep at 3am".
            info!("keep-awake ended; this machine can sleep again");
        }
        take_and_stop(machine).await;
        return Ok(());
    };

    // A live session that already holds the right thing is left strictly alone.
    // Deadlines live in `state`, not in the child, so a changed deadline is not a
    // reason to respawn an inhibitor — only a changed *coverage* is.
    if let Some(session) = machine.session.as_ref() {
        if session.coverage == plan.coverage && session.wanted_lease == plan.want_lease {
            return Ok(());
        }
    }

    spawn_session(machine, plan).await
}

/// Spawn an inhibitor for `plan` and retire whatever it replaces.
async fn spawn_session(machine: &mut Machine, plan: Plan) -> Result<(), (StatusCode, String)> {
    let argv = inhibitor_argv(plan.coverage).map_err(|e| (StatusCode::NOT_IMPLEMENTED, e))?;

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Belt to the pipe's braces for the ordinary paths (a dropped session,
        // a panicking task); the pipe is what covers SIGKILL.
        .kill_on_drop(true);
    // No `process_group(0)`, deliberately: staying in the daemon's group is a
    // second way this cannot outlive it. See the module docs.

    let mut child = cmd.spawn().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {e}", argv[0]),
        )
    })?;
    let stdin = child.stdin.take();
    if stdin.is_none() {
        // Unreachable with `Stdio::piped()`, but the pipe *is* the off switch —
        // a session without one could never be turned off, so refuse to have it.
        let _ = child.kill().await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not open a pipe to the keep-awake process".to_owned(),
        ));
    }

    // The predecessor is torn down only now that its replacement is running.
    // Replace rather than refuse, because changing the answer is the menu's whole
    // job — but the *order* matters twice over: stopping first left the machine
    // unheld for as long as the teardown took (a helper release plus a process
    // reap, seconds in the bad case), and a spawn that then failed would have
    // destroyed a running session while reporting only "couldn't start". The two
    // inhibitors overlap for an instant instead, which costs nothing.
    take_and_stop(machine).await;

    // The privileged half, after the unprivileged one is already holding: this
    // can only ever *add* battery/lid-closed coverage, so its failure must not
    // cost the caller the hold they asked for. Only ever reached for a hold a
    // human asked for — see the module docs.
    let helper = if plan.want_lease {
        acquire_battery_lease().await
    } else {
        None
    };
    let battery = Arc::new(AtomicBool::new(helper.is_some()));
    let renew = helper
        .clone()
        .map(|client| tokio::spawn(renew_battery_lease(client, Arc::clone(&battery))));

    info!(
        reason = machine.state.reasons.wire(),
        coverage = ?plan.coverage,
        battery = battery.load(Ordering::Relaxed),
        until = ?machine.state.reasons.expires_at(),
        "keeping this machine awake"
    );
    // **Both** callers of this need the tick, which is why it is here as well as
    // in `reconcile`. `post_start` applies directly and never reconciles, so a
    // manual hold on a daemon that had never hosted a share or seen a settings
    // write would have had nothing at all to expire it — "keep this machine
    // awake for 15 minutes" would have held until the daemon stopped, which is
    // the exact failure direction this module promises never to take. `Once`
    // makes the double call free.
    start_supervisor();
    // It started, so whatever stopped the last attempt is over. Without this the
    // flag outlives the failure it describes and the UI keeps reporting a
    // machine that cannot be held awake while one is being held.
    machine.state.spawn_failed = false;
    machine.session = Some(Session {
        child,
        stdin,
        started_at: Utc::now(),
        coverage: plan.coverage,
        battery,
        wanted_lease: plan.want_lease,
        lease_required: plan.lease_required,
        helper,
        renew,
    });
    Ok(())
}

/// Re-read the world and act on it. The one entry point every trigger uses.
///
/// Fired by a share starting or stopping, by a settings change, and by the
/// supervising tick. Idempotent, because [`State::recompute`] reads the current
/// facts rather than a remembered edge — so two of these racing converge instead
/// of needing an epoch to order them.
pub(crate) async fn reconcile() {
    let quiet = {
        let guard = ACTIVE.lock().await;
        guard.session.is_none()
            && guard.state.reasons.is_empty()
            // **Every** latching bit, not just the visible ones. Testing only
            // the session and the reasons made `recompute`'s "the episode is
            // over" arm unreachable in a running daemon — the share count hits
            // zero, this returns early, and the flags that arm clears stay set
            // for the daemon's life. One failed spawn then meant no automatic
            // hold ever again, with the cup permanently reporting a machine that
            // could not be held awake.
            && guard.state.episode.is_none()
            && guard.state.opted_out.is_none()
            && !guard.state.spawn_failed
            && guard.state.deaths == 0
            && SHARES.lock().unwrap_or_else(|e| e.into_inner()).count == 0
    };
    // Nothing held, nothing shared, and nothing remembered: there is no decision
    // to make, and making it anyway would spawn `pmset` and open a database every
    // thirty seconds for the life of a daemon whose user is not sharing anything.
    if quiet {
        return;
    }

    // **Before** the spawn, not after it. Tying the tick to a successful
    // `spawn_session` looked equivalent and was not: a share started while the
    // switch for the current power source is *off* arms nothing, so nothing
    // would ever poll — and plugging the charger in, which is precisely the
    // event that should widen the answer, would never be noticed. The supervisor
    // has to exist wherever there is something to supervise, which is here.
    start_supervisor();

    // Both reads happen **before** the session lock. `keep_awake()` goes through
    // a blocking rusqlite handle and the power probe spawns a process; either one
    // inside the critical section would stall every status poll behind it, and
    // this module's own `get_state` already avoids exactly that shape.
    // Falling back rather than returning. Bailing here looked safe and is the
    // opposite: nothing else prunes a deadline or stops a session, so a database
    // that is briefly unreadable — locked, disk full, mid-`veld update` — would
    // leave the machine held awake for as long as it stayed that way, which is
    // the one direction this module promises never to fail in.
    let prefs = load_prefs().await.unwrap_or_else(fallback_prefs);
    let power = current_power().await;

    let mut guard = ACTIVE.lock().await;
    // Read **under** the session lock, unlike prefs and power above. Those two
    // are stale-tolerant — being a moment behind on a setting or a charger costs
    // one tick — but the share count is what decides whether a hold exists at
    // all, and a reconcile that read `1` before an unshare and applied after it
    // would re-arm a hold with nothing shared, claiming "while you're sharing" in
    // the UI until the next tick undid it.
    let shares = *SHARES.lock().unwrap_or_else(|e| e.into_inner());
    reap_if_dead(&mut guard).await;
    guard.state.recompute(Utc::now(), &prefs, power, shares);
    if let Err((_, message)) = apply(&mut guard, &prefs, power).await {
        // A detached caller has nowhere to return this to, and the share that
        // triggered it must not fail because the machine cannot be held awake.
        warn!(error = %message, "could not update the keep-awake hold");
        // Only the **automatic** reason is dropped. Clearing all of them also
        // cancelled a manual hold the user had explicitly asked for — and since
        // `spawn_session` fails *before* it retires the predecessor, a transient
        // failure left the old inhibitor running while the next tick planned for
        // no reasons at all and tore that working session down. One failed fork
        // could end an eight-hour hold.
        guard.state.reasons.sharing = None;
        // And only when nothing survived: a machine still holding something is
        // not a machine that cannot hold anything, and the flag is what the cup
        // reports as "Veld could not start the keep-awake".
        if guard.session.is_none() {
            guard.state.spawn_failed = true;
        }
    }
}

/// How long a settings read is reused by the status path.
///
/// `GET /api/caffeinate` has no CSRF gate — it is a read — and is polled by every
/// open client on window focus, so a database open per request is a blocking-pool
/// slot a cross-origin page can spend at will. The same reasoning the helper
/// probe and the power reading are already cached under. Short, because the cup's
/// own switch writes these settings and the menu should reflect it.
const PREFS_TTL: Duration = Duration::from_secs(5);

static PREFS: LazyLock<Mutex<Option<(Instant, KeepAwakePrefs)>>> =
    LazyLock::new(|| Mutex::new(None));

/// The keep-awake settings, cached and single-flighted like the other two reads
/// on the status path.
async fn cached_prefs() -> KeepAwakePrefs {
    let mut cache = PREFS.lock().await;
    if let Some((learned, prefs)) = *cache {
        if learned.elapsed() < PREFS_TTL {
            return prefs;
        }
    }
    let prefs = load_prefs().await.unwrap_or_else(fallback_prefs);
    *cache = Some((Instant::now(), prefs));
    prefs
}

/// What to assume when the settings cannot be read.
///
/// The defaults come from the one place that defines them — an inline copy would
/// be a fourth statement of the same numbers, and the one nothing checks.
/// `manual_on_battery` is deliberately **false** here rather than its real
/// default: with the database unreadable we cannot know whether the user turned
/// it off, and guessing `true` makes `plan` want a lease, find none held, and
/// report `no_helper` — sending somebody to `veld setup privileged` to fix a
/// lease they themselves declined.
fn fallback_prefs() -> KeepAwakePrefs {
    KeepAwakePrefs {
        sharing_on_power: true,
        sharing_on_power_minutes: veld_core::db::DEFAULT_KEEP_AWAKE_SHARING_ON_POWER_MINUTES,
        sharing_on_battery: true,
        sharing_on_battery_minutes: veld_core::db::DEFAULT_KEEP_AWAKE_SHARING_ON_BATTERY_MINUTES,
        manual_on_battery: false,
    }
}

/// Read the keep-awake settings off the blocking database handle.
async fn load_prefs() -> Option<KeepAwakePrefs> {
    match tokio::task::spawn_blocking(|| veld_core::db::Db::open().map(|db| db.keep_awake())).await
    {
        Ok(Ok(prefs)) => Some(prefs),
        Ok(Err(e)) => {
            warn!(error = %e, "could not read the keep-awake settings");
            None
        }
        Err(e) => {
            warn!(error = %e, "the keep-awake settings read panicked");
            None
        }
    }
}

/// The tick that makes every deadline and every power change take effect.
///
/// One task for the process rather than one per deadline. A single
/// `sleep(duration)` would be wrong for the case that matters: the machine *can*
/// still be suspended while this is on (macOS, battery, lid shut), and tokio's
/// timer runs on a monotonic clock that does not advance across a suspend — so a
/// four-hour session slept through for one hour would run for five. Re-deciding
/// against the wall clock instead means a suspend shortens the remaining time
/// exactly as the user's watch says it should.
///
/// It is also what notices a charger being plugged in or pulled out, which is
/// deliberately a poll: neither platform offers a power-source event this daemon
/// could subscribe to without linking a system framework, and the cost of being
/// up to a tick late is a hold that is briefly wider or narrower than it will be.
fn start_supervisor() {
    SUPERVISOR.call_once(|| {
        tokio::spawn(async {
            loop {
                // The *sooner* of the cadence and the next deadline. A plain
                // tick made a manual hold stop up to thirty seconds late, where
                // the per-session timer it replaced stopped within milliseconds
                // — and "4 hours" ending at 4:00:29 is the number the user was
                // shown being wrong. Still wall-clock, and still re-decided from
                // scratch, so a suspend shortens the remaining time exactly as
                // the user's watch says it should.
                tokio::time::sleep(next_wakeup().await).await;
                reconcile().await;
            }
        });
    });
}

/// How long until the supervisor should next re-decide.
///
/// Bounded above by [`EXPIRY_TICK`], because a hold can also end for reasons no
/// deadline predicts — a share stopping, an inhibitor dying, a charger moving.
async fn next_wakeup() -> Duration {
    let guard = ACTIVE.lock().await;
    let until_deadline = guard
        .state
        .reasons
        .expires_at()
        .map(|at| (at - Utc::now()).to_std().unwrap_or(Duration::ZERO));
    drop(guard);
    match until_deadline {
        // A deadline already passed, or is within a tick: wake for it. Floored so
        // a deadline in the past cannot spin.
        Some(remaining) if remaining < EXPIRY_TICK => remaining.max(Duration::from_millis(250)),
        _ => EXPIRY_TICK,
    }
}

/// The current power source, cached.
///
/// The probe spawns a process on macOS and the idle status is polled by every
/// open client on window focus, so caching is not an optimisation — it is what
/// stops a tab switch costing a `pmset`. Single-flighted the same way the
/// privileged probe is, and for the same reason.
async fn current_power() -> Power {
    let mut cache = POWER.lock().await;
    if let Some((learned, answer)) = *cache {
        if learned.elapsed() < POWER_TTL {
            return answer;
        }
    }
    let answer = power::read().await;
    *cache = Some((Instant::now(), answer));
    answer
}

/// Record what the share manager currently holds, and act on it.
///
/// Called from `ShareManager` at its two chokepoints. The write is synchronous
/// and absolute (see [`SHARES`]); the reconcile is **detached on purpose** —
/// `spawn_session` can spend seconds against a wedged privileged helper, and a
/// share start must never wait on the machine being kept awake.
/// Drop the cached settings, because they were just written.
///
/// Without this the cup's own "do this whenever I share" switch could take a
/// TTL to be reflected back by the status it is rendered from — a control that
/// visibly lags the click that moved it.
pub(crate) async fn settings_changed() {
    *PREFS.lock().await = None;
    reconcile().await;
}

pub(crate) fn shares_changed(count: usize, latest_expiry: Option<DateTime<Utc>>) {
    *SHARES.lock().unwrap_or_else(|e| e.into_inner()) = ShareFacts {
        count,
        latest_expiry,
    };
    tokio::spawn(reconcile());
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// The wire status.
///
/// `battery_capable` is passed in rather than looked up here because learning it
/// is an `await` and this runs under the session lock. Note what the UI is given:
/// `platform` plus two booleans, not a sentence. Composing "on mains power only,
/// run `veld setup privileged`" is the bundle's job — the daemon knows the facts,
/// and the copy belongs where it can be edited without a daemon release.
fn status_of(
    machine: &Machine,
    battery_capable: bool,
    power: Power,
    prefs: &KeepAwakePrefs,
) -> Value {
    // Reported for the coverage the machine would take *now*, which is the one a
    // menu offering "keep awake" is about to get.
    let unsupported = inhibitor_argv(Coverage::LidToo).err();
    let session = machine.session.as_ref();
    let reasons = machine.state.reasons;
    let plan = machine.state.plan(prefs, power);
    // A lease that was wanted and is not held is the one case worth reporting as
    // a fault. Read from the flag the renewal task owns, so a lease that started
    // failing stops being claimed rather than leaving a promise nothing is keeping.
    let live = session.map(|s| decide::LiveHold {
        coverage: s.coverage,
        lease_required: s.lease_required,
        lease_held: s.battery.load(Ordering::Relaxed),
    });
    let lease_held = live.is_some_and(|l| l.lease_held);
    let (covers_lid, lid_gap) = decide::lid_state(plan, live);
    let mut out = json!({
        "supported": unsupported.is_none(),
        "unsupported_reason": unsupported,
        "active": session.is_some(),
        "platform": if cfg!(target_os = "macos") { "macos" }
                    else if cfg!(target_os = "linux") { "linux" }
                    else { "other" },
        // Whether the battery/lid-closed half is *available* — macOS with a
        // privileged helper. Always false on Linux, where the unprivileged
        // inhibitor already covers battery and there is nothing to add.
        "battery_capable": battery_capable,
        // Retained under its old name and meaning: is the privileged lease in
        // force right now. `lid_gap` is what the UI composes its copy from.
        "covers_battery": lease_held,
        // Where this machine's power is coming from, and whether it has a battery
        // at all — the settings dialog hides two rows that can never apply on a
        // desktop rather than offering controls that do nothing.
        "power_source": power.source.as_str(),
        "has_battery": power.has_battery,
        // Which reasons hold: `manual`, `sharing`, `both`, or `none`. The copy is
        // composed in the bundle, as everything else here is — the daemon knows
        // the facts, and a sentence belongs where it can be edited without a
        // daemon release.
        "reason": reasons.wire(),
        // Does a shut lid keep this machine awake right now, and if not, why not.
        // Four different answers, and telling them apart is what stops the UI
        // reporting a fault for a lease that was never asked for.
        "covers_lid": covers_lid,
        "lid_gap": lid_gap.map(|gap| match gap {
            LidGap::Automatic => "automatic",
            LidGap::Setting => "setting",
            LidGap::NoHelper => "no_helper",
        }),
        // Sharing is live but the automatic hold is not, and will not come back
        // for this share — its cap ran out, or somebody switched it off. The one
        // state a user would otherwise have to infer from a cup that stopped
        // glowing, so it is said rather than left to be noticed.
        "sharing_spent": machine.state.opted_out.is_some(),
        // The inhibitor could not be started at all. Distinct from a spent cap
        // because the remedy is different and the reassurance is wrong.
        "hold_failed": machine.state.spawn_failed,
    });
    if let Some(s) = session {
        let expires_at = reasons.expires_at();
        out["started_at"] = json!(s.started_at.to_rfc3339());
        out["expires_at"] = json!(expires_at.map(|t| t.to_rfc3339()));
        // Clamped at zero: between the deadline passing and the supervising tick
        // taking the lock there is a moment where this would be negative, and a
        // negative "remaining" renders as a countdown running backwards.
        out["remaining_secs"] = json!(expires_at.map(|t| (t - Utc::now()).num_seconds().max(0)));
    }
    out
}

/// Build a status, doing every await that must not happen under the lock first.
async fn current_status() -> Json<Value> {
    // Learned before the lock, not under it: the probe can take up to 3s on a
    // wedged helper, and holding the session lock for that would stall a
    // concurrent start or stop behind a read.
    let capable = battery_capable().await;
    let power = current_power().await;
    let prefs = cached_prefs().await;
    let mut guard = ACTIVE.lock().await;
    reap_if_dead(&mut guard).await;
    Json(status_of(&guard, capable, power, &prefs))
}

async fn get_state() -> Json<Value> {
    current_status().await
}

#[derive(Deserialize)]
struct StartBody {
    /// Absent or `null` means "until I turn it off".
    #[serde(default)]
    duration_secs: Option<u64>,
}

async fn post_start(
    headers: HeaderMap,
    Json(body): Json<StartBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    check_csrf(&headers).map_err(|s| (s, "missing X-Veld-Request header".to_owned()))?;
    if let Some(secs) = body.duration_secs {
        if !(MIN_DURATION_SECS..=MAX_DURATION_SECS).contains(&secs) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "duration_secs must be between {MIN_DURATION_SECS} and {MAX_DURATION_SECS}, or null for no limit"
                ),
            ));
        }
    }
    // Falls back rather than refusing. The coffee button predates this feature
    // and had no database dependency at all, so a locked or unreadable DB must
    // not break it — the settings only decide the *battery lease* half, and
    // `fallback_prefs` declines that rather than guessing at it.
    let prefs = load_prefs().await.unwrap_or_else(fallback_prefs);
    let power = current_power().await;
    {
        let mut guard = ACTIVE.lock().await;
        let expires_at = body
            .duration_secs
            .map(|secs| Utc::now() + chrono::Duration::seconds(secs as i64));
        // Rolled back on failure, which the `?` alone would not do. A manual
        // reason left behind with no session is not merely untidy: it makes
        // `Reasons::is_empty()` false forever, so `reconcile`'s quiet
        // short-circuit never fires again and every tick re-attempts the spawn
        // that just failed — on a machine that has no inhibitor at all, for the
        // daemon's life. The old `start()` could not have this bug because it
        // built the child before writing any state.
        let restore = guard.state.reasons;
        guard.state.manual_start(expires_at);
        if let Err(e) = apply(&mut guard, &prefs, power).await {
            guard.state.reasons = restore;
            return Err(e);
        }
    }
    Ok(current_status().await)
}

async fn delete_stop(headers: HeaderMap) -> Result<Json<Value>, (StatusCode, String)> {
    check_csrf(&headers).map_err(|s| (s, "missing X-Veld-Request header".to_owned()))?;
    let shares = *SHARES.lock().unwrap_or_else(|e| e.into_inner());
    {
        let mut guard = ACTIVE.lock().await;
        // Idempotent: turning off something already off is a success, so two
        // windows clicking "off" don't produce an error toast in the slower one.
        // `manual_stop` is what makes it *stay* off while a share is live — the
        // automatic half would otherwise re-arm on the next tick, which is a
        // button that does not work.
        guard.state.manual_stop(shares);
        take_and_stop(&mut guard).await;
    }
    Ok(current_status().await)
}

#[cfg(test)]
mod tests {
    use super::decide::Coverage;
    use super::power::{Power, PowerSource};
    use super::{
        ACTIVE, MAX_DURATION_SECS, MIN_DURATION_SECS, Machine, inhibitor_argv, routes, status_of,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use veld_core::db::KeepAwakePrefs;

    fn prefs() -> KeepAwakePrefs {
        KeepAwakePrefs {
            sharing_on_power: true,
            sharing_on_power_minutes: 120,
            sharing_on_battery: true,
            sharing_on_battery_minutes: 30,
            manual_on_battery: true,
        }
    }

    fn mains() -> Power {
        Power {
            source: PowerSource::Mains,
            measured: true,
            has_battery: true,
        }
    }

    fn idle_status(battery_capable: bool) -> serde_json::Value {
        status_of(&Machine::default(), battery_capable, mains(), &prefs())
    }

    fn req(method: &str, csrf: bool, body: &str) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri("/api/caffeinate")
            .header("content-type", "application/json");
        if csrf {
            b = b.header("x-veld-request", "1");
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    /// Both mutating routes are gated by a hand-placed `check_csrf` line that
    /// nothing else exercises — delete either and every other test stays green,
    /// while a page on any origin gains a switch that pins the user's machine
    /// awake. `GET` is deliberately not gated: it is a read.
    #[tokio::test]
    async fn both_mutating_routes_reject_a_request_without_the_csrf_header() {
        for (method, body) in [("POST", r#"{"duration_secs":3600}"#), ("DELETE", "")] {
            let res = routes().oneshot(req(method, false, body)).await.unwrap();
            assert_eq!(
                res.status(),
                StatusCode::FORBIDDEN,
                "{method} without header"
            );
        }
    }

    /// The bound exists so a typo cannot mean something wildly different from
    /// what was meant — and it must be checked *before* anything spawns, which
    /// is what this asserts by getting a 400 rather than a started session.
    #[tokio::test]
    async fn an_out_of_range_duration_is_refused_before_anything_spawns() {
        for secs in [0u64, MIN_DURATION_SECS - 1, MAX_DURATION_SECS + 1] {
            let res = routes()
                .oneshot(req("POST", true, &format!(r#"{{"duration_secs":{secs}}}"#)))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{secs}s");
        }
        // And nothing was started by any of those. Asserted against the session
        // directly rather than through `GET /api/caffeinate`: that path probes for
        // a privileged helper, so in a unit test it would open a connection to
        // this machine's real **root** helper socket — slow, dependent on what is
        // installed, and nothing to do with what this test is about.
        assert!(ACTIVE.lock().await.session.is_none());
    }

    /// `null` is the wire form of "no time limit" and must survive the round
    /// trip as `None` rather than being rejected as a missing field.
    #[test]
    fn an_absent_or_null_duration_both_mean_no_limit() {
        let absent: super::StartBody = serde_json::from_str("{}").unwrap();
        let null: super::StartBody = serde_json::from_str(r#"{"duration_secs":null}"#).unwrap();
        assert!(absent.duration_secs.is_none());
        assert!(null.duration_secs.is_none());
    }

    /// The idle status is what the UI decides whether to *offer* the control
    /// from, so it must carry `supported` on a machine where nothing is running.
    #[test]
    fn the_idle_status_reports_support_and_no_session() {
        let v = idle_status(false);
        assert_eq!(v["active"], serde_json::json!(false));
        assert_eq!(
            v["supported"],
            serde_json::json!(inhibitor_argv(Coverage::LidToo).is_ok())
        );
        // No countdown fields on an idle status — a UI reading `remaining_secs`
        // must not find a stale one from a session that ended.
        assert!(v.get("remaining_secs").is_none());
    }

    /// The two battery fields answer different questions, and collapsing them
    /// would put the UI in the position of offering battery coverage as though
    /// it were already on, or refusing to mention it once a lease lapsed.
    #[test]
    fn battery_capability_and_battery_coverage_are_reported_separately() {
        // Capable but idle: nothing is covered, and the menu may still say the
        // machine *can* do it.
        let idle_capable = idle_status(true);
        assert_eq!(idle_capable["battery_capable"], serde_json::json!(true));
        assert_eq!(idle_capable["covers_battery"], serde_json::json!(false));

        // Not capable: both false, on every platform, with no session.
        let idle_incapable = idle_status(false);
        assert_eq!(idle_incapable["battery_capable"], serde_json::json!(false));
        assert_eq!(idle_incapable["covers_battery"], serde_json::json!(false));
    }

    /// The UI composes its own copy from `platform`, so a wrong or missing value
    /// there is what turns "on mains power only" into advice shown to a Linux
    /// user whose lid-closed sleep is already covered.
    #[test]
    fn the_status_names_the_platform_it_is_speaking_for() {
        let v = idle_status(false);
        let expected = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else {
            "other"
        };
        assert_eq!(v["platform"], serde_json::json!(expected));
    }

    /// The argv is the whole feature. A wrong flag here inhibits nothing and
    /// still reports success, so pin the load-bearing parts per platform.
    #[test]
    fn the_platform_argv_asks_for_the_right_inhibition() {
        let Ok(argv) = inhibitor_argv(Coverage::LidToo) else {
            // No inhibitor on this machine (a container without systemd); the
            // status test above already covers what the UI is told about that.
            return;
        };
        // Every platform wraps `cat`: that is what ties the inhibition's
        // lifetime to this daemon's, and dropping it would leave a process that
        // keeps the machine awake after veld is gone.
        assert_eq!(argv.last().map(String::as_str), Some("cat"));
        if cfg!(target_os = "macos") {
            assert!(argv[0].ends_with("caffeinate"), "{argv:?}");
            assert!(argv.contains(&"-s".to_owned()), "{argv:?}");
            assert!(argv.contains(&"-i".to_owned()), "{argv:?}");
        } else {
            assert!(argv[0].ends_with("systemd-inhibit"), "{argv:?}");
            // `handle-lid-switch` is the half that answers the actual question
            // ("does a closed lid still suspend?"); `sleep`/`idle` alone do not.
            assert!(
                argv.iter()
                    .any(|a| a == "--what=handle-lid-switch:sleep:idle"),
                "{argv:?}"
            );
            assert!(argv.contains(&"--mode=block".to_owned()), "{argv:?}");
        }
    }

    /// The narrow form is what an automatic hold gets on battery, and it is the
    /// whole reason default-on is defensible: veld may stop a shared machine
    /// dozing off without anybody asking, and may **not** pin a laptop open in
    /// somebody's bag. A stray `-s` or `handle-lid-switch` here would ship the
    /// second behaviour while every other test stayed green.
    #[test]
    fn the_narrow_argv_holds_idle_sleep_and_leaves_the_lid_alone() {
        let Ok(argv) = inhibitor_argv(Coverage::IdleOnly) else {
            return;
        };
        assert_eq!(argv.last().map(String::as_str), Some("cat"));
        if cfg!(target_os = "macos") {
            assert!(argv.contains(&"-i".to_owned()), "{argv:?}");
            assert!(!argv.contains(&"-s".to_owned()), "{argv:?}");
        } else {
            assert!(argv.contains(&"--what=idle".to_owned()), "{argv:?}");
            assert!(
                !argv.iter().any(|a| a.contains("handle-lid-switch")),
                "{argv:?}"
            );
            assert!(argv.contains(&"--mode=block".to_owned()), "{argv:?}");
        }
    }

    /// A status must never point somebody at `veld setup privileged` for a lease
    /// veld deliberately never asked for. This is the one that stops the existing
    /// lid note becoming a fault report on every automatic hold.
    #[tokio::test]
    async fn an_automatic_hold_reports_its_lid_gap_as_automatic_not_as_a_fault() {
        use super::decide::ShareFacts;
        use chrono::Utc;

        let battery = Power {
            source: PowerSource::Battery,
            measured: true,
            has_battery: true,
        };
        let mut machine = Machine::default();
        machine.state.recompute(
            Utc::now(),
            &prefs(),
            battery,
            ShareFacts {
                count: 1,
                latest_expiry: None,
            },
        );
        // `battery_capable: true` is the trap: a helper *is* installed, so a
        // status that only knew "capable but not covered" would report a fault.
        let status = status_of(&machine, true, battery, &prefs());
        assert_eq!(status["reason"], serde_json::json!("sharing"));
        assert_eq!(status["lid_gap"], serde_json::json!("automatic"));
        assert_eq!(status["covers_battery"], serde_json::json!(false));
    }
}
