//! Resource sampling for the `command` steps a run executes while starting.
//!
//! Builds, installs and codegen are the most expensive part of a run and were
//! the one part nothing observed: a `command` step's process is spawned by the
//! CLI, awaited, and forgotten, so no PID for it exists anywhere outside the
//! task that owns it. The daemon's sampler reads PIDs out of the database and
//! therefore cannot see one.
//!
//! The fix is not to publish the PID — it is to let the process that already
//! holds it do the measuring. A [`CommandStatsRecorder`] lives for the duration
//! of one run inside the CLI, learns each step's root PID as it spawns
//! ([`StepObserver`]), samples the trees it has been told about, and writes them
//! to the same `node_stats` / `node_process_stats` tables under the same
//! `node_key` the daemon uses for `start_server` nodes. Nothing about the
//! transient PID is persisted.
//!
//! That distinction is the whole design. A stats row is a *time series* — "at
//! time T, node K's tree looked like this" — a historical fact that is still
//! true afterwards. `NodeState.pid` is a claim about the *present*, read by
//! `veld stop`, the health monitor, the GC and the orphan reaper; a finished
//! build's PID sitting there would make a run that is still legitimately coming
//! up look like one that spawned and died, and would eventually have `veld stop`
//! signal whatever recycled the number.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::debug;
use veld_core::db::Db;
use veld_core::stats::StepObserver;

use crate::StatsCollector;

/// Interval between samples of a running `command` step, in seconds.
///
/// Deliberately shorter than the daemon's 5s node cadence. A long-lived dev
/// server is sampled for hours and its shape survives a coarse tick; a build is
/// over in seconds-to-minutes and its *peak* is the number a reader came for, so
/// resolution matters more here and there are far fewer trees to walk. It is
/// still a sampler: a step shorter than one interval is represented by the
/// single sample taken when it spawned, and a peak between two ticks is not
/// seen.
pub const COMMAND_SAMPLE_INTERVAL_SECS: u64 = 2;

/// Samples the `command` steps of one run, for as long as it is kept alive.
///
/// Install it on the orchestrator with `Orchestrator::with_step_observer`. Drop
/// it to stop sampling — the background task is aborted, which is also what
/// happens if the CLI exits, and no cleanup is owed because nothing was
/// persisted.
pub struct CommandStatsRecorder {
    /// `node_key` → root PID of the step currently running for that node.
    ///
    /// A plain `std::sync::Mutex`: every critical section is a map insert or
    /// remove, so it is never held across an `.await`, and [`StepObserver`] is a
    /// synchronous trait callable from the orchestrator's spawn path.
    roots: Arc<Mutex<HashMap<String, u32>>>,
    /// Rings the sampler between ticks, so a step that spawns just after a tick
    /// still gets one sample instead of waiting a whole interval — which for a
    /// short step is the difference between a data point and none.
    wake: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl CommandStatsRecorder {
    /// Start sampling for `run_name` in `project_root`. Requires a Tokio
    /// runtime; the sampling task is spawned immediately and idles (without
    /// touching the process table) until the first step registers.
    pub fn start(db: Db, project_root: std::path::PathBuf, run_name: String) -> Self {
        let roots: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(sample_loop(
            db,
            project_root,
            run_name,
            Arc::clone(&roots),
            Arc::clone(&wake),
        ));
        Self { roots, wake, task }
    }
}

impl Drop for CommandStatsRecorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl StepObserver for CommandStatsRecorder {
    fn step_started(&self, node_key: &str, pid: u32) {
        if let Ok(mut roots) = self.roots.lock() {
            roots.insert(node_key.to_owned(), pid);
        }
        self.wake.notify_one();
    }

    fn step_finished(&self, node_key: &str) {
        if let Ok(mut roots) = self.roots.lock() {
            roots.remove(node_key);
        }
    }
}

/// The sampling task: one [`StatsCollector`] for the whole run, one pass per
/// tick over whatever steps are registered at that moment.
///
/// The collector is persistent because `sysinfo` derives CPU usage from the
/// delta between two refreshes of the same process — a fresh collector per pass
/// would report 0% forever.
async fn sample_loop(
    db: Db,
    project_root: std::path::PathBuf,
    run_name: String,
    roots: Arc<Mutex<HashMap<String, u32>>>,
    wake: Arc<tokio::sync::Notify>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(COMMAND_SAMPLE_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut collector = StatsCollector::new();

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = wake.notified() => {}
        }
        sample_pass(&db, &project_root, &run_name, &roots, &mut collector);
    }
}

