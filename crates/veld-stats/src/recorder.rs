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
/// Install it on the orchestrator with `Orchestrator::with_step_observer`, which
/// takes an `Arc` and clones it into every node execution context — so sampling
/// stops when the **last** of those goes, i.e. when the orchestrator is dropped
/// at the end of the command. Do not hand a caller a clone and tell them
/// dropping it stops sampling; it doesn't. No cleanup is owed either way,
/// because nothing was persisted.
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

/// Take the roots map, recovering it if a previous holder panicked.
///
/// Poisoning is the wrong failure mode for this map: every critical section is a
/// single `insert` or `remove` on a plain `HashMap`, so a panic cannot leave it
/// logically half-written, and a mutex never un-poisons. Treating a poisoned
/// lock as unusable would end sampling for the rest of the run — silently, since
/// the symptom downstream is a gap in a chart that looks like idle time.
fn lock_roots(
    roots: &Mutex<HashMap<String, u32>>,
) -> std::sync::MutexGuard<'_, HashMap<String, u32>> {
    roots.lock().unwrap_or_else(|e| e.into_inner())
}

impl StepObserver for CommandStatsRecorder {
    fn step_started(&self, node_key: &str, pid: u32) {
        lock_roots(&self.roots).insert(node_key.to_owned(), pid);
        self.wake.notify_one();
    }

    fn step_finished(&self, node_key: &str) {
        lock_roots(&self.roots).remove(node_key);
    }
}

/// Two passes are never taken closer together than this; a wake that lands
/// inside the gap is **delayed to its end, not dropped**.
///
/// `sysinfo` refuses to recompute the CPU denominator inside its own
/// `MINIMUM_CPU_UPDATE_INTERVAL` (200ms) — on Linux it keeps the previous
/// `/proc/stat` pair, on macOS it reuses `prev_time_interval` — so a pass taken
/// right after another reports a CPU figure understated by roughly the ratio of
/// the two gaps. A whole stage of parallel `command` steps registers within
/// milliseconds of each other, so without this every parallel stage paid for a
/// second machine-wide process scan to produce that.
///
/// **Delaying rather than skipping is the whole point, and this was got wrong
/// once.** Skipping cost a step shorter than one interval its only sample: the
/// wake is what guarantees a 1-second `npm ci` appears at all, and a version of
/// this that `continue`d instead of sleeping silently dropped such steps from
/// the data entirely. Coverage of a short step beats the CPU precision of a
/// long one.
const MIN_RESAMPLE_GAP: Duration = Duration::from_millis(500);

/// The sampling task: one [`StatsCollector`] for the whole run, one pass per
/// tick over whatever steps are registered at that moment.
///
/// The collector is persistent because `sysinfo` derives CPU usage from the
/// delta between two refreshes of the same process — a fresh collector per pass
/// would report 0% forever. It is moved in and out of `spawn_blocking` rather
/// than borrowed, because a pass is genuinely blocking work (see below) and the
/// collector must still outlive it.
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
    let mut last_pass: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = wake.notified() => {
                // Re-phase the schedule to the sample about to be taken, so the
                // next tick is a full interval away rather than whatever was
                // left of the current one.
                interval.reset();
            }
        }
        if let Some(earliest) = last_pass.map(|t| t + MIN_RESAMPLE_GAP) {
            if earliest > tokio::time::Instant::now() {
                tokio::time::sleep_until(earliest).await;
            }
        }
        last_pass = Some(tokio::time::Instant::now());

        // A pass is blocking work on two counts: a machine-wide `sysinfo`
        // refresh plus a per-process memory probe, and a synchronous rusqlite
        // transaction. The second matters most — every `Db` clone shares one
        // `Arc<Mutex<Connection>>` (see `veld_core::db::Db`), and this process's
        // other user of it is the orchestrator's per-node spawn checkpoint. Left
        // on a runtime worker, a 2s sampler would park that worker and hold that
        // mutex during exactly the phase the checkpoint path is busiest.
        let (db2, root2, name2, roots2) = (
            db.clone(),
            project_root.clone(),
            run_name.clone(),
            Arc::clone(&roots),
        );
        collector = match tokio::task::spawn_blocking(move || {
            let sampled = sample_pass(&db2, &root2, &name2, &roots2, &mut collector);
            (collector, sampled)
        })
        .await
        {
            // A pass that found nothing registered never touched the process
            // table, so it must not start the rate-limit clock — otherwise the
            // idle tick that happens to precede a step's registration delays
            // that step's first sample for no reason.
            Ok((c, sampled)) => {
                if !sampled {
                    last_pass = None;
                }
                c
            }
            // The pass panicked and took the collector with it. Sampling
            // continues with a fresh one; its first CPU reading is 0%, as after
            // any first refresh.
            Err(e) => {
                debug!("command-step stats pass failed for '{run_name}': {e}");
                StatsCollector::new()
            }
        };
    }
}

