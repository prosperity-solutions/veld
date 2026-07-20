//! Cross-platform process resource sampling for run nodes.
//!
//! Wraps [`sysinfo`] so the daemon's stats sampler ([`run_stats_sampler`]) can
//! record CPU/memory for each run node once per tick. A node's figure is
//! summed over its process and its
//! descendants — the tree reachable by walking parent→child links from the
//! node's PID — so a `npm run dev` that forks a bundler and a dev server
//! reports one combined number rather than just the shell's. A descendant that
//! reparents away (e.g. a daemonizing double-fork adopted by init/launchd)
//! leaves the tree and is not counted; the parent-walk is used rather than
//! process-group membership because `sysinfo` exposes parent links but not
//! pgids, and it never overcounts a foreground node that shares the CLI's
//! group.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tracing::{debug, warn};
use veld_core::db::Db;
use veld_core::state::RunStatus;
use veld_core::stats::ProcessStats;

/// Interval between resource-stats samples (seconds). Kept at/under
/// `veld_core::stats::STALE_AFTER_SECS` so a healthy sampler always refreshes a
/// node's stats before its last sample ages out.
const SAMPLE_INTERVAL_SECS: u64 = 5;

/// Periodically sample CPU/memory for every running run's node process trees
/// and append them to the `node_stats` table. Runs as its own daemon task,
/// separate from the health monitor, so slow liveness probes there never delay
/// sampling (which would make live stats read as stale).
pub async fn run_stats_sampler() {
    let mut interval = tokio::time::interval(Duration::from_secs(SAMPLE_INTERVAL_SECS));
    // Match the monitor/GC loops: after a macOS sleep, take one sample on wake
    // rather than firing the whole backlog of missed ticks.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Persistent across ticks: sysinfo derives CPU usage from the delta between
    // two refreshes of the same process, so the collector must outlive a tick.
    let mut collector = StatsCollector::new();

    loop {
        interval.tick().await;
        if let Err(e) = sample_once(&mut collector).await {
            warn!("stats sampling error: {e}");
        }
    }
}

/// One sampling pass: refresh the process table once, then record a sample per
/// live node of every running run. Observational only — per-run write failures
/// are logged and skipped, never propagated.
async fn sample_once(collector: &mut StatsCollector) -> anyhow::Result<()> {
    // Open per pass so the sampler self-heals across CLI upgrades that migrate
    // the schema (mirrors the health monitor and GC loops).
    let db = Db::open()?;
    let registry = db.registry()?;

    // Skip the (machine-wide) process-table refresh entirely when nothing is
    // running — no point scanning every process to observe zero nodes.
    let any_running = registry
        .projects
        .values()
        .any(|e| e.runs.values().any(|r| r.status == RunStatus::Running));
    if !any_running {
        return Ok(());
    }
    collector.refresh();

    let sampled_at = chrono::Utc::now();
    for entry in registry.projects.values() {
        for (run_name, run_info) in &entry.runs {
            if run_info.status != RunStatus::Running {
                continue;
            }
            let run_state = match db.get_run(&entry.project_root, run_name) {
                Ok(Some(rs)) => rs,
                _ => continue,
            };
            let mut samples = Vec::new();
            for (key, node_state) in &run_state.nodes {
                if let Some(pid) = node_state.pid {
                    if veld_core::process::is_alive(pid) {
                        if let Some(sample) = collector.sample_tree(pid, sampled_at) {
                            samples.push((key.clone(), sample));
                        }
                    }
                }
            }
            if let Err(e) = db.record_node_stats(&entry.project_root, run_name, &samples) {
                debug!("could not record node stats for run '{run_name}': {e}");
            }
        }
    }

    Ok(())
}

/// Samples CPU/memory for run-node process trees using `sysinfo`.
///
/// Holds a persistent [`System`] across scans on purpose: `sysinfo` derives CPU
/// usage from the delta between two refreshes of the same process, so the
/// instance must outlive a single tick. The first sample taken after a process
/// first appears reads ~0% CPU, which is expected.
pub struct StatsCollector {
    sys: System,
    /// Parent-pid → child-pids, rebuilt on every [`refresh`](Self::refresh) so
    /// [`sample_tree`](Self::sample_tree) can walk descendants without
    /// re-scanning the whole process table per node.
    children: HashMap<Pid, Vec<Pid>>,
}

impl StatsCollector {
    pub fn new() -> Self {
        Self {
            sys: System::new(),
            children: HashMap::new(),
        }
    }

