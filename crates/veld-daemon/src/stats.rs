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
//!
//! Each pass records the tree **twice**: once as the aggregate
//! ([`ProcessStats`], what a status table and a sparkline read) and once as one
//! row per process ([`ProcessSample`], what the expandable UI graphs and
//! `veld stats --processes` read). The per-process rows are not derived from the
//! aggregate later because they cannot be — the parent links and per-PID figures
//! only exist during the scan that produced them.
//!
//! Memory detail beyond RSS comes from [`crate::procmem`], which owns the
//! `/proc/<pid>/smaps_rollup` and `proc_pid_rusage` paths.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tracing::{debug, warn};
use veld_core::db::Db;
use veld_core::state::RunStatus;
use veld_core::stats::{MemoryBreakdown, ProcessSample, ProcessStats};

use crate::procmem;

/// Interval between resource-stats samples (seconds). Kept at/under
/// `veld_core::stats::STALE_AFTER_SECS` so a healthy sampler always refreshes a
/// node's stats before its last sample ages out.
const SAMPLE_INTERVAL_SECS: u64 = 5;

/// Command lines are truncated to this many characters before being stored.
///
/// A dev-server command line is routinely a few hundred characters of resolved
/// `node_modules` paths, and it is stored once per process per 5s tick — the
/// full text would dominate the table while adding nothing a reader can see in a
/// process row. Truncation is marked with `…` so nobody mistakes the result for
/// the real argv.
const CMD_MAX_CHARS: usize = 160;

/// Hard ceiling on processes recorded per node per sample.
///
/// A runaway `make -j` or a container-in-a-container can put hundreds of
/// processes under one node, and per-process rows are written at the sample
/// cadence. The aggregate always counts the *whole* tree
/// (`ProcessStats::process_count`), so exceeding this cap costs the per-process
/// breakdown its tail, never the node's totals — and the breakdown keeps the
/// heaviest processes, since that is what a reader opened it for.
const MAX_PROCESSES_PER_NODE: usize = 64;

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
            let mut trees = Vec::new();
            for (key, node_state) in &run_state.nodes {
                if let Some(pid) = node_state.pid {
                    if veld_core::process::is_alive(pid) {
                        if let Some(tree) = collector.sample_tree(pid, sampled_at) {
                            samples.push((key.clone(), tree.total));
                            trees.push((key.clone(), tree.processes));
                        }
                    }
                }
            }
            if let Err(e) = db.record_node_stats(&entry.project_root, run_name, &samples, &trees) {
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
    /// what a sample needs is refreshed (no disk/network) to keep the scan cheap.
    ///
    /// `with_cmd(OnlyIfNotSet)` is deliberate: a command line cannot change
    /// after `exec`, so re-reading it every 5s for every process on the machine
    /// would be pure waste. `children` is keyed by parent so
    /// [`sample_tree`](Self::sample_tree) walks descendants without re-scanning
    /// the process table per node.
    pub fn refresh(&mut self) {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        self.children.clear();
        for (pid, proc_) in self.sys.processes() {
            if let Some(parent) = proc_.parent() {
                self.children.entry(parent).or_default().push(*pid);
            }
        }
        // Children in ascending PID order, so the recorded tree order is stable
        // between samples. `sysinfo`'s process map iterates in hash order, which
        // would otherwise reshuffle sibling order on every tick and make a
        // process table visibly jitter.
        for kids in self.children.values_mut() {
            kids.sort_unstable();
        }
    }

    /// Sample the process tree rooted at `root_pid` (the node's process and its
    /// descendants): the summed aggregate plus one record per process. Returns
    /// `None` when the root process is absent from the last refresh (it already
    /// exited).
    pub fn sample_tree(&self, root_pid: u32, sampled_at: DateTime<Utc>) -> Option<TreeSample> {
        aggregate_tree(Pid::from_u32(root_pid), &self.children, sampled_at, |pid| {
            let p = self.sys.process(pid)?;
            let resident = p.memory();
            let probe = procmem::probe(pid.as_u32(), resident, p.virtual_memory());
            Some(ProcessReading {
                parent_pid: p.parent().map(|pp| pp.as_u32()),
                name: p.name().to_string_lossy().into_owned(),
                cmd: join_cmd(p.cmd()),
                cpu_percent: p.cpu_usage(),
                cpu_seconds: probe.cpu_seconds.unwrap_or(0.0),
                memory_bytes: resident,
                memory: probe.memory,
                // `start_time` is 0 when the platform didn't report one; a run
                // node started in 1970 is not a case worth representing.
                started_at: match p.start_time() {
                    0 => None,
                    secs => DateTime::from_timestamp(secs as i64, 0),
                },
            })
        })
    }
}

