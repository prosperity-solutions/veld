//! Is this machine on mains power or on its battery?
//!
//! Keep-awake asks because the honest answer differs by power source in two ways
//! at once. **What it may hold**: `caffeinate -s` — the flag that survives a shut
//! lid — is documented as valid on AC power only, so on mains the widest hold
//! costs nothing and needs no privileged helper, while on battery the same
//! coverage exists only behind `pmset disablesleep`, a durable system setting.
//! **How long it may hold it**: an automatic hold on mains spends nothing, and
//! the same hold on battery spends somebody's charge.
//!
//! Both questions are cheap to answer and neither needs root:
//!
//! | Platform | Source |
//! |---|---|
//! | macOS | `pmset -g batt`, whose first line names the source in words |
//! | Linux | `/sys/class/power_supply/*`, where a `Mains` supply carries `online` |
//!
//! **An unknown answer is treated as battery**, which is the conservative
//! direction for both consumers rather than for one: the automatic half gets the
//! shorter cap and the narrower coverage, and the manual half still asks for the
//! privileged lease it would need if the guess is right. Reading it the other way
//! would hold an unasked-for inhibition for hours on a laptop that is discharging.
//!
//! A machine with **no battery at all** is on mains by construction. That is a
//! separate fact from the current source and it is reported separately, because
//! the settings dialog uses it to hide two rows that can never apply — a desktop
//! offering a "while sharing, on battery" switch is a control that does nothing.

use std::time::Duration;

use tokio::process::Command;

/// Where this machine's power is coming from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSource {
    Mains,
    Battery,
}

impl PowerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mains => "mains",
            Self::Battery => "battery",
        }
    }
}

/// What a power reading tells us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Power {
    pub source: PowerSource,
    /// Whether this machine has a battery at all. `false` for a desktop, and for
    /// any machine we could not ask.
    pub has_battery: bool,
}

impl Power {
    /// The answer to use when nothing could be measured. See the module docs for
    /// why the unknown case is battery rather than mains.
    ///
    /// `has_battery` is **true** here, and the two fields are conservative in
    /// opposite-looking directions on purpose because different consumers read
    /// them. The source drives what is held, where guessing battery spends
    /// nothing. `has_battery` drives whether the settings dialog *offers* the
    /// battery rows — and a machine we could not ask is not a machine we know is
    /// a desktop, so claiming `false` would hide the very switch the cup is
    /// writing to while the daemon applies the battery cap.
    const fn unknown() -> Self {
        Self {
            source: PowerSource::Battery,
            has_battery: true,
        }
    }

    const fn mains_only() -> Self {
        Self {
            source: PowerSource::Mains,
            has_battery: false,
        }
    }
}

/// How long we wait for the platform to answer.
///
/// This runs on the session lock's critical path via the expiry tick, so a
/// `pmset` that has wedged must cost a bounded moment and then fall back, rather
/// than stalling the status every open window is polling.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Read the current power source.
pub async fn read() -> Power {
    if cfg!(target_os = "macos") {
        read_macos().await
    } else if cfg!(target_os = "linux") {
        read_linux()
    } else {
        // Nothing here can keep a machine awake anyway (`inhibitor_argv` refuses
        // first), so the value is never acted on — it only has to be harmless.
        Power::mains_only()
    }
}

async fn read_macos() -> Power {
    let out = tokio::time::timeout(
        PROBE_TIMEOUT,
        Command::new("/usr/bin/pmset").args(["-g", "batt"]).output(),
    )
    .await;
    match out {
        Ok(Ok(out)) if out.status.success() => parse_pmset(&String::from_utf8_lossy(&out.stdout)),
        _ => Power::unknown(),
    }
}

/// Parse `pmset -g batt`.
///
/// Its first line is prose — `Now drawing from 'AC Power'` or
/// `Now drawing from 'Battery Power'` — and a battery, when present, is listed
/// underneath as an `InternalBattery` entry. Matching the quoted words rather
/// than the whole sentence is deliberate: the surrounding wording has changed
/// across macOS releases, the two quoted names have not.
fn parse_pmset(stdout: &str) -> Power {
    let has_battery = stdout.contains("InternalBattery");
    let source = if stdout.contains("'AC Power'") {
        PowerSource::Mains
    } else if stdout.contains("'Battery Power'") {
        PowerSource::Battery
    } else {
        // Understood nothing about the source. `has_battery` is still whatever
        // the output actually showed — that half was readable even when this one
        // was not, and it is what decides whether Settings offers the battery
        // rows at all.
        return Power {
            source: PowerSource::Battery,
            has_battery,
        };
    };
    Power {
        source,
        has_battery,
    }
}

