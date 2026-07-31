//! Per-node process resource statistics.
//!
//! A single sample of the resource usage of one run node — summed across the
//! node's process and its descendants (found by walking parent→child links),
//! so a `npm run dev` that forks a bundler and a server reports one combined
//! figure. Descendants that reparent away (a daemonizing double-fork ends up
//! under init/launchd) fall outside the tree and are not counted.
//!
//! Sampling lives in the daemon's stats sampler (see `veld-daemon`'s
//! `StatsCollector`, which owns the cross-platform `sysinfo` probing and the
//! per-platform memory detail); this crate only defines the shared data types
//! and their persistence in [`crate::db`]. Keeping the types here means the CLI
//! can read stored samples without pulling in `sysinfo`.
//!
//! # Why there is more than one memory number
//!
//! "How much memory does this node use" has no single answer, and the obvious
//! one is wrong. RSS counts every resident page, including pages shared with
//! other processes — so summing RSS over a process tree counts the shared libc,
//! the shared Node runtime, and every copy-on-write page inherited at fork
//! *once per process*. A five-process `npm run dev` tree reports far more than
//! it occupies. [`MemoryBreakdown::footprint`] is the figure that doesn't lie:
//! proportional set size on Linux (each shared page divided by the number of
//! processes mapping it, so a tree sum is honest) and `phys_footprint` on macOS
//! (the number Activity Monitor shows). The rest of the breakdown exists because
//! *which* kind of memory a node holds is the diagnostic: private dirty pages
//! are a leak, shared clean pages are just mapped binaries.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A sample older than this many seconds is treated as absent by readers.
/// The daemon's stats sampler runs on its own ~5s timer (`SAMPLE_INTERVAL_SECS`
/// in `veld-daemon`, deliberately decoupled from the liveness-probe loop so
/// slow probes can't stretch the gap), so a reading older than a few intervals
/// means sampling stopped — the node's process died or the daemon isn't
/// running — and the last value is no longer live. Three intervals of slack
/// absorbs a skipped tick without flapping.
pub const STALE_AFTER_SECS: i64 = 15;

/// The memory figures a platform can report for one process, beyond plain RSS.
///
/// Every field except [`footprint`](Self::footprint) and
/// [`virtual_bytes`](Self::virtual_bytes) is optional, because no two kernels
/// expose the same split and pretending otherwise would mean inventing numbers.
/// A `None` means "this platform cannot answer that", not "zero" — readers must
/// render it as unavailable rather than as an empty bar. What each platform
/// fills in:
///
/// | field | Linux (`/proc/<pid>/smaps_rollup`) | macOS (`proc_pid_rusage`) |
/// |---|---|---|
/// | `footprint` | `Pss` | `ri_phys_footprint` |
/// | `virtual_bytes` | `sysinfo` | `sysinfo` |
/// | `private_clean` / `private_dirty` | yes | — |
/// | `shared_clean` / `shared_dirty` | yes | — |
/// | `swap` | `Swap` | — |
/// | `wired` | `Locked` | `ri_wired_size` |
///
/// When the detailed source can't be read (a process that exits mid-scan, a
/// hardened kernel, `VELD_STATS_MEMORY_DETAIL=off`), `footprint` falls back to
/// RSS and every optional field is `None`. That is why the UI keys "detailed
/// breakdown available" off the optional fields being present rather than off a
/// separate flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryBreakdown {
    /// The honest footprint: proportional set size (Linux) or phys_footprint
    /// (macOS). Safe to sum across a process tree — unlike RSS, it does not
    /// count a shared page once per process that maps it. Falls back to RSS
    /// when the detailed source is unavailable.
    pub footprint: u64,
    /// Virtual address-space size. Large and mostly meaningless on 64-bit
    /// runtimes that reserve address space eagerly (the JVM, Go, sanitizers) —
    /// kept because a *change* in it still diagnoses a mapping leak.
    pub virtual_bytes: u64,
    /// Pages mapped only by this process that have not been written since being
    /// paged in — mostly the process's own read-only mappings.
    pub private_clean: Option<u64>,
    /// Pages mapped only by this process that have been written: the heap, the
    /// stack, anything modified after fork. The number that grows when a node
    /// leaks, and the only memory the kernel must write to swap to reclaim.
    pub private_dirty: Option<u64>,
    /// Resident pages shared with at least one other process and unmodified —
    /// mapped executables and shared libraries. Cheap; near-free to add another
    /// process that maps the same files.
    pub shared_clean: Option<u64>,
    /// Resident pages shared with another process and written by someone —
    /// copy-on-write pages after a fork, `MAP_SHARED` buffers, shared memory.
    pub shared_dirty: Option<u64>,
    /// Anonymous memory paged out to swap. Not resident, so it appears in
    /// neither RSS nor footprint; a node whose RSS looks flat while this climbs
    /// is thrashing.
    pub swap: Option<u64>,
    /// Memory that can never be paged out (`mlock`ed on Linux, wired on macOS).
    pub wired: Option<u64>,
}