/// A node's tree at one instant: the aggregate and the per-process rows that
/// produced it.
pub struct TreeSample {
    pub total: ProcessStats,
    pub processes: Vec<ProcessSample>,
}

/// Everything one process contributes to a sample. Separated from
/// [`ProcessSample`] because the walk assigns `pid` and `depth` itself — the
/// lookup only knows about the process, not its place in the tree.
struct ProcessReading {
    parent_pid: Option<u32>,
    name: String,
    cmd: Option<String>,
    cpu_percent: f32,
    cpu_seconds: f64,
    memory_bytes: u64,
    memory: MemoryBreakdown,
    started_at: Option<DateTime<Utc>>,
}

/// Space-join an argv into a displayable command line, truncated to
/// [`CMD_MAX_CHARS`]. `None` for a process that reports no argv (a zombie, or
/// one whose `cmdline` the kernel withholds).
///
/// Truncates on a **character** boundary, not a byte one: an argv can hold any
/// UTF-8 (a project path with an umlaut is enough), and slicing bytes would
/// panic mid-codepoint.
fn join_cmd(argv: &[std::ffi::OsString]) -> Option<String> {
    if argv.is_empty() {
        return None;
    }
    let full = argv
        .iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    if full.chars().count() <= CMD_MAX_CHARS {
        return Some(full);
    }
    let mut out: String = full.chars().take(CMD_MAX_CHARS).collect();
    out.push('…');
    Some(out)
}

/// Walk the tree rooted at `root`, summing every reachable process into a
/// [`ProcessStats`] and collecting the per-process rows. `None` if `root` itself
/// is absent (the caller treats that as "exited, no sample"). Pure over its
/// inputs so it can be unit-tested without a live `sysinfo::System`.
fn aggregate_tree(
    root: Pid,
    children: &HashMap<Pid, Vec<Pid>>,
    sampled_at: DateTime<Utc>,
    lookup: impl Fn(Pid) -> Option<ProcessReading>,
) -> Option<TreeSample> {
    lookup(root)?; // root gone → no sample

    let mut cpu_percent = 0.0f32;
    let mut memory_bytes = 0u64;
    let mut cpu_seconds = 0.0f64;
    let mut process_count = 0u32;
    let mut memory = MemoryBreakdown::zero_sum();
    let mut processes: Vec<ProcessSample> = Vec::new();

    // Pre-order depth-first: a parent is always recorded before its children, so
    // a renderer can indent by `depth` without sorting. `visited` guards against
    // a parent cycle that PID reuse could theoretically introduce.
    let mut stack = vec![(root, 0u32)];
    let mut visited = HashSet::new();
    while let Some((pid, depth)) = stack.pop() {
        if !visited.insert(pid) {
            continue;
        }
        if let Some(r) = lookup(pid) {
            cpu_percent += r.cpu_percent;
            memory_bytes += r.memory_bytes;
            cpu_seconds += r.cpu_seconds;
            process_count += 1;
            memory.add(&r.memory);
            processes.push(ProcessSample {
                pid: pid.as_u32(),
                // The root's parent is outside the tree (it's the CLI or
                // launchd), so recording it would suggest a row that isn't
                // there.
                parent_pid: if pid == root { None } else { r.parent_pid },
                depth,
                name: r.name,
                cmd: r.cmd,
                cpu_percent: r.cpu_percent,
                cpu_seconds: r.cpu_seconds,
                memory_bytes: r.memory_bytes,
                memory: r.memory,
                started_at: r.started_at,
            });
            if let Some(kids) = children.get(&pid) {
                // Reversed, because the stack pops last-in-first: this makes the
                // walk visit siblings in ascending PID order.
                stack.extend(kids.iter().rev().map(|k| (*k, depth + 1)));
            }
        }
    }

    Some(TreeSample {
        total: ProcessStats {
            cpu_percent,
            memory_bytes,
            process_count,
            cpu_seconds,
            memory,
            sampled_at,
        },
        processes: cap_processes(processes),
    })
}

