//! Per-platform memory and CPU-time detail for a single process.
//!
//! `sysinfo` gives every platform the same two memory numbers — RSS and virtual
//! size — and neither answers the questions a developer actually asks of a dev
//! server. RSS counts shared pages once per process, so summing it over a
//! `npm run dev` tree over-reports; and it cannot distinguish a process holding
//! 400 MB of its own dirty heap (a leak) from one mapping 400 MB of shared
//! binaries (free). Both facts are available from the OS, just not portably, so
//! this module owns the two platform paths and hands back the portable
//! [`MemoryBreakdown`].
//!
//! | | Linux | macOS |
//! |---|---|---|
//! | source | `/proc/<pid>/smaps_rollup`, `/proc/<pid>/stat` | `proc_pid_rusage(RUSAGE_INFO_V4)` |
//! | footprint | `Pss` — each shared page divided by the number of processes mapping it | `ri_phys_footprint` — what Activity Monitor calls "Memory" |
//! | page classes | `Private_{Clean,Dirty}`, `Shared_{Clean,Dirty}`, `Swap`, `Locked` | not exposed |
//! | cpu seconds | `utime + stime` / `_SC_CLK_TCK` | `ri_user_time + ri_system_time` (ns) |
//!
//! macOS has no unprivileged per-page-class split: it needs `task_for_pid` plus
//! a `mach_vm_region` walk, which requires root or a signing entitlement. So the
//! page-class fields stay `None` there rather than being guessed, and the UI
//! hides the "split by memory type" view instead of drawing an empty stack.
//!
//! Every probe is best-effort. A process that exits between the process-table
//! refresh and this read, a hardened kernel that hides `smaps_rollup`, a
//! `pid` owned by another user — all degrade to
//! [`MemoryBreakdown::basic`] (footprint = RSS, no classes) rather than failing
//! the sample. Losing detail for one process must not cost the whole node its
//! CPU and memory graph.

use std::sync::OnceLock;

use veld_core::stats::MemoryBreakdown;

/// What one probe learned about a process beyond what `sysinfo` reports.
pub struct ProcProbe {
    pub memory: MemoryBreakdown,
    /// Cumulative CPU time since the process started. `None` when the platform
    /// source was unreadable.
    pub cpu_seconds: Option<f64>,
}

/// Whether detailed probing is enabled. Off via `VELD_STATS_MEMORY_DETAIL=off`
/// (also `0`/`false`/`no`).
///
/// The escape hatch exists because the Linux path reads `smaps_rollup`, which
/// walks the process's VMA list under `mmap_lock`. That is cheap for a dev
/// server and measurably not cheap for a process with a pathological number of
/// mappings (a sanitizer build, a JVM with a huge heap) — and a monitoring
/// feature must never be the reason a node stalls. Cached once: the sampler
/// reads it every 5s and the answer cannot change within a daemon's life.
fn detail_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        match std::env::var("VELD_STATS_MEMORY_DETAIL") {
            Ok(v) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            ),
            // Absent → on. The detail is the feature; opting out is the
            // exception.
            Err(_) => true,
        }
    })
}

/// Probe `pid` for memory detail and cumulative CPU time.
///
/// `resident` and `virtual_bytes` come from the caller's `sysinfo` refresh and
/// are used as the fallback, so this never returns less than the portable
/// baseline.
pub fn probe(pid: u32, resident: u64, virtual_bytes: u64) -> ProcProbe {
    if !detail_enabled() {
        return ProcProbe {
            memory: MemoryBreakdown::basic(resident, virtual_bytes),
            cpu_seconds: None,
        };
    }
    platform::probe(pid, resident, virtual_bytes)
}

#[cfg(target_os = "linux")]
mod platform {
    use super::ProcProbe;
    use veld_core::stats::MemoryBreakdown;

    /// Clock ticks per second, for converting `/proc/<pid>/stat`'s CPU fields.
    /// Constant for the life of the machine, so resolve it once.
    fn clock_ticks() -> f64 {
        use std::sync::OnceLock;
        static TICKS: OnceLock<f64> = OnceLock::new();
        *TICKS.get_or_init(|| {
            // SAFETY: `sysconf` reads a static system parameter and has no
            // preconditions; a negative return means "indeterminate".
            let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            if t > 0 { t as f64 } else { 100.0 }
        })
    }