/// Read `/sys/class/power_supply`.
///
/// Blocking file reads, deliberately not `spawn_blocking`: these are sysfs
/// pseudo-files whose contents are generated in the kernel on read, with no
/// device I/O behind them, so the cost is a handful of microseconds. The macOS
/// side spawns a process and *is* worth awaiting.
fn read_linux() -> Power {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return Power::unknown();
    };
    let mut has_battery = false;
    let mut mains_online = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        let kind = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
        match kind.trim() {
            "Battery" => has_battery = true,
            // `Mains` is the AC adapter; `USB` covers USB-PD charging, which is
            // how a modern laptop is usually plugged in and would otherwise read
            // as "on battery" while charging.
            "Mains" | "USB" => {
                let online = std::fs::read_to_string(dir.join("online")).unwrap_or_default();
                // Any supply reporting online wins: a machine with both a barrel
                // jack and a USB-C port has two, and only one of them is in use.
                if online.trim() == "1" {
                    mains_online = Some(true);
                } else {
                    mains_online.get_or_insert(false);
                }
            }
            _ => {}
        }
    }
    match (mains_online, has_battery) {
        // A supply said so.
        (Some(true), _) => Power {
            source: PowerSource::Mains,
            has_battery,
        },
        (Some(false), _) => Power {
            source: PowerSource::Battery,
            has_battery,
        },
        // No mains supply and no battery: a desktop, a server, a VM. Nothing to
        // conserve, so nothing to be conservative about.
        (None, false) => Power::mains_only(),
        // A battery and no adapter listed at all. Unusual, and the one shape where
        // guessing mains would be actively wrong.
        (None, true) => Power {
            source: PowerSource::Battery,
            has_battery: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Power, PowerSource, parse_pmset};

    #[test]
    fn a_mac_on_its_charger_reads_as_mains_and_still_reports_its_battery() {
        let out = "Now drawing from 'AC Power'\n \
                   -InternalBattery-0 (id=23003235)\t83%; charging; 1:12 remaining present: true\n";
        assert_eq!(
            parse_pmset(out),
            Power {
                source: PowerSource::Mains,
                has_battery: true,
            }
        );
    }

    #[test]
    fn a_mac_on_its_battery_reads_as_battery() {
        // Verbatim from a real `pmset -g batt`, which is the only reason to trust
        // the quoted-name match at all.
        let out = "Now drawing from 'Battery Power'\n \
                   -InternalBattery-0 (id=23003235)\t83%; discharging; 7:11 remaining present: true\n";
        assert_eq!(
            parse_pmset(out),
            Power {
                source: PowerSource::Battery,
                has_battery: true,
            }
        );
    }

    #[test]
    fn a_desktop_mac_reads_as_mains_with_no_battery() {
        assert_eq!(
            parse_pmset("Now drawing from 'AC Power'\n"),
            Power {
                source: PowerSource::Mains,
                has_battery: false,
            }
        );
    }

    #[test]
    fn output_naming_no_recognised_source_falls_back_to_battery() {
        // The direction that matters: an unparseable answer must never buy a
        // longer hold or a wider one than the machine has earned.
        assert_eq!(
            parse_pmset("something else entirely\n").source,
            PowerSource::Battery
        );
    }

    #[test]
    fn a_battery_is_still_reported_when_the_source_line_is_not_understood() {
        // `has_battery` drives whether the settings dialog offers the battery rows
        // at all, so it must survive a source we could not read — otherwise a
        // laptop whose `pmset` output changed shape loses two settings.
        let parsed = parse_pmset("-InternalBattery-0 (id=1)\t50%; discharging\n");
        assert!(parsed.has_battery);
        assert_eq!(parsed.source, PowerSource::Battery);
    }
}