impl MemoryBreakdown {
    /// A breakdown carrying only what every platform can answer, used when the
    /// detailed source is unreadable. `footprint` degrades to RSS, which
    /// over-reports a multi-process tree — the honest failure mode, since the
    /// alternative is reporting nothing at all.
    pub fn basic(resident: u64, virtual_bytes: u64) -> Self {
        Self {
            footprint: resident,
            virtual_bytes,
            ..Self::default()
        }
    }

    /// Whether this breakdown carries the per-page-class split (Linux only
    /// today). Readers use it to hide a "split by memory type" view rather than
    /// render an empty chart.
    pub fn has_page_classes(&self) -> bool {
        self.private_dirty.is_some() || self.shared_clean.is_some()
    }

    /// Add `other` into `self`, treating an absent field as absent in the sum:
    /// if *any* summed process could not report `private_dirty`, the total
    /// cannot either. Silently coercing the gap to zero would render a partial
    /// tree as a complete breakdown that happens to be too small — the exact
    /// error a resource graph must not make.
    pub fn add(&mut self, other: &Self) {
        self.footprint = self.footprint.saturating_add(other.footprint);
        self.virtual_bytes = self.virtual_bytes.saturating_add(other.virtual_bytes);
        fn add_opt(acc: &mut Option<u64>, v: Option<u64>) {
            *acc = match (*acc, v) {
                (Some(a), Some(b)) => Some(a.saturating_add(b)),
                _ => None,
            };
        }
        add_opt(&mut self.private_clean, other.private_clean);
        add_opt(&mut self.private_dirty, other.private_dirty);
        add_opt(&mut self.shared_clean, other.shared_clean);
        add_opt(&mut self.shared_dirty, other.shared_dirty);
        add_opt(&mut self.swap, other.swap);
        add_opt(&mut self.wired, other.wired);
    }

    /// The neutral element for [`add`](Self::add): every optional field present
    /// and zero, so the first real breakdown folded in decides which fields
    /// survive. Starting from [`Default`] instead would make every sum `None`.
    pub fn zero_sum() -> Self {
        Self {
            footprint: 0,
            virtual_bytes: 0,
            private_clean: Some(0),
            private_dirty: Some(0),
            shared_clean: Some(0),
            shared_dirty: Some(0),
            swap: Some(0),
            wired: Some(0),
        }
    }
}

/// Which memory figure a reader wants plotted or tabulated.
///
/// Selecting the metric belongs to the reader, not the collector: the same
/// stored sample answers "is this node leaking" (`PrivateDirty`), "what does it
/// cost the machine" (`Footprint`), and "why does `top` say 4 GB"
/// (`Virtual`). Parsed from `veld stats --memory <metric>` and the
/// `metric=` query parameter, so the spellings are user-facing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMetric {
    /// [`MemoryBreakdown::footprint`] — the default, and the only figure that
    /// sums correctly over a process tree.
    #[default]
    Footprint,
    /// Resident set size. Sums over a tree by double-counting shared pages.
    Resident,
    /// [`MemoryBreakdown::virtual_bytes`].
    Virtual,
    PrivateClean,
    PrivateDirty,
    SharedClean,
    SharedDirty,
    Swap,
    Wired,
}

