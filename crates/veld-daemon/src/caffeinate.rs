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
//! read from the flag the renewal task owns, so it stops claiming coverage the
//! moment renewals start failing — a status that lies in the optimistic
//! direction is worse than no status, because the user planned around it.
//!
//! The lease itself is the helper's safety property, not this module's; see
//! `veld-helper`'s `sleep` module for why a durable `pmset` setting may only be
//! held on something that expires.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

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
use veld_core::helper::HelperClient;

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

/// How long a "is there a privileged helper" answer is reused.
///
/// The probe is a real round trip with a 3s ceiling, and the idle status is
/// polled by every open client on window focus — so caching is not an
/// optimisation, it is what stops a tab switch costing a socket connection. The
/// answer changes only when somebody runs `veld setup privileged`, and being a
/// minute stale about that shows up as one menu that has not caught up yet.
const PRIVILEGED_PROBE_TTL: Duration = Duration::from_secs(60);

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
const HELPER_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// The live inhibition, if there is one. At most one per machine.
struct Session {
    /// Distinguishes this session from its successor, so an expiry task that
    /// wakes after a replacement cannot tear the *new* session down.
    id: u64,
    child: Child,
    /// The write end of the wrapped `cat`'s stdin. Holding it is the inhibition;
    /// dropping it is the whole shutdown sequence.
    stdin: Option<ChildStdin>,
    started_at: DateTime<Utc>,
    /// `None` for "until I turn it off".
    expires_at: Option<DateTime<Utc>>,
    /// The task watching `expires_at`. `None` for an unlimited session.
    timer: Option<tokio::task::JoinHandle<()>>,
    /// Whether the privileged half — battery, lid closed — is in force.
    ///
    /// Shared with the renewal task rather than a plain `bool`, so a lease that
    /// starts failing downgrades what the status claims instead of leaving a
    /// promise nothing is keeping. It doubles as the renewal loop's stop flag:
    /// clearing it before releasing is what stops a renewal landing after the
    /// release and re-arming a lease nobody wants.
    battery: Arc<AtomicBool>,
    /// The privileged helper this session's lease lives on, and the task
    /// renewing it. Both `None` when there is no privileged half.
    helper: Option<HelperClient>,
    renew: Option<tokio::task::JoinHandle<()>>,
}

