//! What the engine needs from whoever hosts it.
//!
//! The engine has no HTTP code and no idea a daemon exists. A few side effects
//! live outside it — today only peer/web shares, which are held in the daemon's
//! memory — and the host does them on the engine's behalf. The engine worker's
//! host talks to its daemon; a test's host records the call.

use std::future::Future;
use std::pin::Pin;

/// How releasing a run's shares went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareRelease {
    /// This many shares were released (zero is the common case).
    Released(usize),
    /// Nothing holds shares: no daemon answers, so there are none to release.
    NotRunning,
    Failed(String),
    TimedOut,
}

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait EngineHost: Send + Sync {
    /// Release every share of `run_id`. Bounded by the host (a few seconds): it
    /// sits between a stop and a start during a restart, so it must not wedge.
    fn release_shares<'a>(&'a self, run_id: &'a str) -> HostFuture<'a, ShareRelease>;

    /// Whether the face has asked to cancel. A cancel is SIGINT to the worker,
    /// which the engine also watches with `tokio::signal::ctrl_c` while a start
    /// runs — but a signal that arrives before that listener exists, or during a
    /// restart's stop phase, is only remembered here.
    fn cancel_requested(&self) -> bool {
        false
    }

    /// A recorder for the CPU/memory of a run's `command` steps while they
    /// execute, if the host samples processes. Kept out of the engine so it does
    /// not depend on a process sampler.
    fn command_stats_observer(
        &self,
        _db: &veld_core::db::Db,
        _project_root: &std::path::Path,
        _run: &str,
    ) -> Option<std::sync::Arc<dyn veld_core::stats::StepObserver>> {
        None
    }
}

/// The notice a face shows for a release, or `None` when there is nothing to
/// say. `whose` is "the replaced run" / "the previous run" / "the stopped run".
pub fn share_release_notice(release: &ShareRelease, whose: &str, now: &str) -> Option<String> {
    match release {
        ShareRelease::Released(0) | ShareRelease::NotRunning => None,
        ShareRelease::Released(n) => Some(format!(
            "Released {n} share(s) of {whose} — re-share if you still need the URL."
        )),
        ShareRelease::Failed(e) => Some(format!(
            "Could not release {whose}'s shares ({e}) — any share of it now points at {now}. \
             Check `veld network list` and stop it with `veld network unshare <id>`."
        )),
        ShareRelease::TimedOut => Some(format!(
            "Timed out releasing {whose}'s shares — any share of it now points at {now}. \
             Check `veld network list` and stop it with `veld network unshare <id>`."
        )),
    }
}
