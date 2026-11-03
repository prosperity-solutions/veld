use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tracing::info;

#[path = "management.rs"]
pub mod management;

#[path = "engine.rs"]
pub(crate) mod engine;

#[path = "engine_query.rs"]
pub(crate) mod engine_query;

/// The in-memory record of which coding agents are working or waiting.
#[path = "agent_overview.rs"]
pub(crate) mod agent_overview;

#[path = "desktop.rs"]
mod desktop;

/// Running the commands `ide.extensions` declares.
#[path = "extensions.rs"]
mod extensions;

/// Listing the sessions a config-declared pane could adopt.
#[path = "pane_sessions.rs"]
mod pane_sessions;

#[path = "worktree_trash.rs"]
pub mod worktree_trash;

#[path = "pty.rs"]
// `pub(crate)` only so the periodic GC can call `pty::prune_pastes_now` — the
// pasted-file directory is the pty module's, and nothing else reaches into it.
pub(crate) mod pty;

/// The IDE control channel: worktree claims, focus, and the pane-layout store.
#[path = "ide.rs"]
mod ide;

#[path = "settings.rs"]
mod settings;

#[path = "config_vars.rs"]
mod config_vars;

#[path = "config_cache.rs"]
pub(crate) mod config_cache;

#[path = "repo_fingerprint.rs"]
mod repo_fingerprint;

#[path = "promotions.rs"]
mod promotions;

/// Local files served to a browser pane, on an origin of their own.
#[path = "files.rs"]
// `pub(crate)` so `main.rs` can start the second listener, and the `open` shim's
// route can ask whether file serving is up before promising a pane.
pub(crate) mod files;

/// The file pane's text reads, its grants outside the worktree, and `veld ide open`.
#[path = "file_pane.rs"]
mod file_pane;

/// What a worktree changed against its merge-base, for review.
#[path = "changes.rs"]
mod changes;

/// outfit profiles for new worktrees: whether they are in play, and pinning one.
#[path = "outfit.rs"]
mod outfit;

/// Serialises the tests that point `VELD_DB_PATH` at a temp database.
///
/// **Pre-existing flake, fixed here because it reddens CI at random.** Two modules
/// under this one — `settings` and `extensions` — each set and then remove that
/// process-wide variable around an HTTP-layer test, and each carries a SAFETY note
/// asserting the process is single-threaded. It is not: `cargo test` runs them on a
/// thread pool, so `settings`' `remove_var` lands inside `extensions`' request and
/// the handler opens a different database than the one the test seeded. The symptom
/// is a 404 where the test expects 200, on a file neither author touched. Measured
/// at roughly one full-suite run in four on `origin/main`.
///
/// **Take it exactly once per test, and bind it.** The mutex is not reentrant, so a
/// second `lock()` in the same test deadlocks — and `let _ = ...` drops the guard
/// immediately (releasing it before the body runs) where `let _guard = ...` holds it
/// to the end of scope. Both mistakes have been made in this repo before.
///
/// Poisoning is ignored: a panicking test has already failed, and refusing the lock
/// afterwards would convert one failure into every later test failing too.
#[cfg(test)]
pub(crate) static DB_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn lock_db_env() -> std::sync::MutexGuard<'static, ()> {
    DB_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[path = "caffeinate/mod.rs"]
pub(crate) mod caffeinate;

/// Note the terminal sessions being left running.
///
/// Re-exported for the daemon's shutdown path. It no longer *ends* anything: a
/// session's PTY belongs to a holder process, so the shells survive a restart —
/// which is what makes `veld self update` safe to run with terminals open. See
/// `pty::shutdown_sessions`.
pub async fn shutdown_terminal_sessions() {
    pty::shutdown_sessions().await;
}

/// Keep the ports the dashboard is served on current, for the `Origin` allowlist
/// the terminal and IDE-channel upgrades share.
///
/// Re-exported for `main`, which spawns it beside the other background timers. It
/// is a timer rather than a startup read because `veld self setup` restarts this daemon
/// *before* it installs the helper and *before* it writes the new mode — so a
/// fresh daemon's first look finds neither, and an allowlist stuck on that answer
/// refuses every upgrade the dashboard makes, which a browser cannot report. See
/// `pty::track_helper_ports`.
pub async fn track_dashboard_ports() {
    pty::track_helper_ports().await;
}

/// Serve one terminal session as a holder process (`veld-daemon --pty-holder`).
///
/// Re-exported for `main`, which dispatches to it before any of the daemon's own
/// startup: a holder binds no port, opens no database, and must not be mistaken
/// for a second daemon.
pub async fn run_pty_holder() -> anyhow::Result<()> {
    pty::holder::run_from_stdin().await
}