/// One pass. Observational only: a write failure is logged and the next tick
/// tries again — a run must never fail because its memory graph didn't.
///
/// Returns whether the process table was actually refreshed, which is what the
/// caller's rate limiter must be measured from.
fn sample_pass(
    db: &Db,
    project_root: &std::path::Path,
    run_name: &str,
    roots: &Mutex<HashMap<String, u32>>,
    collector: &mut StatsCollector,
) -> bool {
    // Copy out under the lock: the refresh below is a machine-wide scan and the
    // orchestrator registers steps from other tasks while it runs.
    let current: Vec<(String, u32)> = lock_roots(roots)
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    // Nothing registered — skip the process-table refresh entirely. Between
    // stages, and for a run made only of `start_server` nodes, this is every
    // tick, and a scan of every process on the machine is not free.
    if current.is_empty() {
        return false;
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
        let Some(tree) = collector.sample_tree(pid, sampled_at) else {
            continue;
        };
        // A tree that reports no memory at all is a **zombie root**: the step's
        // process has exited but the CLI has not reached its `wait()` yet, so it
        // is still in the process table with nothing mapped. A live process
        // always has resident pages — `samples_this_process_with_real_platform_detail`
        // asserts exactly that. Recording it would write the "used no memory"
        // sample the paragraph above exists to prevent, at the end of every
        // build, which is the worst possible place for it.
        if tree.total.memory_bytes == 0 {
            continue;
        }
        samples.push((key.clone(), tree.total));
        trees.push((key, tree.processes));
    }
    if samples.is_empty() {
        // The refresh still happened, so the rate limit still applies.
        return true;
    }
    if let Err(e) = db.record_node_stats(project_root, run_name, &samples, &trees) {
        debug!("could not record command-step stats for run '{run_name}': {e}");
    }
    true
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

    /// **A step shorter than the tick interval must still be recorded.** This is
    /// what the wake-up exists for, and it is the case a review fix broke once:
    /// rate-limiting the wake by *skipping* it, rather than delaying it, dropped
    /// every short step from the data — invisibly, because a missing sample and
    /// a step that used no memory look identical downstream.
    ///
    /// The step here lives well under `COMMAND_SAMPLE_INTERVAL_SECS`, and a
    /// second step registers immediately after it so the rate limiter is
    /// actually engaged, exactly as a parallel stage would.
    #[tokio::test]
    async fn a_step_shorter_than_the_interval_is_still_sampled() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open_at(&tmp.path().join("veld.db")).unwrap();
        let project_root = tmp.path().join("proj");
        db.save_run(&project_root, "proj", &RunState::new("dev", "proj"))
            .unwrap();

        let mut sibling = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 20")
            .spawn()
            .unwrap();
        let recorder =
            CommandStatsRecorder::start(db.clone(), project_root.clone(), "dev".to_owned());

        // A long step registers first and is sampled, which starts the
        // rate-limit clock. This is the ordinary shape of a stage, and it is
        // what makes the next registration land inside the window.
        recorder.step_started("slow:local", sibling.id().unwrap());
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            db.latest_node_stats(&project_root, "dev")
                .unwrap()
                .contains_key("slow:local"),
            "precondition: the first step must have been sampled, or this test \
             is not exercising the rate limiter at all"
        );

        // Now a step far shorter than the tick interval, registering inside the
        // rate-limit window. It must still be sampled — delayed to the end of
        // the window, never skipped to the next tick, which it would not live
        // to see.
        let mut short = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 1")
            .spawn()
            .unwrap();
        recorder.step_started("quick:local", short.id().unwrap());
        tokio::time::sleep(Duration::from_millis(900)).await;
        let _ = short.kill().await;

        let latest = db.latest_node_stats(&project_root, "dev").unwrap();
        assert!(
            latest.contains_key("quick:local"),
            "a sub-interval step registering inside the rate-limit window must \
             still produce a sample; got keys {:?}",
            latest.keys().collect::<Vec<_>>()
        );
        let _ = sibling.kill().await;
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