impl MemoryMetric {
    /// Every metric, in the order UIs should offer them: the two totals first,
    /// then the page classes, then the two that are usually zero.
    pub const ALL: &'static [MemoryMetric] = &[
        MemoryMetric::Footprint,
        MemoryMetric::Resident,
        MemoryMetric::PrivateDirty,
        MemoryMetric::PrivateClean,
        MemoryMetric::SharedDirty,
        MemoryMetric::SharedClean,
        MemoryMetric::Swap,
        MemoryMetric::Wired,
        MemoryMetric::Virtual,
    ];

    /// The metrics that stack into a total without overlapping, when the
    /// platform reports them. Deliberately *not* every metric: footprint,
    /// resident and virtual each already total the others, so stacking them
    /// beside a page class would draw the same bytes twice.
    pub const STACK: &'static [MemoryMetric] = &[
        MemoryMetric::PrivateDirty,
        MemoryMetric::PrivateClean,
        MemoryMetric::SharedDirty,
        MemoryMetric::SharedClean,
        MemoryMetric::Swap,
    ];

    /// Whether this metric is a page class rather than a total. Only page
    /// classes can be absent on a platform, so this is what a UI checks before
    /// telling the reader "no split available here".
    pub fn is_page_class(&self) -> bool {
        !matches!(
            self,
            MemoryMetric::Footprint | MemoryMetric::Resident | MemoryMetric::Virtual
        )
    }

    /// The wire/CLI spelling. Matches the serde representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryMetric::Footprint => "footprint",
            MemoryMetric::Resident => "resident",
            MemoryMetric::Virtual => "virtual",
            MemoryMetric::PrivateClean => "private_clean",
            MemoryMetric::PrivateDirty => "private_dirty",
            MemoryMetric::SharedClean => "shared_clean",
            MemoryMetric::SharedDirty => "shared_dirty",
            MemoryMetric::Swap => "swap",
            MemoryMetric::Wired => "wired",
        }
    }

    /// Short column header for a table.
    pub fn label(&self) -> &'static str {
        match self {
            MemoryMetric::Footprint => "FOOTPRINT",
            MemoryMetric::Resident => "RSS",
            MemoryMetric::Virtual => "VIRT",
            MemoryMetric::PrivateClean => "PRIV CLEAN",
            MemoryMetric::PrivateDirty => "PRIV DIRTY",
            MemoryMetric::SharedClean => "SHR CLEAN",
            MemoryMetric::SharedDirty => "SHR DIRTY",
            MemoryMetric::Swap => "SWAP",
            MemoryMetric::Wired => "WIRED",
        }
    }

    /// Read this metric out of a breakdown. `resident` is passed separately
    /// because RSS lives on the sample, not the breakdown (there is exactly one
    /// RSS and it predates the breakdown on the wire). `None` = the platform
    /// does not report it.
    pub fn read(&self, resident: u64, m: &MemoryBreakdown) -> Option<u64> {
        match self {
            MemoryMetric::Footprint => Some(m.footprint),
            MemoryMetric::Resident => Some(resident),
            MemoryMetric::Virtual => Some(m.virtual_bytes),
            MemoryMetric::PrivateClean => m.private_clean,
            MemoryMetric::PrivateDirty => m.private_dirty,
            MemoryMetric::SharedClean => m.shared_clean,
            MemoryMetric::SharedDirty => m.shared_dirty,
            MemoryMetric::Swap => m.swap,
            MemoryMetric::Wired => m.wired,
        }
    }
}