static ACTIVE: LazyLock<Mutex<Option<Session>>> = LazyLock::new(|| Mutex::new(None));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Cached answer to "is a privileged helper reachable", with the time it was
/// learned. See [`PRIVILEGED_PROBE_TTL`].
static PRIVILEGED: LazyLock<Mutex<Option<(Instant, bool)>>> = LazyLock::new(|| Mutex::new(None));

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
fn inhibitor_argv() -> Result<Vec<String>, String> {
    let name = program_name()?;
    let path = which_on_path(name).ok_or_else(|| {
        format!("Keeping this machine awake needs `{name}`, which isn’t on this machine.")
    })?;

    let mut argv = vec![path.to_string_lossy().into_owned()];
    if cfg!(target_os = "macos") {
        // -s: no system sleep — this is the one that survives a closed lid, and
        //     it is honoured on AC power only (see the module docs).
        // -i: no idle sleep — the one that keeps a locked screen from putting
        //     the machine under while a build runs.
        // Display sleep is deliberately *not* held: the screen may blank and
        // lock, which is what somebody walking away from the machine wants.
        argv.push("-s".to_owned());
        argv.push("-i".to_owned());
    } else {
        // `handle-lid-switch` is what makes a closed lid a no-op; `sleep` covers
        // an explicit suspend request and `idle` covers logind's idle action.
        argv.push("--what=handle-lid-switch:sleep:idle".to_owned());
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
    if !cfg!(target_os = "macos") {
        return false;
    }
    if let Some((learned, answer)) = *PRIVILEGED.lock().await {
        if learned.elapsed() < PRIVILEGED_PROBE_TTL {
            return answer;
        }
    }
    let answer = HelperClient::connect_privileged().await.is_ok();
    *PRIVILEGED.lock().await = Some((Instant::now(), answer));
    answer
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
            warn!(error = %e, "could not hold battery sleep; keeping the machine awake on mains power only");
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
    loop {
        tokio::time::sleep(HELPER_RENEW_EVERY).await;
        // Checked before every renewal so a teardown that has already cleared
        // the flag cannot be followed by a renewal that re-arms the lease.
        if !battery.load(Ordering::Relaxed) {
            return;
        }
        if let Err(e) = client.hold_sleep_disabled(HELPER_LEASE_SECS).await {
            // One failure is not fatal — the lease outlives three renewals — but
            // the status must stop claiming coverage, because the next thing
            // that happens if this keeps failing is the helper reverting.
            warn!(error = %e, "battery sleep lease renewal failed");
            battery.store(false, Ordering::Relaxed);
            return;
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

/// Stop whatever is in `guard`, aborting its expiry task. For the callers that
/// are *not* the expiry task.
async fn take_and_stop(guard: &mut Option<Session>) {
    if let Some(mut session) = guard.take() {
        if let Some(timer) = session.timer.take() {
            timer.abort();
        }
        stop_session(session).await;
    }
}

/// Start (or replace) the inhibition. `duration_secs` of `None` means no limit.
async fn start(duration_secs: Option<u64>) -> Result<Value, (StatusCode, String)> {
    let argv = inhibitor_argv().map_err(|e| (StatusCode::NOT_IMPLEMENTED, e))?;

    let mut guard = ACTIVE.lock().await;
    // Replace rather than refuse. Changing the answer is the menu's whole job,
    // and an off-then-on round trip would leave a window in which the machine
    // could suspend between the two requests.
    take_and_stop(&mut guard).await;

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

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let started_at = Utc::now();
    let expires_at = duration_secs.map(|secs| started_at + chrono::Duration::seconds(secs as i64));
    let timer = expires_at.map(|deadline| tokio::spawn(expire_at(id, deadline)));

    // The privileged half, after the unprivileged one is already holding: this
    // can only ever *add* battery/lid-closed coverage, so its failure must not
    // cost the caller the hold they asked for.
    let helper = acquire_battery_lease().await;
    let battery = Arc::new(AtomicBool::new(helper.is_some()));
    let renew = helper
        .clone()
        .map(|client| tokio::spawn(renew_battery_lease(client, Arc::clone(&battery))));

    info!(
        seconds = ?duration_secs,
        battery = battery.load(Ordering::Relaxed),
        "keeping this machine awake"
    );
    *guard = Some(Session {
        id,
        child,
        stdin,
        started_at,
        expires_at,
        timer,
        battery,
        helper,
        renew,
    });
    // Cached by now — `acquire_battery_lease` just asked.
    let capable = battery_capable().await;
    Ok(status_of(guard.as_ref(), capable))
}

/// Watch the wall clock and stop session `id` once `deadline` passes.
async fn expire_at(id: u64, deadline: DateTime<Utc>) {
    loop {
        let remaining = deadline - Utc::now();
        if remaining <= chrono::Duration::zero() {
            break;
        }
        let nap = remaining.to_std().unwrap_or(EXPIRY_TICK).min(EXPIRY_TICK);
        tokio::time::sleep(nap).await;
    }
    let mut guard = ACTIVE.lock().await;
    // Only if this is still *our* session: a replacement started while we slept
    // owns the machine now, and tearing it down would silently shorten it.
    if guard.as_ref().is_some_and(|s| s.id == id) {
        if let Some(session) = guard.take() {
            stop_session(session).await;
            info!("keep-awake expired; this machine can sleep again");
        }
    }
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
fn status_of(session: Option<&Session>, battery_capable: bool) -> Value {
    let unsupported = inhibitor_argv().err();
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
        // Whether it is *in force* for the live session. Read from the flag the
        // renewal task owns, so a lease that started failing stops being claimed.
        "covers_battery": session.is_some_and(|s| s.battery.load(Ordering::Relaxed)),
    });
    if let Some(s) = session {
        out["started_at"] = json!(s.started_at.to_rfc3339());
        out["expires_at"] = json!(s.expires_at.map(|t| t.to_rfc3339()));
        // Clamped at zero: between the deadline passing and the expiry task
        // taking the lock there is a moment where this would be negative, and a
        // negative "remaining" renders as a countdown running backwards.
        out["remaining_secs"] = json!(s.expires_at.map(|t| (t - Utc::now()).num_seconds().max(0)));
    }
    out
}

async fn get_state() -> Json<Value> {
    // Learned before the lock, not under it: the probe can take up to 3s on a
    // wedged helper, and holding the session lock for that would stall a
    // concurrent start or stop behind a read.
    let capable = battery_capable().await;
    let guard = ACTIVE.lock().await;
    Json(status_of(guard.as_ref(), capable))
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
    Ok(Json(start(body.duration_secs).await?))
}

async fn delete_stop(headers: HeaderMap) -> Result<Json<Value>, (StatusCode, String)> {
    check_csrf(&headers).map_err(|s| (s, "missing X-Veld-Request header".to_owned()))?;
    let mut guard = ACTIVE.lock().await;
    // Idempotent: turning off something already off is a success, so two windows
    // clicking "off" don't produce an error toast in the slower one.
    take_and_stop(&mut guard).await;
    Ok(Json(status_of(None, battery_capable().await)))
}

#[cfg(test)]
mod tests {
    use super::{MAX_DURATION_SECS, MIN_DURATION_SECS, inhibitor_argv, routes, status_of};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

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
        // And nothing was started by any of those.
        let res = routes().oneshot(req("GET", false, "")).await.unwrap();
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["active"], serde_json::json!(false));
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
        let v = status_of(None, false);
        assert_eq!(v["active"], serde_json::json!(false));
        assert_eq!(v["supported"], serde_json::json!(inhibitor_argv().is_ok()));
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
        let idle_capable = status_of(None, true);
        assert_eq!(idle_capable["battery_capable"], serde_json::json!(true));
        assert_eq!(idle_capable["covers_battery"], serde_json::json!(false));

        // Not capable: both false, on every platform, with no session.
        let idle_incapable = status_of(None, false);
        assert_eq!(idle_incapable["battery_capable"], serde_json::json!(false));
        assert_eq!(idle_incapable["covers_battery"], serde_json::json!(false));
    }

    /// The UI composes its own copy from `platform`, so a wrong or missing value
    /// there is what turns "on mains power only" into advice shown to a Linux
    /// user whose lid-closed sleep is already covered.
    #[test]
    fn the_status_names_the_platform_it_is_speaking_for() {
        let v = status_of(None, false);
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
        let Ok(argv) = inhibitor_argv() else {
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
}