    /// Refresh the process table once. Call at the start of each scan, before
    /// [`sample_tree`](Self::sample_tree). Dead processes are dropped, and only
    /// CPU + memory are refreshed (not disk/network) to keep the scan cheap.
    pub fn refresh(&mut self) {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        self.children.clear();
        for (pid, proc_) in self.sys.processes() {
            if let Some(parent) = proc_.parent() {
                self.children.entry(parent).or_default().push(*pid);
            }
        }
    }

    /// Sum resource usage over the process tree rooted at `root_pid` (the
    /// node's process and its descendants). Returns `None` when the root
    /// process is absent from the last refresh (it already exited).
    pub fn sample_tree(&self, root_pid: u32, sampled_at: DateTime<Utc>) -> Option<ProcessStats> {
        aggregate_tree(Pid::from_u32(root_pid), &self.children, sampled_at, |pid| {
            self.sys.process(pid).map(|p| (p.cpu_usage(), p.memory()))
        })
    }
}

/// Walk the tree rooted at `root`, summing `(cpu_percent, memory_bytes)` from
/// `lookup` over every reachable process. `None` if `root` itself is absent
/// (the caller treats that as "exited, no sample"). Pure over its inputs so it
/// can be unit-tested without a live `sysinfo::System`.
fn aggregate_tree(
    root: Pid,
    children: &HashMap<Pid, Vec<Pid>>,
    sampled_at: DateTime<Utc>,
    lookup: impl Fn(Pid) -> Option<(f32, u64)>,
) -> Option<ProcessStats> {
    lookup(root)?; // root gone → no sample

    let mut cpu_percent = 0.0f32;
    let mut memory_bytes = 0u64;
    let mut process_count = 0u32;

    // Depth-first over the tree; `visited` guards against a parent cycle that
    // PID reuse could theoretically introduce.
    let mut stack = vec![root];
    let mut visited = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) {
            continue;
        }
        if let Some((cpu, mem)) = lookup(pid) {
            cpu_percent += cpu;
            memory_bytes += mem;
            process_count += 1;
            if let Some(kids) = children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }
    }

    Some(ProcessStats {
        cpu_percent,
        memory_bytes,
        process_count,
        sampled_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u32) -> Pid {
        Pid::from_u32(n)
    }

    fn now() -> DateTime<Utc> {
        chrono::DateTime::<Utc>::UNIX_EPOCH
    }

    #[test]
    fn sums_whole_tree() {
        // 1 → {2, 3}, 3 → {4}
        let children: HashMap<Pid, Vec<Pid>> =
            [(p(1), vec![p(2), p(3)]), (p(3), vec![p(4)])].into();
        let vals: HashMap<Pid, (f32, u64)> = [
            (p(1), (1.0, 10)),
            (p(2), (2.0, 20)),
            (p(3), (3.0, 30)),
            (p(4), (4.0, 40)),
        ]
        .into();
        let s = aggregate_tree(p(1), &children, now(), |pid| vals.get(&pid).copied()).unwrap();
        assert_eq!(s.process_count, 4);
        assert_eq!(s.memory_bytes, 100);
        assert!((s.cpu_percent - 10.0).abs() < 1e-6);
    }

    #[test]
    fn absent_root_is_none() {
        let children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        let empty: HashMap<Pid, (f32, u64)> = HashMap::new();
        assert!(aggregate_tree(p(1), &children, now(), |pid| empty.get(&pid).copied()).is_none());
    }

    #[test]
    fn cycle_guard_counts_each_once() {
        // 1 ↔ 2 (parent cycle from PID reuse)
        let children: HashMap<Pid, Vec<Pid>> = [(p(1), vec![p(2)]), (p(2), vec![p(1)])].into();
        let vals: HashMap<Pid, (f32, u64)> = [(p(1), (1.0, 10)), (p(2), (1.0, 10))].into();
        let s = aggregate_tree(p(1), &children, now(), |pid| vals.get(&pid).copied()).unwrap();
        assert_eq!(s.process_count, 2);
        assert_eq!(s.memory_bytes, 20);
    }

    #[test]
    fn skips_children_missing_from_lookup() {
        // A listed child (99) that already exited is skipped, not counted.
        let children: HashMap<Pid, Vec<Pid>> = [(p(1), vec![p(2), p(99)])].into();
        let vals: HashMap<Pid, (f32, u64)> = [(p(1), (0.0, 5)), (p(2), (0.0, 5))].into();
        let s = aggregate_tree(p(1), &children, now(), |pid| vals.get(&pid).copied()).unwrap();
        assert_eq!(s.process_count, 2);
        assert_eq!(s.memory_bytes, 10);
    }
}