/// One pass. Observational only: a write failure is logged and the next tick
/// tries again — a run must never fail because its memory graph didn't.
fn sample_pass(
    db: &Db,
    project_root: &std::path::Path,
    run_name: &str,
    roots: &Mutex<HashMap<String, u32>>,
    collector: &mut StatsCollector,
) {
    // Copy out under the lock: the refresh below is a machine-wide scan and the
    // orchestrator registers steps from other tasks while it runs.
    let current: Vec<(String, u32)> = match roots.lock() {
        Ok(r) => r.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        Err(_) => return,
    };
    // Nothing registered — skip the process-table refresh entirely. Between
    // stages, and for a run made only of `start_server` nodes, this is every
    // tick, and a scan of every process on the machine is not free.
    if current.is_empty() {
        return;
    }

    collector.refresh();
    let sampled_at = chrono::Utc::now();
    let mut samples = Vec::new();
    let mut trees = Vec::new();
    for (key, pid) in current {
        // `None` means the root is already gone: the step finished between the
        // refresh and now, or exited before its first sample. Not an error, and
        // deliberately not recorded as a zero — an absent sample is a gap, and
        // "wasn't sampling" must not render like "used no memory".
        if let Some(tree) = collector.sample_tree(pid, sampled_at) {
            samples.push((key.clone(), tree.total));
            trees.push((key, tree.processes));
        }
    }
    if samples.is_empty() {
        return;
    }
    if let Err(e) = db.record_node_stats(project_root, run_name, &samples, &trees) {
        debug!("could not record command-step stats for run '{run_name}': {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::state::RunState;

    /// The whole CLI-side path against a real process and a real database: a
    /// registered step is sampled, written, and readable by everything that
    /// reads node stats — and deregistering it stops the samples.
    #[tokio::test]
    async fn samples_a_registered_step_and_stops_when_it_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        // `open_at`, never `Db::open()`: a test must not touch the real database.
        let db = Db::open_at(&tmp.path().join("veld.db")).unwrap();
        let project_root = tmp.path().join("proj");

        // The run row has to exist for samples to be attributable — the
        // orchestrator persists it before the first stage, which is exactly what
        // makes start-phase sampling possible at all.
        let run = RunState::new("dev", "proj");
        db.save_run(&project_root, "proj", &run).unwrap();

        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a stand-in build");
        let pid = child.id().expect("child has a pid");

        let recorder =
            CommandStatsRecorder::start(db.clone(), project_root.clone(), "dev".to_owned());
        recorder.step_started("build:local", pid);

        // Long enough for the immediate wake-up sample plus at least one tick.
        tokio::time::sleep(Duration::from_secs(COMMAND_SAMPLE_INTERVAL_SECS + 1)).await;

        let latest = db.latest_node_stats(&project_root, "dev").unwrap();
        let s = latest
            .get("build:local")
            .expect("a command step's tree was recorded under its node key");
        assert!(s.memory_bytes > 0, "a live step reports memory");
        assert!(s.process_count >= 1);
        let first_seen = s.sampled_at;

        let tree = db
            .latest_process_tree(&project_root, "dev", "build:local")
            .unwrap();
        assert_eq!(tree[0].pid, pid, "the step's own process roots the tree");

        // Deregistering stops it, even though the process is still alive: the
        // recorder samples what it was told about, never what it can find.
        recorder.step_finished("build:local");
        let _ = child.kill().await;
        tokio::time::sleep(Duration::from_secs(COMMAND_SAMPLE_INTERVAL_SECS + 1)).await;
        let after = db.latest_node_stats(&project_root, "dev").unwrap();
        assert_eq!(
            after.get("build:local").unwrap().sampled_at,
            first_seen,
            "no further samples after the step finished"
        );
    }

    /// Dropping the recorder must stop the task; otherwise a `veld stop` or a
    /// failed run would leave a sampler writing rows for a run that has ended.
    #[tokio::test]
    async fn dropping_the_recorder_stops_sampling() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_at(&tmp.path().join("veld.db")).unwrap();
        let project_root = tmp.path().join("proj");
        let run = RunState::new("dev", "proj");
        db.save_run(&project_root, "proj", &run).unwrap();

        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();

        {
            let recorder =
                CommandStatsRecorder::start(db.clone(), project_root.clone(), "dev".to_owned());
            recorder.step_started("build:local", pid);
            tokio::time::sleep(Duration::from_secs(COMMAND_SAMPLE_INTERVAL_SECS + 1)).await;
        }
        let at_drop = db
            .latest_node_stats(&project_root, "dev")
            .unwrap()
            .get("build:local")
            .expect("sampled while the recorder was alive")
            .sampled_at;

        tokio::time::sleep(Duration::from_secs(COMMAND_SAMPLE_INTERVAL_SECS + 1)).await;
        assert_eq!(
            db.latest_node_stats(&project_root, "dev")
                .unwrap()
                .get("build:local")
                .unwrap()
                .sampled_at,
            at_drop,
            "the sampling task did not outlive the recorder"
        );
        let _ = child.kill().await;
    }
}