// The daemon's HTTP server listens on this instance's daemon port —
// `veld_core::instance::daemon_port()` (19899 for the installed instance;
// a dev instance overrides via VELD_DAEMON_PORT).

/// Start the daemon's HTTP server on 127.0.0.1 at this instance's daemon
/// port (`veld_core::instance::daemon_port()`).
pub async fn run_http_server(
    share_manager: Arc<crate::share::manager::ShareManager>,
) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], veld_core::instance::daemon_port()));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot own daemon API listener {addr}"))?;
    let app = Router::new()
        // Management UI (served at veld.localhost via Caddy, also reachable
        // directly on this port for debugging).
        .merge(management::routes())
        .merge(engine::routes())
        .merge(engine_query::routes())
        .merge(desktop::routes())
        // User preferences. Kept out of desktop::routes() because settings are
        // not desktop-specific (runs mode reads them too) and that router's
        // blanket CSRF layer only covers routes registered before it.
        .merge(settings::routes())
        // Machine-overridable vars. Same reasoning as settings — not
        // desktop-specific, and it needs its own CSRF checks rather than that
        // router's blanket layer.
        .merge(config_vars::routes())
        // Feature promotions. Same reasoning as settings — not desktop-specific,
        // and its handlers call `check_csrf` themselves rather than relying on
        // that router's blanket layer.
        .merge(promotions::routes())
        // What local files are worth opening, and whether a watched one changed.
        // Only the *questions* are here — the bytes are served on their own origin
        // (see files.rs), and these two are same-origin precisely so the `/ide`
        // bundle can read their bodies.
        //
        // Both check CSRF despite being GETs, which is the exception to the rule
        // elsewhere in this file. This comment used to claim they had no side effects;
        // that was wrong twice over — the listing mints and persists a grant, and it
        // spends a bounded but real amount of disk walking per call. See `list_viewable`.
        .merge(files::api_routes())
        // The file pane's reads and `veld ide open`, then the changed-files list.
        // Every handler checks CSRF itself, the GETs included, for the reason given
        // for `files::api_routes` just above.
        .merge(file_pane::routes())
        .merge(changes::routes())
        // Keep-awake. Same reasoning as settings — machine-wide rather than
        // desktop-specific, and its mutating handlers call `check_csrf`
        // themselves rather than relying on that router's blanket layer.
        .merge(caffeinate::routes())
        // Terminal sockets. Kept out of desktop::routes() because that
        // router's CSRF layer cannot gate a WebSocket upgrade — see pty.rs.
        .merge(pty::routes())
        // Worktree claims, focus requests and pane layouts. Out of
        // desktop::routes() for the same reason pty is: its load-bearing route
        // is a WebSocket upgrade, which that router's CSRF layer cannot gate.
        .merge(ide::routes())
        .merge(crate::share::api::routes(share_manager));

    // Terminal sessions outlive their socket (so a page reload keeps its
    // shell), which means something has to collect the ones nobody comes back
    // for. Started here rather than in pty::routes() so that building a router
    // in a test doesn't leave a timer running.
    // Holders whose daemon went away are still serving their shells; this is
    // where a fresh daemon takes them back. Before the reaper, so an adopted
    // session's detach clock is running by the time anything can collect it, and
    // before the listener accepts requests, so no attach races partial adoption.
    pty::adopt_existing_sessions().await;
    pty::spawn_session_reaper();
    // The `$BROWSER` shims every new session's environment points at. Here rather
    // than on the first terminal so a machine that cannot have them says so in the
    // startup log, and so `$VELD_SHIM_DIR` is usable from the first shell.
    pty::prepare_shims();
    // The worker that empties the worktree trash. Starting it also runs the retention
    // sweep once — which is the *whole* of what a restart resumes, deliberately: the
    // trash flag is not a work queue, and reading it as one is what used to delete
    // every binned worktree on every restart. Started for the same reason the reaper
    // is (not inside `routes()`), so building a router in a test starts no worker.
    worktree_trash::spawn();

    info!("http server listening on {addr}");
    pty::clear_unbacked_shims();
    let database = veld_core::db::Db::default_path()?;
    let load_path = database.clone();
    let config =
        crate::offload::blocking(move || veld_core::runner::load_config(&load_path)).await?;
    if let Some(config) = config {
        // Ingress must never outlive this owned listener and reach a replacement daemon.
        tokio::select! {
            result = axum::serve(listener, app) => result.context("daemon API stopped"),
            result = crate::remote::run(database, config) => result.context("runner ingress stopped"),
        }
    } else {
        axum::serve(listener, app)
            .await
            .context("daemon API stopped")
    }
}