    pub(super) fn probe(pid: u32, resident: u64, virtual_bytes: u64) -> ProcProbe {
        let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
            .ok()
            .and_then(|text| parse_smaps_rollup(&text, virtual_bytes));
        let cpu = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|text| parse_stat_cpu_seconds(&text, clock_ticks()));
        ProcProbe {
            memory: rollup.unwrap_or_else(|| MemoryBreakdown::basic(resident, virtual_bytes)),
            cpu_seconds: cpu,
        }
    }

    /// Parse `/proc/<pid>/smaps_rollup` — the kernel's own pre-summed rollup of
    /// `smaps`, which is why this is affordable per-process per-tick where
    /// parsing `smaps` itself would not be.
    ///
    /// `None` when the file is absent (kernels before 4.14), unreadable, or the
    /// process exited mid-read. Absent-but-readable is impossible to distinguish
    /// from "no mappings", so an empty parse also returns `None`.
    fn parse_smaps_rollup(text: &str, virtual_bytes: u64) -> Option<MemoryBreakdown> {
        let mut pss = None;
        let mut private_clean = None;
        let mut private_dirty = None;
        let mut shared_clean = None;
        let mut shared_dirty = None;
        let mut swap = None;
        let mut locked = None;

        for line in text.lines() {
            let Some((key, rest)) = line.split_once(':') else {
                continue;
            };
            // Every size line is `<Key>:<spaces><number> kB`. Match on the exact
            // key: `Pss_Dirty` and `SwapPss` are siblings of the fields wanted
            // here and a prefix match would silently pick the wrong one.
            let slot = match key {
                "Pss" => &mut pss,
                "Private_Clean" => &mut private_clean,
                "Private_Dirty" => &mut private_dirty,
                "Shared_Clean" => &mut shared_clean,
                "Shared_Dirty" => &mut shared_dirty,
                "Swap" => &mut swap,
                "Locked" => &mut locked,
                _ => continue,
            };
            if let Some(kb) = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
            {
                // The kernel writes kB meaning KiB.
                *slot = Some(kb.saturating_mul(1024));
            }
        }

        // Pss is the whole point of reading this file. Without it there is no
        // honest footprint, so report no detail at all rather than a breakdown
        // whose headline number silently came from somewhere else.
        let footprint = pss?;
        Some(MemoryBreakdown {
            footprint,
            // Not in the rollup — /proc/<pid>/statm would have it, but sysinfo
            // already read it during the refresh this probe rides along with.
            virtual_bytes,
            private_clean,
            private_dirty,
            shared_clean,
            shared_dirty,
            swap,
            wired: locked,
        })
    }

    /// `utime + stime` from `/proc/<pid>/stat`, in seconds.
    ///
    /// The `comm` field is an arbitrary process name wrapped in parentheses and
    /// may itself contain spaces *and* parens (`(sh) -c (weird)`), so the fields
    /// after it are only findable by splitting at the **last** `)`. Splitting on
    /// whitespace from the left is the classic bug here and it silently reads a
    /// number out of the wrong column.
    fn parse_stat_cpu_seconds(text: &str, ticks: f64) -> Option<f64> {
        let tail = &text[text.rfind(')')? + 1..];
        let fields: Vec<&str> = tail.split_whitespace().collect();
        // `tail` starts at field 3 (`state`), so utime (field 14) is index 11
        // and stime (15) is index 12.
        let utime: u64 = fields.get(11)?.parse().ok()?;
        let stime: u64 = fields.get(12)?.parse().ok()?;
        Some((utime + stime) as f64 / ticks)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A real `smaps_rollup`, trimmed. Note `Pss_Dirty` and `SwapPss`: both
        /// are siblings whose names *begin with* a key this parser wants, which
        /// is why the match is on the exact key. A `starts_with` would read
        /// `Pss_Dirty` as `Pss` and `SwapPss` as `Swap`, silently reporting the
        /// wrong footprint — and both platforms' live tests would still pass.
        const ROLLUP: &str = "\
55a4c0000000-7ffd8f7ff000 ---p 00000000 00:00 0                          [rollup]
Rss:              102400 kB
Pss:               51200 kB
Pss_Dirty:         40960 kB
Pss_Anon:          30720 kB
Shared_Clean:      20480 kB
Shared_Dirty:       1024 kB
Private_Clean:      4096 kB
Private_Dirty:     76800 kB
Referenced:        98304 kB
Anonymous:         71680 kB
Swap:                512 kB
SwapPss:             256 kB
Locked:              128 kB
";

        #[test]
        fn parses_the_documented_keys_exactly() {
            let m = parse_smaps_rollup(ROLLUP, 4096).expect("Pss present");
            // kB in the file means KiB.
            assert_eq!(m.footprint, 51200 * 1024, "Pss, not Pss_Dirty or Pss_Anon");
            assert_eq!(m.private_dirty, Some(76800 * 1024));
            assert_eq!(m.private_clean, Some(4096 * 1024));
            assert_eq!(m.shared_dirty, Some(1024 * 1024));
            assert_eq!(m.shared_clean, Some(20480 * 1024));
            assert_eq!(m.swap, Some(512 * 1024), "Swap, not SwapPss");
            assert_eq!(m.wired, Some(128 * 1024));
            assert_eq!(m.virtual_bytes, 4096, "virtual comes from the caller");
            assert!(m.has_page_classes());
        }

        #[test]
        fn no_pss_means_no_detail_at_all() {
            // Without Pss there is no honest footprint, so the whole breakdown is
            // refused rather than reporting one whose headline came from RSS
            // while its classes came from the file.
            let without_pss = ROLLUP
                .lines()
                .filter(|l| !l.starts_with("Pss:"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(parse_smaps_rollup(&without_pss, 0).is_none());
            assert!(parse_smaps_rollup("", 0).is_none());
        }

        #[test]
        fn stat_cpu_survives_a_comm_containing_spaces_and_parens() {
            // The classic /proc/<pid>/stat bug: `comm` is arbitrary and wrapped
            // in parens, so fields are only findable from the LAST ')'.
            // Splitting from the left reads a number out of the wrong column.
            // Fields after comm: state(3) ppid(4) pgrp(5) session(6) tty(7)
            // tpgid(8) flags(9) minflt(10) cminflt(11) majflt(12) cmajflt(13)
            // utime(14) stime(15) → utime is index 11 of the tail.
            let tail = "S 1 2 3 4 5 6 7 8 9 10 400 100 0 0 20 0 1 0 100 0";
            for comm in ["node", "sh) -c (weird", "(nested)", "a b c", ")("] {
                let line = format!("1234 ({comm}) {tail}");
                let secs = parse_stat_cpu_seconds(&line, 100.0)
                    .unwrap_or_else(|| panic!("failed to parse comm {comm:?}"));
                // (utime 400 + stime 100) / 100 ticks = 5.0s.
                assert!(
                    (secs - 5.0).abs() < 1e-9,
                    "comm {comm:?} gave {secs}, expected 5.0 — a shifted field index \
                     would pick minflt/cminflt, which land in a plausible range"
                );
            }
        }

        #[test]
        fn stat_cpu_scales_by_the_clock_tick() {
            let line = "1 (x) S 1 2 3 4 5 6 7 8 9 10 400 100 0 0 20 0 1 0 100 0";
            assert!((parse_stat_cpu_seconds(line, 1000.0).unwrap() - 0.5).abs() < 1e-9);
        }

        #[test]
        fn malformed_stat_lines_degrade_instead_of_panicking() {
            for bad in ["", "no parens here", "1 (x)", "1 (x) S", "1 (x) S a b c"] {
                assert!(parse_stat_cpu_seconds(bad, 100.0).is_none(), "{bad:?}");
            }
        }

        #[test]
        fn reads_this_process_for_real() {
            // The live path, asserted rather than conditionally skipped: this is
            // the only Linux execution the page-class path gets, so wrapping it
            // in `if let Some(...)` would let it pass having checked nothing.
            // `smaps_rollup` has existed since kernel 4.14 (2017).
            let pid = std::process::id();
            let text = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
                .expect("/proc/self/smaps_rollup is readable on linux (kernel >= 4.14)");
            let m = parse_smaps_rollup(&text, 4096).expect("our own Pss cannot be missing");
            assert!(m.footprint > 0);
            assert!(
                m.has_page_classes(),
                "the 'by type' UI mode depends on this"
            );

            let stat =
                std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("/proc/self/stat");
            let secs = parse_stat_cpu_seconds(&stat, clock_ticks()).expect("own cpu time");
            assert!(secs >= 0.0 && secs < 86_400.0, "implausible: {secs}");
        }

        #[test]
        fn missing_pid_degrades_instead_of_panicking() {
            // PID 0 is never a real process in /proc.
            let p = probe(0, 4096, 8192);
            assert_eq!(p.memory.footprint, 4096, "falls back to the caller's RSS");
            assert!(p.cpu_seconds.is_none());
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::ProcProbe;
    use veld_core::stats::MemoryBreakdown;

    pub(super) fn probe(pid: u32, resident: u64, virtual_bytes: u64) -> ProcProbe {
        match rusage(pid) {
            Some(ri) => ProcProbe {
                memory: MemoryBreakdown {
                    // phys_footprint is the figure macOS itself reports as a
                    // process's memory cost — it excludes clean file-backed
                    // pages that RSS counts and includes compressed pages that
                    // RSS doesn't.
                    footprint: ri.ri_phys_footprint,
                    virtual_bytes,
                    // No unprivileged per-page-class split on macOS; see the
                    // module docs.
                    private_clean: None,
                    private_dirty: None,
                    shared_clean: None,
                    shared_dirty: None,
                    swap: None,
                    wired: Some(ri.ri_wired_size),
                },
                cpu_seconds: Some((ri.ri_user_time.saturating_add(ri.ri_system_time)) as f64 / 1e9),
            },
            None => ProcProbe {
                memory: MemoryBreakdown::basic(resident, virtual_bytes),
                cpu_seconds: None,
            },
        }
    }

    /// `proc_pid_rusage(pid, RUSAGE_INFO_V4, …)`.
    ///
    /// V4 is the oldest flavor carrying `ri_phys_footprint` and is available on
    /// every macOS version veld supports; the kernel fills only the requested
    /// flavor's prefix, so asking for a newer one on an older system is what
    /// would fail. `None` on any error — most often `ESRCH` for a process that
    /// exited between the process-table refresh and this call.
    fn rusage(pid: u32) -> Option<libc::rusage_info_v4> {
        // SAFETY: `buf` is a fully-initialized, correctly-sized V4 record, and
        // the flavor passed matches its type — the kernel writes at most
        // `sizeof(rusage_info_v4)` bytes into it. The cast mirrors the C call
        // (`rusage_info_t` is `void *`, and the parameter is `rusage_info_t *`).
        // `pid` is validated by the kernel, not by us.
        unsafe {
            let mut buf: libc::rusage_info_v4 = std::mem::zeroed();
            let rc = libc::proc_pid_rusage(
                pid as libc::c_int,
                libc::RUSAGE_INFO_V4,
                &mut buf as *mut libc::rusage_info_v4 as *mut libc::rusage_info_t,
            );
            if rc == 0 { Some(buf) } else { None }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn reads_own_footprint() {
            let ri = rusage(std::process::id()).expect("own rusage is readable");
            assert!(ri.ri_phys_footprint > 0, "our own footprint cannot be zero");
            let p = probe(std::process::id(), 1, 2);
            assert_eq!(p.memory.virtual_bytes, 2, "virtual comes from the caller");
            assert!(p.memory.footprint > 0);
            assert!(!p.memory.has_page_classes(), "macOS reports no page split");
            assert!(p.cpu_seconds.unwrap() >= 0.0);
        }

        #[test]
        fn missing_pid_degrades_to_basic() {
            // PID 0 is the kernel task; rusage on it is not permitted.
            let p = probe(0, 4096, 8192);
            assert_eq!(p.memory.footprint, 4096, "falls back to the caller's RSS");
            assert!(p.cpu_seconds.is_none());
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::ProcProbe;
    use veld_core::stats::MemoryBreakdown;

    /// veld ships for macOS and Linux only (see the release matrix in
    /// `.github/workflows/release.yml`). This arm keeps the crate compiling for
    /// anyone building elsewhere: no detail, RSS as the footprint.
    pub(super) fn probe(_pid: u32, resident: u64, virtual_bytes: u64) -> ProcProbe {
        ProcProbe {
            memory: MemoryBreakdown::basic(resident, virtual_bytes),
            cpu_seconds: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_never_reports_less_than_the_baseline() {
        // Whatever the platform manages, the caller's RSS/virtual survive as the
        // floor — a node's graph must not go blank because detail was
        // unavailable.
        let p = probe(std::process::id(), 4096, 8192);
        assert!(p.memory.footprint > 0);
        assert_eq!(p.memory.virtual_bytes, 8192);
    }
}