/// Trim a recorded tree to [`MAX_PROCESSES_PER_NODE`], keeping the heaviest
/// processes by footprint and preserving the pre-order of those kept.
///
/// Dropping a mid-tree parent can leave a child whose `parent_pid` is not in the
/// list; renderers indent by `depth` and must not require the parent to be
/// present. The aggregate is unaffected — it summed the whole tree before this
/// runs.
fn cap_processes(mut processes: Vec<ProcessSample>) -> Vec<ProcessSample> {
    if processes.len() <= MAX_PROCESSES_PER_NODE {
        return processes;
    }
    // Rank by footprint to find the cut-off, then filter the original order by
    // the surviving PIDs so pre-order survives.
    let mut by_weight: Vec<(u64, u32)> = processes
        .iter()
        .map(|p| (p.memory.footprint, p.pid))
        .collect();
    by_weight.sort_unstable_by(|a, b| b.cmp(a));
    let keep: HashSet<u32> = by_weight
        .into_iter()
        .take(MAX_PROCESSES_PER_NODE)
        .map(|(_, pid)| pid)
        .collect();
    processes.retain(|p| keep.contains(&p.pid));
    processes
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

    /// A reading with detailed memory: footprint = half of RSS, one page class.
    fn reading(cpu: f32, mem: u64) -> ProcessReading {
        ProcessReading {
            parent_pid: None,
            name: format!("proc{mem}"),
            cmd: Some("cmd".into()),
            cpu_percent: cpu,
            cpu_seconds: cpu as f64,
            memory_bytes: mem,
            memory: MemoryBreakdown {
                footprint: mem / 2,
                virtual_bytes: mem * 4,
                private_dirty: Some(mem / 4),
                ..Default::default()
            },
            started_at: None,
        }
    }

    /// Build a lookup over a `(pid, cpu, mem)` table.
    fn lookup_of(vals: &[(u32, f32, u64)]) -> impl Fn(Pid) -> Option<ProcessReading> + '_ {
        move |pid| {
            vals.iter()
                .find(|(v, _, _)| *v == pid.as_u32())
                .map(|(_, cpu, mem)| reading(*cpu, *mem))
        }
    }

    #[test]
    fn sums_whole_tree() {
        // 1 → {2, 3}, 3 → {4}
        let children: HashMap<Pid, Vec<Pid>> =
            [(p(1), vec![p(2), p(3)]), (p(3), vec![p(4)])].into();
        let vals = [(1, 1.0, 10), (2, 2.0, 20), (3, 3.0, 30), (4, 4.0, 40)];
        let t = aggregate_tree(p(1), &children, now(), lookup_of(&vals)).unwrap();
        assert_eq!(t.total.process_count, 4);
        assert_eq!(t.total.memory_bytes, 100);
        assert!((t.total.cpu_percent - 10.0).abs() < 1e-6);
        assert_eq!(t.total.memory.footprint, 50, "footprints sum too");
        // mem/4 per process, truncated: 2 + 5 + 7 + 10.
        assert_eq!(t.total.memory.private_dirty, Some(24));
        assert_eq!(t.total.cpu_seconds, 10.0);
        assert_eq!(t.processes.len(), 4);
    }

    #[test]
    fn records_processes_in_preorder_with_depth() {
        // 1 → {3, 2}, 3 → {4}: parents first, siblings by ascending pid.
        let children: HashMap<Pid, Vec<Pid>> =
            [(p(1), vec![p(2), p(3)]), (p(3), vec![p(4)])].into();
        let vals = [(1, 0.0, 10), (2, 0.0, 10), (3, 0.0, 10), (4, 0.0, 10)];
        let t = aggregate_tree(p(1), &children, now(), lookup_of(&vals)).unwrap();
        let order: Vec<(u32, u32)> = t.processes.iter().map(|x| (x.pid, x.depth)).collect();
        assert_eq!(order, vec![(1, 0), (2, 1), (3, 1), (4, 2)]);
        assert_eq!(
            t.processes[0].parent_pid, None,
            "the root's parent is outside the tree"
        );
        assert_eq!(t.processes[3].depth, 2);
    }

    #[test]
    fn one_process_without_detail_poisons_the_class_totals() {
        // The tree sum must not present a partial page-class total as complete.
        let children: HashMap<Pid, Vec<Pid>> = [(p(1), vec![p(2)])].into();
        let t = aggregate_tree(p(1), &children, now(), |pid| match pid.as_u32() {
            1 => Some(reading(0.0, 100)),
            2 => Some(ProcessReading {
                memory: MemoryBreakdown::basic(100, 400),
                ..reading(0.0, 100)
            }),
            _ => None,
        })
        .unwrap();
        assert_eq!(t.total.memory_bytes, 200);
        assert_eq!(t.total.memory.footprint, 150, "50 detailed + 100 fallback");
        assert_eq!(t.total.memory.private_dirty, None);
    }

    #[test]
    fn absent_root_is_none() {
        let children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        assert!(aggregate_tree(p(1), &children, now(), lookup_of(&[])).is_none());
    }

    #[test]
    fn cycle_guard_counts_each_once() {
        // 1 ↔ 2 (parent cycle from PID reuse)
        let children: HashMap<Pid, Vec<Pid>> = [(p(1), vec![p(2)]), (p(2), vec![p(1)])].into();
        let vals = [(1, 1.0, 10), (2, 1.0, 10)];
        let t = aggregate_tree(p(1), &children, now(), lookup_of(&vals)).unwrap();
        assert_eq!(t.total.process_count, 2);
        assert_eq!(t.total.memory_bytes, 20);
        assert_eq!(t.processes.len(), 2);
    }

    #[test]
    fn skips_children_missing_from_lookup() {
        // A listed child (99) that already exited is skipped, not counted.
        let children: HashMap<Pid, Vec<Pid>> = [(p(1), vec![p(2), p(99)])].into();
        let vals = [(1, 0.0, 5), (2, 0.0, 5)];
        let t = aggregate_tree(p(1), &children, now(), lookup_of(&vals)).unwrap();
        assert_eq!(t.total.process_count, 2);
        assert_eq!(t.total.memory_bytes, 10);
    }

    #[test]
    fn cap_keeps_the_heaviest_in_preorder_and_leaves_the_total_alone() {
        // A wide tree past the cap: the aggregate counts everything, the
        // per-process list keeps the biggest MAX_PROCESSES_PER_NODE.
        let kids: Vec<Pid> = (2..=200u32).map(p).collect();
        let children: HashMap<Pid, Vec<Pid>> = [(p(1), kids)].into();
        // Memory grows with pid, so the survivors are the highest pids.
        let t = aggregate_tree(p(1), &children, now(), |pid| {
            Some(reading(0.0, pid.as_u32() as u64 * 1000))
        })
        .unwrap();
        assert_eq!(
            t.total.process_count, 200,
            "the aggregate sees every process"
        );
        assert_eq!(t.processes.len(), MAX_PROCESSES_PER_NODE);
        let pids: Vec<u32> = t.processes.iter().map(|x| x.pid).collect();
        let mut sorted = pids.clone();
        sorted.sort_unstable();
        assert_eq!(pids, sorted, "kept processes stay in pre-order");
        assert_eq!(*pids.last().unwrap(), 200, "the heaviest survived");
        assert!(!pids.contains(&2), "the lightest was dropped");
    }

    /// End-to-end against the real platform: sample this test process's own
    /// tree and check the detail actually arrived. The pure `aggregate_tree`
    /// tests above use a synthetic lookup, so nothing else would notice if
    /// `procmem` stopped returning a footprint on this OS.
    #[test]
    fn samples_this_process_with_real_platform_detail() {
        let mut collector = StatsCollector::new();
        collector.refresh();
        let t = collector
            .sample_tree(std::process::id(), now())
            .expect("our own process is in the refreshed table");

        assert!(t.total.process_count >= 1);
        assert!(
            t.total.memory_bytes > 0,
            "sysinfo reported no RSS for a live process"
        );
        assert!(
            t.total.memory.footprint > 0,
            "footprint must never be zero — it degrades to RSS, not to nothing"
        );
        assert!(t.total.memory.virtual_bytes > 0);
        let me = t
            .processes
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("the root is recorded");
        assert_eq!(me.depth, 0);
        assert!(me.cmd.is_some(), "a live process reports an argv");

        // Assert the platform's capability on the *root's own* breakdown, not on
        // the tree total. Under a parallel test run this process has children
        // (other tests spawn them), and any one of them whose detailed probe
        // fails legitimately poisons the total's optional fields to `None` — see
        // `MemoryBreakdown::add`. Checking the total here would make this test
        // fail depending on which tests ran beside it, which is exactly the
        // flake it looked like the first time.
        #[cfg(target_os = "linux")]
        assert!(
            me.memory.has_page_classes(),
            "linux must report the private/shared split"
        );
        #[cfg(target_os = "macos")]
        {
            assert!(
                !me.memory.has_page_classes(),
                "macOS cannot split page classes unprivileged"
            );
            assert!(me.memory.wired.is_some(), "macOS reports wired memory");
        }
    }

    /// The whole path, on real processes and a real database: sample a live
    /// process tree, persist both the aggregate and the per-process rows, then
    /// read them back the way `veld stats` and `/api/stats/history` do.
    ///
    /// The unit tests above cover the walk and the SQL separately; this is the
    /// only thing that would notice if the two stopped fitting together — a
    /// column written in one order and read in another, or a tree whose
    /// timestamp doesn't match its aggregate's and so is invisible to every
    /// reader.
    #[tokio::test]
    async fn records_and_reads_back_a_real_tree() {
        use veld_core::state::RunState;
        use veld_core::stats::{MemoryMetric, StatsWindow};

        let tmp = tempfile::tempdir().unwrap();
        // `open_at` rather than `Db::open()`: the sampler's own `sample_once`
        // opens the machine's real database, which a test must not touch.
        let db = Db::open_at(&tmp.path().join("veld.db")).unwrap();
        let project_root = tmp.path().join("proj");

        // A real two-process tree: a shell that keeps a child alive.
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn test tree");
        let pid = child.id().expect("child has a pid");

        let mut run = RunState::new("dev", "proj");
        run.nodes.insert(
            "web:local".to_owned(),
            veld_core::state::NodeState {
                pid: Some(pid),
                ..veld_core::state::NodeState::new("web", "local")
            },
        );
        db.save_run(&project_root, "proj", &run).unwrap();

        let mut collector = StatsCollector::new();
        // Two passes: sysinfo derives CPU% from the delta between refreshes, so
        // the first sample of a new process always reads 0%.
        let mut last = None;
        for i in 0..2 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            collector.refresh();
            let at = Utc::now() + chrono::Duration::seconds(i);
            let tree = collector.sample_tree(pid, at).expect("tree is alive");
            db.record_node_stats(
                &project_root,
                "dev",
                &[("web:local".to_owned(), tree.total)],
                &[("web:local".to_owned(), tree.processes)],
            )
            .unwrap();
            last = Some(at);
        }
        let last = last.unwrap();

        let latest = db.latest_node_stats(&project_root, "dev").unwrap();
        let s = latest.get("web:local").expect("aggregate persisted");
        assert!(s.memory.footprint > 0, "footprint round-tripped");
        assert_eq!(
            s.memory_metric(MemoryMetric::Resident),
            Some(s.memory_bytes)
        );
        assert!(s.process_count >= 1);

        let tree = db
            .latest_process_tree(&project_root, "dev", "web:local")
            .unwrap();
        assert!(!tree.is_empty(), "per-process rows persisted");
        assert_eq!(tree[0].pid, pid, "the root comes first");
        assert_eq!(tree[0].depth, 0);
        assert_eq!(
            tree.len(),
            s.process_count as usize,
            "the stored tree matches the aggregate's count for a small tree"
        );

        // The reader the UI's chart uses. One-second buckets over a window that
        // contains both samples.
        let window = StatsWindow {
            start: last - chrono::Duration::seconds(30),
            end: last + chrono::Duration::seconds(1),
            bucket_secs: 1,
        };
        let buckets = db
            .node_stats_buckets(&project_root, "dev", "web:local", window)
            .unwrap();
        assert!(!buckets.is_empty(), "history buckets came back");
        assert!(buckets.iter().all(|b| b.samples >= 1));
        assert!(buckets.iter().any(|b| b.memory.footprint > 0));

        let series = db
            .process_stats_buckets(&project_root, "dev", "web:local", window)
            .unwrap();
        assert!(!series.is_empty(), "per-process series came back");
        assert!(
            series.iter().any(|x| x.pid == pid),
            "the root process has its own series"
        );

        let _ = child.kill().await;
    }

    #[test]
    fn cmd_is_joined_and_truncated_on_char_boundaries() {
        use std::ffi::OsString;
        assert_eq!(join_cmd(&[]), None);
        assert_eq!(
            join_cmd(&[OsString::from("node"), OsString::from("server.js")]).unwrap(),
            "node server.js"
        );
        // Multi-byte characters: truncating bytes here would panic.
        let long = OsString::from("ü".repeat(CMD_MAX_CHARS * 2));
        let out = join_cmd(&[long]).unwrap();
        assert_eq!(
            out.chars().count(),
            CMD_MAX_CHARS + 1,
            "cap plus the ellipsis"
        );
        assert!(out.ends_with('…'));
    }

    /// RC1 characterization: the sampled *set* for a detached node.
    ///
    /// The parent-tree walk from the tracked PID must reach both stages of the
    /// detached pipeline — the node's own process and the `veld _log` writer —
    /// because the tracked PID is the pipeline shell that parents both. Any
    /// change to the spawn path that reparents a stage (e.g. rebuilding the
    /// pipeline with explicit file descriptors) would silently change which
    /// processes this sums, showing up as an unexplained step change in the
    /// CPU/memory graph rather than as a test failure. So pin the set here.
    #[tokio::test]
    async fn process_tree_shape_stable() {
        let tmp = tempfile::tempdir().unwrap();
        // `tee` stands in for `veld _log`: one exec'd binary draining the pipe,
        // matching production's topology. `sleep` stands in for the node.
        let sink = vec![
            "tee".to_owned(),
            tmp.path().join("sink.log").to_string_lossy().into_owned(),
        ];
        let handle = veld_core::process::spawn_detached(
            &veld_core::config::CommandSpec::Shell("sleep 30".to_owned()),
            tmp.path(),
            &HashMap::new(),
            &sink,
        )
        .expect("detached spawn");
        let pid = handle.pid();

        // Give both stages time to fork and exec.
        let mut collector = StatsCollector::new();
        let mut names: Vec<String> = Vec::new();
        let mut count = 0u32;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            collector.refresh();
            names = walk_names(&collector, pid);
            count = collector
                .sample_tree(pid, Utc::now())
                .map(|s| s.total.process_count)
                .unwrap_or(0);
            if names.iter().any(|n| n.contains("sleep")) && names.iter().any(|n| n.contains("tee"))
            {
                break;
            }
        }

        assert!(
            names.iter().any(|n| n.contains("sleep")),
            "the node's own process must be in the sampled tree, saw {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("tee")),
            "the log writer must be in the sampled tree, saw {names:?}"
        );
        assert_eq!(
            count as usize,
            names.len(),
            "process_count must equal the walked set"
        );
        assert!(
            count >= 2,
            "the pipeline contributes at least the node and the log writer, got {count}"
        );

        let _ = veld_core::process::kill_process(pid).await;
    }

    /// Names of every process the tree walk from `root` reaches, using the same
    /// parent map [`StatsCollector::sample_tree`] sums over.
    fn walk_names(collector: &StatsCollector, root: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![Pid::from_u32(root)];
        let mut visited = HashSet::new();
        while let Some(pid) = stack.pop() {
            if !visited.insert(pid) {
                continue;
            }
            let Some(proc_) = collector.sys.process(pid) else {
                continue;
            };
            out.push(proc_.name().to_string_lossy().into_owned());
            if let Some(kids) = collector.children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }
        out
    }
}
