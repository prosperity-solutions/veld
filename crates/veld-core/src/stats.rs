//! Per-node process resource statistics.
//!
//! A single sample of the resource usage of one run node — summed across the
//! node's process and its descendants (found by walking parent→child links),
//! so a `npm run dev` that forks a bundler and a server reports one combined
//! figure. Descendants that reparent away (a daemonizing double-fork ends up
//! under init/launchd) fall outside the tree and are not counted.
//!
//! Sampling lives in the daemon's stats sampler (see `veld-daemon`'s
//! `StatsCollector`, which owns the cross-platform `sysinfo` probing); this
//! crate only defines the shared data type and its persistence in
//! [`crate::db`]. Keeping the type here means the CLI can read stored samples
//! without pulling in `sysinfo`.

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

/// One resource-usage sample for a node's process tree at a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProcessStats {
    /// CPU usage as a percentage of a single core (may exceed 100 on a
    /// multi-threaded tree), summed across the process tree.
    pub cpu_percent: f32,
    /// Resident memory (RSS) in bytes, summed across the process tree.
    pub memory_bytes: u64,
    /// Number of live processes in the tree (the node's process + descendants).
    pub process_count: u32,
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
}