impl fmt::Display for MemoryMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MemoryMetric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept `-` as well as `_` so `--memory private-dirty` works; a hyphen
        // is what a human types on a command line.
        let norm = s.trim().to_ascii_lowercase().replace('-', "_");
        match norm.as_str() {
            "footprint" => Ok(MemoryMetric::Footprint),
            "resident" | "rss" => Ok(MemoryMetric::Resident),
            "virtual" | "virt" | "vsz" => Ok(MemoryMetric::Virtual),
            "private_clean" => Ok(MemoryMetric::PrivateClean),
            "private_dirty" | "dirty" => Ok(MemoryMetric::PrivateDirty),
            "shared_clean" => Ok(MemoryMetric::SharedClean),
            "shared_dirty" => Ok(MemoryMetric::SharedDirty),
            "swap" => Ok(MemoryMetric::Swap),
            "wired" | "locked" => Ok(MemoryMetric::Wired),
            other => Err(format!(
                "unknown memory metric '{other}' (expected one of: {})",
                MemoryMetric::ALL
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// One resource-usage sample for a node's process tree at a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProcessStats {
    /// CPU usage as a percentage of a single core (may exceed 100 on a
    /// multi-threaded tree), summed across the process tree.
    pub cpu_percent: f32,
    /// Resident memory (RSS) in bytes, summed across the process tree.
    ///
    /// Double-counts pages shared between processes in the tree; prefer
    /// `memory.footprint`. It stays the top-level field (rather than moving
    /// into [`MemoryBreakdown`]) because `veld status --json` and every shipped
    /// management-UI bundle read this name.
    pub memory_bytes: u64,
    /// Number of live processes in the tree (the node's process + descendants).
    pub process_count: u32,
    /// Total CPU time consumed by the tree since each process started, in
    /// seconds. Unlike `cpu_percent` this is cumulative and immune to sampling
    /// gaps, so it answers "how much CPU did this node burn overall" — but it
    /// *drops* when a busy child exits and leaves the tree.
    #[serde(default)]
    pub cpu_seconds: f64,
    /// Memory detail, summed across the tree (see [`MemoryBreakdown::add`] for
    /// how absent fields propagate).
    #[serde(default)]
    pub memory: MemoryBreakdown,
    /// When the sample was taken. Serialized as epoch milliseconds so
    /// `veld status --json` consumers get a plain number.
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub sampled_at: DateTime<Utc>,
}

impl ProcessStats {
    /// Whether this sample is recent enough to present as a live figure
    /// (see [`STALE_AFTER_SECS`]). A future `sampled_at` (clock skew) counts
    /// as fresh.
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        (now - self.sampled_at).num_seconds() <= STALE_AFTER_SECS
    }

    /// Read one memory metric off this sample.
    pub fn memory_metric(&self, metric: MemoryMetric) -> Option<u64> {
        metric.read(self.memory_bytes, &self.memory)
    }
}

/// One process inside a node's tree at a point in time.
///
/// Stored one row per process per sample so "graph this subprocess over time"
/// is a query rather than a scan over serialized trees. `pid` is not a stable
/// identity across a run — PIDs are reused, and a `npm run dev` that restarts
/// its child gets a new one — so a series keyed by PID legitimately starts and
/// stops mid-window; readers must not interpolate across the gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessSample {
    pub pid: u32,
    /// The parent as observed at sample time, `None` for the tree root (the
    /// node's tracked PID) or when the parent had already exited.
    pub parent_pid: Option<u32>,
    /// Depth below the tree root; the root itself is 0. Precomputed at sample
    /// time because the parent links needed to derive it are only complete
    /// during the scan that produced them.
    pub depth: u32,
    /// Executable name as the OS reports it (`node`, `esbuild`, `tee`).
    pub name: String,
    /// Command line, space-joined and truncated (see `CMD_MAX_CHARS` in the
    /// daemon's sampler). `None` when the platform withholds it — a zombie, or
    /// a process owned by another user.
    pub cmd: Option<String>,
    pub cpu_percent: f32,
    /// Cumulative CPU seconds this process has consumed since it started.
    pub cpu_seconds: f64,
    /// Resident set size for this process alone. Unlike the tree total, a
    /// single process's RSS double-counts nothing.
    pub memory_bytes: u64,
    pub memory: MemoryBreakdown,
    /// When the process started, when the platform reports it. Lets a reader
    /// tell "this PID is new" from "this PID was reused".
    #[serde(default, with = "chrono::serde::ts_milliseconds_option")]
    pub started_at: Option<DateTime<Utc>>,
}

impl ProcessSample {
    /// Read one memory metric off this process.
    pub fn memory_metric(&self, metric: MemoryMetric) -> Option<u64> {
        metric.read(self.memory_bytes, &self.memory)
    }
}

/// A node's whole process tree at one instant: the aggregate plus every process
/// that went into it, in pre-order (parents before children) so a renderer can
/// indent by `depth` without sorting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessTreeSample {
    pub total: ProcessStats,
    pub processes: Vec<ProcessSample>,
}

/// A time window plus the resolution to aggregate it at.
///
/// `bucket_secs` is derived by the caller from the window and a point budget
/// (see `StatsWindow::for_points`) rather than passed by a client directly, so
/// no request can ask for a million one-second buckets.
#[derive(Debug, Clone, Copy)]
pub struct StatsWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub bucket_secs: i64,
}

impl StatsWindow {
    /// A window covering `[start, end)` bucketed into at most `points` buckets.
    ///
    /// The bucket width is rounded *up*, so the caller's point budget is a hard
    /// ceiling: asking for 200 points over an hour yields 18s buckets (200 of
    /// them), never 3600 one-second ones. A window shorter than `points`
    /// seconds collapses to 1s buckets, which is finer than the 5s sample
    /// cadence and therefore just means "no aggregation".
    pub fn for_points(start: DateTime<Utc>, end: DateTime<Utc>, points: u32) -> Self {
        let span = (end - start).num_seconds().max(1);
        let points = points.clamp(1, 5_000) as i64;
        Self {
            start,
            end,
            // Ceiling division; both operands are >= 1 after the clamps above,
            // so this can't overflow or divide by zero. (`i64::div_ceil` is
            // still unstable.)
            bucket_secs: (span + points - 1) / points,
        }
    }
}

/// One bucket of aggregated history for a node.
///
/// A window is divided into fixed-width buckets and every sample inside a
/// bucket is averaged, so the payload size depends on the requested resolution
/// rather than on how long the run has been up. Averaging hides spikes, which
/// is why the peaks are carried alongside: a bucket whose `cpu_peak` is 400%
/// and whose `cpu_percent` is 30% is a very different node from a steady 30%.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StatsBucket {
    /// Start of the bucket (inclusive). Serialized as epoch milliseconds.
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub bucket_start: DateTime<Utc>,
    /// How many raw samples landed in this bucket. `0` never appears — empty
    /// buckets are omitted rather than zero-filled, because "the daemon wasn't
    /// sampling" and "the node used no memory" must not render the same.
    pub samples: u32,
    pub cpu_percent: f32,
    pub cpu_peak: f32,
    pub process_count: f32,
    pub memory_bytes: u64,
    pub memory: MemoryBreakdown,
    /// Peak footprint inside the bucket.
    pub footprint_peak: u64,
}

/// One process's aggregated history inside the same bucketing as
/// [`StatsBucket`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessSeries {
    pub pid: u32,
    /// The name last observed for this PID in the window. A reused PID reports
    /// whichever name it had most recently — hence [`ProcessSample`]'s warning
    /// about PID identity.
    pub name: String,
    pub cmd: Option<String>,
    pub buckets: Vec<StatsBucket>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_round_trips_through_str() {
        for m in MemoryMetric::ALL {
            assert_eq!(MemoryMetric::from_str(m.as_str()).unwrap(), *m);
        }
    }

    #[test]
    fn metric_accepts_hyphens_and_aliases() {
        assert_eq!(
            MemoryMetric::from_str("private-dirty").unwrap(),
            MemoryMetric::PrivateDirty
        );
        assert_eq!(
            MemoryMetric::from_str("RSS").unwrap(),
            MemoryMetric::Resident
        );
        assert!(MemoryMetric::from_str("nonsense").is_err());
    }

    #[test]
    fn absent_field_poisons_the_sum() {
        // A tree where one process could not report its page classes must not
        // report a too-small total as if it were complete.
        let mut total = MemoryBreakdown::zero_sum();
        total.add(&MemoryBreakdown {
            footprint: 10,
            virtual_bytes: 100,
            private_dirty: Some(4),
            ..Default::default()
        });
        assert_eq!(total.private_dirty, Some(4));
        assert_eq!(
            total.shared_clean, None,
            "absent in the addend → absent in the sum"
        );
        total.add(&MemoryBreakdown::basic(7, 70));
        assert_eq!(total.footprint, 17);
        assert_eq!(total.virtual_bytes, 170);
        assert_eq!(
            total.private_dirty, None,
            "a process with no page-class detail must poison the class totals"
        );
    }

    #[test]
    fn basic_falls_back_to_rss_and_reports_no_classes() {
        let b = MemoryBreakdown::basic(1234, 9999);
        assert_eq!(b.footprint, 1234);
        assert_eq!(b.virtual_bytes, 9999);
        assert!(!b.has_page_classes());
        assert_eq!(MemoryMetric::PrivateDirty.read(1234, &b), None);
        assert_eq!(MemoryMetric::Resident.read(1234, &b), Some(1234));
    }

    #[test]
    fn stack_metrics_do_not_include_totals() {
        // Stacking a total beside its own components would draw the same bytes
        // twice; keep the guard in a test so a future edit has to notice.
        for m in MemoryMetric::STACK {
            assert!(
                !matches!(
                    m,
                    MemoryMetric::Footprint | MemoryMetric::Resident | MemoryMetric::Virtual
                ),
                "{m} is a total, not a page class"
            );
        }
    }
}
