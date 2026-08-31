use crate::output;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use veld_core::helper_gate::GateSource;

/// `veld doctor` — comprehensive system diagnostics.
pub async fn run(json: bool) -> i32 {
    let mut diag = Diagnostics::default();
    diag.gather().await;

    if json {
        println!("{}", serde_json::to_string_pretty(&diag.to_json()).unwrap());
    } else {
        diag.print();
    }

    if diag.checks.iter().any(|c| !c.pass) {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Diagnostics {
    // Installation
    binary_path: String,
    binary_version: String,
    helper_path: String,
    helper_version: String,
    daemon_path: String,
    daemon_version: String,
    caddy_path: String,
    /// macOS only; empty elsewhere and on a machine with no app.
    desktop_app: String,
    caddy_exists: bool,
    lib_dir: String,
    config_path: String,
    config_mode: String,
    /// Where the daemon's own diagnostics go, and whether they are actually
    /// getting there. Printed unconditionally rather than only on failure: the
    /// row exists so that "check the daemon log" is a followable instruction,
    /// and the moment someone needs it is the moment they are already stuck.
    daemon_log: String,
    /// Where Caddy's own log goes. Same reasoning as `daemon_log`, and one step
    /// more load-bearing: certificate issuance and renewal happen entirely
    /// inside Caddy, veld cannot report on them, and this file is the only place
    /// they are written down.
    caddy_log: String,

    // Services
    helper_status: String,
    daemon_status: String,
    caddy_status: String,
    ca_status: String,
    /// The certificate a browser actually gets from the HTTPS port, and how long
    /// it is still good for.
    ///
    /// The `ca_status` line above answers a different question — whether this
    /// machine *trusts* veld's CA — and answering only that is what let a Caddy
    /// serving a leaf certificate two weeks past its expiry report all-green
    /// while Chrome refused every veld URL with `ERR_CERT_DATE_INVALID`. Empty
    /// when there was no HTTPS port to ask.
    cert_status: String,
    /// Whether anything is keeping this machine awake, and why.
    ///
    /// The helper line above already reports the *privileged* half, and that was
    /// only ever half the answer: the ordinary unprivileged hold — the one every
    /// machine can take, and the one a live share now arms by itself — appeared
    /// in no CLI output at all. So "why won't this Mac sleep", asked from a
    /// support transcript with no window open, had no answer for the common case
    /// and a partial one for the rare case. Empty when nothing is held.
    keep_awake: String,

    // Checks
    checks: Vec<Check>,

    /// The update that is running right now, if one is.
    ///
    /// Reported first and loudest, because during an update most of the rest of
    /// this report is *expected* to look broken: the helper and daemon are being
    /// restarted onto new binaries and their versions disagree with the CLI's for
    /// tens of seconds at a time. Without this line, `veld doctor` during an
    /// update is a page of red that invites someone to "fix" a machine that is
    /// already fixing itself. It is deliberately **not** a `Check` — an update in
    /// flight is not a failure, and pushing a failing check would make
    /// `veld doctor` exit non-zero for it.
    update_in_progress: Option<veld_core::update_lock::UpdateState>,

    // Tip
    tip: String,
}

struct Check {
    pass: bool,
    label: String,
}

/// The real user's uid — `SUDO_UID` when running under sudo (the services
/// live in the invoking user's gui domain, not root's), else `id -u`.
fn current_uid() -> Option<String> {
    if let Ok(uid) = std::env::var("SUDO_UID") {
        if !uid.is_empty() {
            return Some(uid);
        }
    }
    let out = std::process::Command::new("id").arg("-u").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!uid.is_empty()).then_some(uid)
}

/// [`current_uid`] as a number, for comparing against a uid the helper reports.
///
/// `None` when it cannot be resolved. Note that a plain root shell answers `0`
/// here, which callers must read as "no opinion about which user this is for"
/// rather than as a uid to match against — `SUDO_UID` is what makes a root
/// invocation know who it is standing in for.
fn invoking_uid() -> Option<u64> {
    current_uid()?.parse().ok()
}

impl Diagnostics {
    async fn gather(&mut self) {
        self.update_in_progress = veld_core::update_lock::current();
        self.gather_installation();
        self.gather_services().await;
        self.gather_checks().await;
        self.gather_tip();
    }

    // -- Installation --------------------------------------------------------

    fn gather_installation(&mut self) {
        let cli_version = env!("CARGO_PKG_VERSION").to_string();

        // Binary
        self.binary_path = std::env::current_exe()
            .map(|p| tilde_path(&p))
            .unwrap_or_else(|_| "unknown".to_string());
        self.binary_version = cli_version.clone();

        // Lib dir
        let lib = veld_core::paths::lib_dir();
        self.lib_dir = tilde_path(&lib);

        // Helper
        let helper_bin = lib.join("veld-helper");
        self.helper_path = tilde_path(&helper_bin);
        self.helper_version =
            query_binary_version(&helper_bin).unwrap_or_else(|| "not found".into());

        // Daemon
        let daemon_bin = lib.join("veld-daemon");
        self.daemon_path = tilde_path(&daemon_bin);
        self.daemon_version =
            query_binary_version(&daemon_bin).unwrap_or_else(|| "not found".into());

        // Caddy
        let caddy = veld_core::paths::caddy_bin();
        self.caddy_path = tilde_path(&caddy);
        self.caddy_exists = caddy.exists();

        // Config
        let config_path = dirs::home_dir()
            .map(|h| h.join(".veld").join("setup.json"))
            .unwrap_or_else(|| PathBuf::from("~/.veld/setup.json"));
        self.config_path = tilde_path(&config_path);
        self.config_mode = read_mode(&config_path);

        // Veld Desktop. Optional on macOS, updated by `veld update` when the user
        // wants it, and able to be stale in a way nothing else here would show —
        // the app half can lag while every binary above is current. Doctor is
        // where a user is sent when something is wrong, so a stale app belongs in
        // the list rather than only in `veld desktop status`.
        //
        // The recorded preference is part of the row rather than a separate one,
        // because "the app is old" and "the app is old *because you opted out*"
        // are the same fact with and without its explanation — and without it a
        // reader chases a stale app that is stale on purpose.
        let opted_out = veld_core::desktop_pref::read()
            == Some(veld_core::desktop_pref::DesktopChoice::Unwanted);
        self.desktop_app = match veld_core::setup::desktop_app_status() {
            Some((path, version)) => {
                let version = version.unwrap_or_else(|| "unknown version".to_string());
                let suffix = if opted_out {
                    " — you opted out, so veld leaves it alone ('veld desktop install' to opt in)"
                        .to_string()
                } else if version == cli_version {
                    String::new()
                } else {
                    format!(" — CLI is {cli_version}, run 'veld desktop update'")
                };
                format!("{} ({version}){suffix}", tilde_path(&path))
            }
            None if std::env::consts::OS == "macos" && opted_out => {
                "not installed — you opted out ('veld desktop install' to opt in)".to_string()
            }
            None if std::env::consts::OS == "macos" => {
                "not installed ('veld desktop install')".to_string()
            }
            // Elsewhere veld does not install the app, so it reports rather than
            // judges — and says nothing at all when it found nothing, because an
            // AppImage lives wherever the user saved it and "not found" would
            // read as "not installed".
            None => match veld_core::setup::desktop_app_linux() {
                Some(path) => format!("{} (managed by your package manager)", tilde_path(&path)),
                None => String::new(),
            },
        };

        // Read after the mode, because the repair it suggests names it.
        self.daemon_log = daemon_log_row(&daemon_bin, &self.config_mode);

        // Caddy's own log. Unlike the daemon's, no service manager captures it:
        // the helper spawns Caddy with its output discarded and Caddy writes this
        // file itself, so it simply does not exist until a Caddy new enough to be
        // configured for it has run. The path comes from `veld_core::paths` rather
        // than being derived here, so this row cannot name a different file from
        // the one the helper configured.
        let caddy_log = veld_core::paths::caddy_log_path();
        self.caddy_log = if caddy_log.exists() {
            tilde_path(&caddy_log)
        } else {
            format!(
                "{} (not written yet — restart Caddy to start it)",
                tilde_path(&caddy_log)
            )
        };
    }

    // -- Services ------------------------------------------------------------

    async fn gather_services(&mut self) {
        // Helper — connect to the socket for the CONFIGURED mode so we report on
        // the right helper. Plain `connect()` falls through system -> user, which
        // in privileged mode would report a stray user/auto helper on :18443 when
        // the privileged LaunchDaemon is actually down, masking the real failure
        // (and contradicting the mode-aware errors from `veld start`/`veld update`).
        let helper_conn = match super::read_setup_mode().as_deref() {
            Some("privileged") => veld_core::helper::HelperClient::connect_to(
                &veld_core::helper::system_socket_path(),
            )
            .await,
            Some("unprivileged") => {
                veld_core::helper::HelperClient::connect_to(&veld_core::helper::user_socket_path())
                    .await
            }
            _ => veld_core::helper::HelperClient::connect().await,
        };
        match helper_conn {
            Ok(client) => {
                let status_data = client.status().await.ok().and_then(|r| r.data);

                // Both ports as the helper itself reports them. The HTTP one used
                // to be *derived* from the HTTPS one, which is a guess about a
                // value that is right there in the same response.
                let port_info = status_data
                    .as_ref()
                    .and_then(|d| d.get("https_port"))
                    .and_then(|v| v.as_u64())
                    .map(|https| {
                        let http = status_data
                            .as_ref()
                            .and_then(|d| d.get("http_port"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(if https == 443 {
                                80
                            } else {
                                veld_core::instance::UNPRIVILEGED_HTTP_PORT as u64
                            });
                        format!("port {https}/{http}")
                    })
                    .unwrap_or_default();

                let helper_pid = status_data
                    .as_ref()
                    .and_then(|d| d.get("helper_pid"))
                    .and_then(|v| v.as_u64())
                    .map(|p| format!("pid {p}"))
                    .unwrap_or_default();

                let caddy_pid = status_data
                    .as_ref()
                    .and_then(|d| d.get("caddy_pid"))
                    .and_then(|v| v.as_u64())
                    .map(|p| format!("pid {p}"))
                    .unwrap_or_default();

                // Whether the helper is holding the machine's sleep setting.
                //
                // Worth a line here specifically because it is the one thing veld
                // does that outlives the process doing it: `pmset disablesleep` is
                // durable, so "why won't this Mac sleep" is a question somebody
                // asks hours later, from a support transcript, with the IDE
                // closed. The keep-awake menu answers it only while a window is
                // open, and only for a lease *this daemon* took — a lease taken
                // straight on the helper socket appears nowhere else at all.
                let holding_sleep = status_data
                    .as_ref()
                    .and_then(|d| d.get("sleep_disabled"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let sleep_hold = if holding_sleep {
                    "holding sleep off"
                } else {
                    ""
                };

                let parts: Vec<&str> = [helper_pid.as_str(), port_info.as_str(), sleep_hold]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect();
                if parts.is_empty() {
                    self.helper_status = "running".to_string();
                } else {
                    self.helper_status = format!("running ({})", parts.join(", "));
                }

                // Caddy status from helper's perspective
                let caddy_running = status_data
                    .as_ref()
                    .and_then(|d| d.get("caddy"))
                    .and_then(|v| v.as_str())
                    == Some("running");
                if caddy_running {
                    let caddy_parts: Vec<&str> = [caddy_pid.as_str()]
                        .iter()
                        .filter(|s| !s.is_empty())
                        .copied()
                        .collect();
                    if caddy_parts.is_empty() {
                        self.caddy_status = "running (admin API on 2019, sentinel OK)".to_string();
                    } else {
                        self.caddy_status = format!(
                            "running ({}, admin API on 2019, sentinel OK)",
                            caddy_parts.join(", ")
                        );
                    }
                }
            }
            Err(_) => {
                self.helper_status = "not running".to_string();
            }
        }

        // Daemon
        self.daemon_status = check_daemon_status().await;
        self.keep_awake = check_keep_awake().await;

        // Caddy (only check independently if helper didn't report it)
        if self.caddy_status.is_empty() || self.caddy_status == "not running" {
            self.caddy_status = check_caddy_status().await;
        }

        // CA
        self.ca_status = check_ca_status();
    }

    // -- Checks --------------------------------------------------------------

    /// Verify the mode-appropriate helper is actually owned by its service
    /// manager (launchd/systemd), not an unmanaged direct-spawned process.
    /// Three-state: pid match → pass; job absent or pid mismatch → fail
    /// (orphan); query failure/timeout → no check emitted, since a transient
    /// launchctl/systemctl hiccup must not tell users to redo setup.
    async fn check_helper_managed(&mut self, privileged: bool) {
        let socket = if privileged {
            veld_core::helper::system_socket_path()
        } else {
            veld_core::helper::user_socket_path()
        };
        let Ok(client) = veld_core::helper::HelperClient::connect_to(&socket).await else {
            return; // helper down — already reported by the socket check
        };
        let Some(socket_pid) = client
            .status()
            .await
            .ok()
            .and_then(|r| r.data)
            .and_then(|d| d.get("helper_pid").and_then(|v| v.as_u64()))
        else {
            return; // pre-`helper_pid` helper — can't verify
        };

        let verdict = if cfg!(target_os = "macos") {
            let label = veld_core::setup::HELPER_LABEL_MACOS;
            let domain = if privileged {
                "system".to_string()
            } else {
                let Some(uid) = current_uid() else { return };
                format!("gui/{uid}")
            };
            // Job absent is the orphan signal; distinguish it from a wedged
            // launchctl (query returns None) which proves nothing.
            match veld_core::setup::launchd_job_pid(&domain, label).await {
                Some(pid) => Some(pid == socket_pid as u32),
                // In unprivileged mode "no job in gui/<uid>" is NOT proof of an
                // orphan: a legacy `load -w` agent (headless-session fallback)
                // is genuinely managed but invisible to that query. Only the
                // system domain gives a definitive answer.
                None if !privileged => None,
                None => match veld_core::setup::launchd_job_registered(&domain, label).await {
                    Some(false) => Some(false), // no job at all: unmanaged orphan
                    // Registered without readable pid, or query failed/timed
                    // out — inconclusive either way.
                    Some(true) | None => None,
                },
            }
            .map(|m| (m, "launchd"))
        } else {
            // Same three-state on Linux: `systemctl show` succeeding with
            // MainPID=0 means the unit exists but is NOT running this helper —
            // orphan. Only a failed/timed-out query is inconclusive.
            match veld_core::setup::systemd_pid_query(
                veld_core::setup::HELPER_SERVICE_LINUX,
                !privileged,
            )
            .await
            {
                Some(Some(pid)) => Some(pid == socket_pid as u32),
                Some(None) => Some(false),
                None => None,
            }
            .map(|m| (m, "systemd"))
        };

        let Some((managed, manager)) = verdict else {
            return;
        };
        let setup_cmd = if privileged {
            "veld setup privileged"
        } else {
            "veld setup unprivileged"
        };
        self.checks.push(Check {
            pass: managed,
            label: if managed {
                format!("Helper managed by {manager}")
            } else {
                format!("Helper running but NOT managed by {manager} — run `{setup_cmd}`")
            },
        });
    }

    /// Whether the privileged helper's socket is actually gated to one uid, and
    /// to the *right* one.
    ///
    /// The socket is world-writable (`0o777`) so the unprivileged CLI can reach
    /// it, so an ungated privileged helper takes `shutdown` — which stops Caddy
    /// and drops every live URL — from *any* local process. Until #337 that
    /// state passed every other row in this command: green-but-unprotected, the
    /// worst thing a diagnostic can say.
    ///
    /// Asked of the running helper, never of the service definition. A helper
    /// with no `--allow-uid` in its plist derives the uid from its own install
    /// directory at startup, so the plist and the truth routinely disagree —
    /// and the direction they disagree in is the one that would report a gated
    /// helper as exposed.
    ///
    /// Keyed on the socket answering rather than on `setup.json` saying
    /// "privileged", because `setup.json` is per-user: the person a
    /// newly-derived gate locks out is by construction *not* the installing
    /// user and has no such file, so keying on the mode marker would silence
    /// this row for exactly the reader who needs it.
    ///
    /// The cost of that choice is a second reader it cannot tell apart: a user
    /// on a shared machine whose privileged veld legitimately belongs to
    /// somebody else. They reach the 0o777 socket, are refused, and are equally
    /// entitled to an explanation — but the remedy that fits the first reader
    /// would have them rewrite a system service that is not theirs. So the
    /// refusal row names both readings and tells the second one to do nothing.
    async fn check_helper_uid_gate(&mut self) {
        let socket = veld_core::helper::system_socket_path();
        let reported = match veld_core::helper::HelperClient::connect_to(&socket).await {
            Ok(client) => match client.status().await.ok().and_then(|r| r.data) {
                Some(data) => GateReport::Status(data),
                None => return,
            },
            // The gate refused *us*. `connect_to` probes with a `status`, and a
            // rejected peer gets an `ok:false` reply, so a live-but-refusing
            // helper is indistinguishable from a dead one by the error *kind* —
            // which is why the socket row above says "not reachable" and
            // `veld start` spends 20s waiting for a helper that is already up.
            // This is the one failure mode deriving the uid can introduce, so it
            // is the one this row most has to name.
            Err(veld_core::helper::HelperError::CommandError(e))
                if e.contains(veld_core::helper_gate::REJECTED_PEER_ERROR) =>
            {
                GateReport::Refused
            }
            // Nothing is listening. The socket row above already says so, and a
            // helper that is down has no gate to report.
            Err(veld_core::helper::HelperError::ConnectionFailed { .. }) => return,
            // Something accepted the connection and then the exchange broke.
            //
            // This is the refusal above arriving by its other route, and the
            // reason this arm exists at all: `reject_connection` writes its
            // refusal and drops the stream **without reading the request**, so
            // if that close wins the race against our own write we get
            // `SendFailed(EPIPE)` instead of the message. Both `reject_connection`
            // and `REJECTED_PEER_ERROR` are explicit that the reply is
            // best-effort and must never be depended on — so this row does not
            // depend on it. It claims only what is certain in both cases: a
            // helper answered, and the gate could not be read.
            Err(_) => GateReport::Unreadable,
        };

        self.checks.push(gate_check(&reported, invoking_uid()));
    }

    async fn gather_checks(&mut self) {
        // 0. Central database opens and is at a supported schema version.
        // Everything (run state, logs, feedback, tokens) lives here now, so a
        // corrupt/locked/newer-than-supported database must be visible in the
        // one diagnostic command.
        //
        // Skipped under sudo: `Db::open()` creates the file (and -wal/-shm)
        // if missing, and root's data dir is not the user's — running it as
        // root would either check the wrong database or leave root-owned
        // files that break every later non-sudo `veld` command.
        if std::env::var("SUDO_UID").is_ok_and(|u| !u.is_empty()) {
            self.checks.push(Check {
                pass: true,
                label: "Database check skipped under sudo (run `veld doctor` without sudo)".into(),
            });
        } else {
            let path = veld_core::db::Db::default_path()
                .map(|p| tilde_path(&p))
                .unwrap_or_else(|_| "unknown".into());
            match veld_core::db::Db::open() {
                Ok(db) => {
                    let version = db.schema_version().unwrap_or(0);
                    let size = veld_core::db::Db::default_path()
                        .ok()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .map(|m| format!("{:.1} MB", m.len() as f64 / 1_048_576.0))
                        .unwrap_or_else(|| "?".into());
                    // **Opening is not health, and this row used to claim it
                    // was.** A database with one damaged page opens fine, reads
                    // fine everywhere the damage is not, and reported "Database
                    // OK" here for the whole 17 hours of the incident that
                    // produced this check — while the daemon logged the same
                    // corruption 440 times. `quick_check` is what actually
                    // asks, and it costs ~15 ms on a real database.
                    match db.integrity() {
                        Ok(veld_core::db::Integrity::Ok) => self.checks.push(Check {
                            pass: true,
                            label: format!("Database OK ({path}, schema v{version}, {size})"),
                        }),
                        Ok(veld_core::db::Integrity::Damaged(detail)) => {
                            self.checks.push(Check {
                                pass: false,
                                label: format!(
                                    "Database DAMAGED ({path}, schema v{version}, {size}): \
                                     {} — see the backup row below, then \
                                     `veld backup restore`",
                                    detail.lines().next().unwrap_or(&detail).trim()
                                ),
                            });
                        }
                        // The check itself could not run for a reason that is not
                        // damage (a lock, a permission). Not a pass — but not a
                        // corruption claim either.
                        Err(e) => self.checks.push(Check {
                            pass: false,
                            label: format!(
                                "Database could not be checked ({path}, schema v{version}): {e}"
                            ),
                        }),
                    }
                }
                Err(e) => {
                    // **What to advise depends on what went wrong.** Restoring a
                    // backup is the answer to a damaged file and emphatically not
                    // to a full disk or a volume that has been remounted
                    // read-only — those clear on their own, and discarding every
                    // change since the last copy to "fix" one is a real loss. The
                    // daemon's restore endpoint refuses for exactly this reason
                    // (`dbhealth::corruption_recorded`), so a doctor row that
                    // sent the user to the CLI for the same condition would be
                    // this tool contradicting the product.
                    let advice = match e.fault() {
                        Some(veld_core::db::DbFault::Io) => {
                            "the storage underneath is refusing — check free space and \
                             whether the volume is mounted read-only, then try again \
                             before restoring anything"
                        }
                        _ => "see the backup row below, then `veld backup restore`",
                    };
                    self.checks.push(Check {
                        pass: false,
                        label: format!("Database not usable at {path}: {e} — {advice}"),
                    });
                }
            }

            // 0b. Backups. Reported unconditionally, pass or fail, for the same
            // reason the daemon-log row is: the moment somebody needs to know
            // whether there is a copy of their state is the moment they are
            // already in trouble, and "is there one, how old, what schema" is not
            // a question to answer by hunting for a directory.
            let backups = check_backups();
            self.checks.push(backups);
        }

        // 1. Helper socket reachable
        let helper_ok = veld_core::helper::HelperClient::connect().await.is_ok();
        self.checks.push(Check {
            pass: helper_ok,
            label: if helper_ok {
                "Helper socket reachable".into()
            } else {
                "Helper socket not reachable".into()
            },
        });

        // In managed modes, a reachable socket is not enough: a helper spawned
        // outside launchd/systemd (setup's direct-spawn fallback) serves the
        // socket fine but nothing relaunches it after a crash, reboot, or binary
        // update. Catch that state while everything still looks healthy.
        let mode = super::read_setup_mode();
        if matches!(mode.as_deref(), Some("privileged") | Some("unprivileged")) {
            self.check_helper_managed(mode.as_deref() == Some("privileged"))
                .await;
        }

        // The privileged helper's fail-closed signing gate (#261) refuses to
        // relaunch onto an on-disk binary without a valid org .sig — so a
        // signature mismatch means the next `veld update` will sit on the old
        // version. Check the SERVICE-pinned binary (the one the gate actually
        // verifies via `current_exe()`), not a lib-dir guess: on a machine
        // carrying both ~/.local and /usr/local, the guess would accuse a file
        // the helper never runs.
        if mode.as_deref() == Some("privileged") {
            let path = veld_core::setup::privileged_helper_program()
                .await
                .unwrap_or_else(|| std::path::PathBuf::from(self.helper_path.clone()));
            match veld_core::signing::relaunch_guard(&path) {
                None => self.checks.push(Check {
                    pass: true,
                    label: format!("Helper binary signature OK ({})", tilde_path(&path)),
                }),
                Some(reason) => self.checks.push(Check {
                    pass: false,
                    label: format!(
                        "{reason} — the running helper is still the genuine one, but it will \
                         refuse to update onto this file. Re-deploy a signed helper with \
                         `veld update` (or `veld setup privileged`). Do NOT force a restart: \
                         `launchctl kill`/`systemctl restart` would run this unverified binary \
                         as root, which is exactly what the gate is refusing"
                    ),
                }),
            }
        }

        // Deliberately outside the `mode == privileged` branch above: see
        // `check_helper_uid_gate`. The row emits nothing at all when no
        // privileged helper answers the system socket, so an unprivileged or
        // auto install stays silent without needing the marker to say so.
        self.check_helper_uid_gate().await;

        // Determine HTTPS port for later checks. When the helper is down we
        // can't ask it, so fall back based on the configured mode — privileged
        // serves on 443; probing 18443 there checks a port nothing should be
        // listening on and misdiagnoses a healthy Caddy.
        let mode = super::read_setup_mode();
        let fallback_port: u16 = if mode.as_deref() == Some("privileged") {
            443
        } else {
            veld_core::instance::UNPRIVILEGED_HTTPS_PORT
        };
        let https_port: u16 = if let Ok(client) = veld_core::helper::HelperClient::connect().await {
            client.https_port().await.unwrap_or(fallback_port)
        } else {
            fallback_port
        };

        // 2. Caddy admin API responds
        let caddy_api = http_get_ok("http://localhost:2019/config/").await;
        self.checks.push(Check {
            pass: caddy_api,
            label: if caddy_api {
                "Caddy admin API responds".into()
            } else {
                "Caddy admin API not responding".into()
            },
        });

        // 3. Caddy sentinel verified
        let sentinel = http_get_ok("http://localhost:2019/id/veld-sentinel").await;
        self.checks.push(Check {
            pass: sentinel,
            label: if sentinel {
                "Caddy sentinel verified".into()
            } else {
                "Caddy sentinel not found".into()
            },
        });

        // 4. HTTPS port listening
        let https_ok = tcp_connect_ok("127.0.0.1", https_port).await;
        self.checks.push(Check {
            pass: https_ok,
            label: if https_ok {
                format!("HTTPS port listening ({})", https_port)
            } else {
                format!("HTTPS port not listening ({})", https_port)
            },
        });

        // 5. The certificate a browser would be handed.
        //
        // Only worth asking when something is listening — otherwise this would
        // spend the probe's connect timeout to restate the red row above. Every
        // other TLS check veld had was about the *CA*: whether this machine
        // trusts the authority that signs the leaves. None of them looked at a
        // leaf, which is how a Caddy serving one that expired a day earlier
        // passed `veld doctor` while Chrome refused every URL it serves.
        if https_ok {
            let cert = veld_core::tls_health::probe(https_port).await;
            self.cert_status = describe_cert(&cert);
            self.checks.push(Check {
                // Green means both halves: a browser loads the page today, *and*
                // renewal is still on schedule. A certificate deep in its
                // renewal window is still servable and is already proof that
                // renewal stopped — see `TlsHealth::renewal_is_overdue`.
                pass: cert.serves_browsers() && !cert.renewal_is_overdue(),
                label: cert_check_label(veld_core::instance::MANAGEMENT_HOST, &cert),
            });
        }

        // 6. Feedback server responding. Name the port so a contributor
        // running a dev instance (VELD_DAEMON_PORT) can tell WHICH daemon
        // this green/red check is about.
        let daemon_port = veld_core::instance::daemon_port();
        let feedback_ok = tcp_connect_ok("127.0.0.1", daemon_port).await;
        let instance_note = if daemon_port == veld_core::instance::DEFAULT_DAEMON_PORT {
            String::new()
        } else {
            format!(" (dev instance, port {daemon_port})")
        };
        self.checks.push(Check {
            pass: feedback_ok,
            label: if feedback_ok {
                format!("Feedback server responding{instance_note}")
            } else {
                format!("Feedback server not responding{instance_note}")
            },
        });

        // 7. .localhost DNS resolves
        let dns_ok = resolve_localhost_dns();
        self.checks.push(Check {
            pass: dns_ok,
            label: if dns_ok {
                ".localhost DNS resolves".into()
            } else {
                ".localhost DNS does not resolve".into()
            },
        });

        // 8. No stale system install
        let stale_path = Path::new("/usr/local/lib/veld");
        let lib = veld_core::paths::lib_dir();
        // Only warn if the system dir exists AND it's not the active lib dir
        let has_stale = stale_path.exists() && lib != stale_path;
        self.checks.push(Check {
            pass: !has_stale,
            label: if has_stale {
                format!("Stale system install at {}", stale_path.display())
            } else {
                "No stale system install".into()
            },
        });

        // 9. No stale binaries next to CLI (e.g. ~/.local/bin/veld-daemon
        //    left over from manual testing while lib dir has the real copy)
        if let Ok(cli_path) = std::env::current_exe() {
            if let Some(cli_dir) = cli_path.parent() {
                for name in ["veld-daemon", "veld-helper"] {
                    let sibling = cli_dir.join(name);
                    let canonical = lib.join(name);
                    // Only flag if both exist and they're different files
                    if sibling.exists() && canonical.exists() && sibling != canonical {
                        let sib_ver =
                            query_binary_version(&sibling).unwrap_or_else(|| "unknown".into());
                        let lib_ver =
                            query_binary_version(&canonical).unwrap_or_else(|| "unknown".into());
                        let stale = sib_ver != lib_ver;
                        self.checks.push(Check {
                            pass: !stale,
                            label: if stale {
                                format!(
                                    "Stale {} at {} ({}) — lib has {}. Remove with: rm {}",
                                    name,
                                    tilde_path(&sibling),
                                    sib_ver,
                                    lib_ver,
                                    tilde_path(&sibling),
                                )
                            } else {
                                format!("No stale {} next to CLI", name)
                            },
                        });
                    }
                }
            }
        }

        // 10. Terminal holder processes.
        //
        // Each open terminal in `/ide` has a process of its own holding its PTY,
        // which is what lets a shell survive `veld update`. They are invisible to
        // every other check here — not the daemon, not a run — so "my terminal
        // died after an update" had nowhere to look. This names the directory and
        // counts what is in it; a socket nobody answers is a holder that is gone
        // and gets swept at the next daemon start.
        self.checks.push(self.terminal_holders_check(feedback_ok));

        // 11. The terminal-URL shims.
        //
        // Same reason as the row above: this is a feature whose failure mode is
        // *silence*. The daemon writes these once at startup and logs a single
        // warning if it cannot, and a `OnceLock` means it never tries again — so a
        // machine with no `veld` beside the daemon, or a `~/.veld` it cannot write,
        // has terminal URL opening switched off with nothing anywhere to say so.
        self.checks.push(self.terminal_shims_check().await);
    }

    /// Which generated file catches `open`/`xdg-open` for this shell, and what to
    /// call the mechanism — or `None` when veld has none for it.
    ///
    /// Returns a path *relative to the shim directory*, so the caller reports the
    /// same `~/.veld/...` prefix either way. The bash arm is **probed**, not assumed
    /// from the basename: macOS ships bash 3.2 as `/bin/bash`, which ignores `$ENV`
    /// in posix mode, so the file exists and would never be read — reporting it as
    /// the working mechanism would be the exact silent-success this row exists to
    /// prevent.
    async fn handoff_for(shell: &str) -> Option<(String, &'static str)> {
        match veld_core::shell::kind(shell) {
            veld_core::shell::Kind::Zsh => Some(("zdotdir/.zshenv".to_owned(), "ZDOTDIR")),
            veld_core::shell::Kind::Bash
                if veld_core::shell::supports_posix_env_handoff(shell).await =>
            {
                Some(("bash/veldenv.bash".to_owned(), "$ENV"))
            }
            _ => None,
        }
    }

    /// A shell's basename, for a message a human reads.
    fn shell_name(shell: &str) -> String {
        std::path::Path::new(shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(shell)
            .to_owned()
    }

    /// Whether a terminal can route a URL into a Veld browser pane.
    ///
    /// Reports the directory, and passes only if the `veld-open` script the session's
    /// `$BROWSER` points at exists *and* names an executable CLI. Both halves matter
    /// and neither is visible from anywhere else: the script is generated from the
    /// binary sitting beside the running daemon, so an install that moved, an
    /// interrupted update, or a daemon started from a directory with no sibling `veld`
    /// all end with a `$BROWSER` that cannot work — and the only other signal is one
    /// `warn!` in the daemon log at boot.
    ///
    /// A note rather than a failure: every other part of a terminal still works, and
    /// clicking a URL in the output still opens it in the system browser.
    async fn terminal_shims_check(&self) -> Check {
        // **Skipped whole under sudo**, exactly as check 0 skips itself, and for both
        // of its reasons rather than one. `Db::open()` creates the file, so as root it
        // leaves root-owned `veld.db`/`-wal`/`-shm` that break every later non-sudo
        // `veld` command — and `shim_dir()` is `HOME`-derived and no more
        // `SUDO_USER`-aware than the database path is, so the row would then report on
        // *root's* `~/.veld`, find nothing there, and turn a passing `veld doctor` into
        // a failing one. Guarding only the database left the second half live; this is
        // one condition covering both, because two guards for one hazard is how the
        // first version of this got it half right.
        if std::env::var("SUDO_UID").is_ok_and(|u| !u.is_empty()) {
            return Check {
                pass: true,
                label: "Terminal URL check skipped under sudo (run `veld doctor` without sudo)"
                    .to_owned(),
            };
        }
        let dir = veld_core::instance::shim_dir();
        let shown = tilde_path(&dir);
        // The scripts are written at every daemon start regardless of the settings, so
        // their presence says nothing about whether the feature is *on*. Reporting
        // "Terminal URLs open in Veld" while the user has switched it off is a green
        // answer to the exact question someone runs this to ask.
        let db = veld_core::db::Db::open().ok();
        let settings = db.as_ref().map(|db| {
            (
                db.terminal_open_urls_in_app(),
                db.terminal_intercept_system_open(),
            )
        });
        // Which shell the handoff has to be *for*. `terminal.shell` decides which
        // mechanism applies, and each has its own file — so a row that only ever
        // looked for the zsh one reported "not caught" to every bash user and
        // "caught" to a zsh user who had switched to fish.
        let shell = db
            .as_ref()
            .map(|db| db.terminal_shell())
            .unwrap_or_else(veld_core::shell::auto_shell);
        if let Some((false, _)) = settings {
            return Check {
                pass: true,
                label: "Terminal URLs open in your system browser (terminal.openUrlsInApp is off)"
                    .to_owned(),
            };
        }
        let browser = dir.join("veld-open");
        if !browser.is_file() {
            // Two different failures reach here and they have different repairs, so
            // the row has to say which one it is. The daemon needs a `veld` CLI it
            // can name by absolute path (`paths::cli_for_exe`): when one is there,
            // the scripts are simply older than it and a restart writes them; when
            // one is not, restarting achieves nothing and the install is what is
            // wrong. The previous wording asked for neither and pointed at a daemon
            // log that macOS was discarding — three unfollowable instructions for a
            // check whose whole job is to be followable.
            // Resolved from the directory the **running** daemon lives in, which is
            // the plist's `ProgramArguments` when it names one and `lib_dir()`
            // otherwise. Not `lib_dir()` alone: a legacy `/usr/local` install with a
            // leftover `~/.local/lib/veld` (install.sh's cleanup removes the three
            // binaries and leaves `caddy-data`, so the directory survives and
            // `lib_dir()` prefers it) would have this row resolve a CLI the daemon
            // never considered and print "restart it" for a daemon that will keep
            // writing nothing. Doctor asking a different question than the daemon is
            // the exact defect this row exists to report.
            let daemon_dir = daemon_program_dir().unwrap_or_else(veld_core::paths::lib_dir);
            let cause = match veld_core::paths::cli_for_exe(&daemon_dir) {
                Some(_) => format!("Restart the daemon to write it: {}", restart_daemon_hint()),
                None => {
                    let looked: Vec<String> = veld_core::paths::cli_candidates(&daemon_dir)
                        .iter()
                        .map(|p| tilde_path(p))
                        .collect();
                    // Reinstall, or a symlink — and not `veld setup`, which writes
                    // service definitions and records nothing about where the CLI
                    // is, so suggesting it here would be a repair that cannot
                    // work. A custom `VELD_INSTALL_DIR` is the case that lands
                    // here on a healthy install: the CLI is fine, it is simply
                    // somewhere the daemon has no way to derive.
                    format!(
                        "the daemon found no veld CLI to point it at (looked for {}). \
                         Reinstall with `veld update`, or symlink your veld binary to \
                         one of those paths if it lives somewhere else",
                        looked.join(" and ")
                    )
                }
            };
            return Check {
                pass: false,
                label: format!(
                    "Terminal URL opening is off: {shown}/veld-open is missing. {cause}"
                ),
            };
        }
        // The baked path is the load-bearing half: the script tests it with `-x` at
        // run time precisely because it can go away under an upgrade.
        let baked = std::fs::read_to_string(&browser).unwrap_or_default();
        let cli = baked
            .lines()
            .find_map(|l| l.split('\'').nth(1))
            .map(std::path::PathBuf::from);
        match cli {
            Some(cli) if cli.is_file() => {
                let intercept = settings.map(|(_, i)| i).unwrap_or(true);
                if !intercept {
                    // Names the shell even here. Someone debugging "my fish
                    // aliases do not load" is asking about the shell, not about
                    // URL routing, and this row is the only place `veld doctor`
                    // reports which shell terminals actually open — so dropping
                    // the name on this branch sends them to read the setting in
                    // the UI to learn something the diagnostic already knew.
                    return Check {
                        pass: true,
                        label: format!(
                            "Terminal URLs open in Veld ({shown} → {}); terminals run {}, open/xdg-open not caught",
                            cli.display(),
                            Self::shell_name(&shell)
                        ),
                    };
                }
                match Self::handoff_for(&shell).await {
                    // The file that performs the handoff is missing, and for zsh that
                    // is worse than the feature being off: `ZDOTDIR` redirects every
                    // zsh startup file, so a missing `.zshenv` there means none of the
                    // user's own zsh config runs. The daemon checks this per session
                    // too; this row makes the state visible before a terminal opens.
                    Some((file, _)) if !dir.join(&file).is_file() => Check {
                        pass: false,
                        label: format!(
                            "Terminal URLs open in Veld, but {shown}/{file} is missing, so open/xdg-open are not caught. Restart the daemon to rewrite it: {}",
                            restart_daemon_hint()
                        ),
                    },
                    Some((_, mechanism)) => Check {
                        pass: true,
                        label: format!(
                            "Terminal URLs open in Veld ({shown} → {}, open/xdg-open caught in {} via {mechanism})",
                            cli.display(),
                            Self::shell_name(&shell)
                        ),
                    },
                    // No mechanism for this shell — fish, nushell, or a bash too old
                    // to honour `$ENV` (macOS ships 3.2 as /bin/bash). A note, not a
                    // failure: `$BROWSER` still works, and the one line that closes
                    // the gap is the point of saying anything at all.
                    None => Check {
                        pass: true,
                        label: format!(
                            "Terminal URLs open in Veld ({shown} → {}), but open/xdg-open are not caught in {}. Add to your shell's startup file: PATH=\"$VELD_SHIM_DIR:$PATH\"",
                            cli.display(),
                            Self::shell_name(&shell)
                        ),
                    },
                }
            }
            Some(cli) => Check {
                pass: false,
                label: format!(
                    "Terminal URL opening is off: {shown}/veld-open points at {}, \
                     which is not there. Restart the daemon to rewrite it: {}",
                    cli.display(),
                    restart_daemon_hint()
                ),
            },
            None => Check {
                pass: false,
                label: format!(
                    "Terminal URL opening is off: {shown}/veld-open names no veld \
                     binary. Restart the daemon to rewrite it: {}",
                    restart_daemon_hint()
                ),
            },
        }
    }

    /// Count holder sockets, and — only when it is safe to ask — how many of them
    /// answer.
    ///
    /// **`daemon_up` decides whether this row probes at all, and that is the
    /// point.** Connecting is the only way to tell a live holder from a leftover
    /// socket file, and a holder treats a connection as a daemon arriving. On a
    /// holder from *this* build that costs nothing (a connection has to stay open
    /// for `TAKEOVER_PROBATION` before it displaces anything) — but the holders
    /// that matter most are the ones already running when that fix ships, which
    /// keep the old binary until their shell exits and still hand the session to
    /// anything that connects. That is the bug this command was the reported
    /// trigger for: one `veld doctor` disconnected every terminal on the machine.
    ///
    /// So the probe is skipped exactly when it could hurt: a reachable daemon is
    /// attached to these holders, and the count it would have confirmed is
    /// something the daemon already knows. With no daemon there is nothing to
    /// displace, and that is also the state this row exists to explain — "my
    /// terminals died, is anything still holding them?"
    ///
    /// One consequence to know rather than to fix: a holder with no daemon
    /// attached treats the probe as a daemon arriving and leaving, which re-arms
    /// its orphan grace. So running this on a machine whose daemon is down gives
    /// every orphaned shell another full grace before it hangs itself up. That is
    /// the friendly direction to be wrong in — somebody diagnosing a dead daemon
    /// is the last person who should lose a shell to the diagnosis.
    ///
    /// The row doubles as the check that a holder *can* bind here at all. A
    /// `sockaddr_un` path is capped at 104 bytes and this directory's is not
    /// something the user chose — it follows `$HOME` and the daemon port — so a
    /// deep enough home overruns it. Reporting "No terminal holders" in that state
    /// is true and useless: there are none because none can start, and the bare
    /// `bind` error ("path must be shorter than SUN_LEN") only ever reaches the
    /// user as a terminal that will not open. So the length is checked first, and
    /// it is a *failure* rather than a note — nothing works until it is fixed.
    fn terminal_holders_check(&self, daemon_up: bool) -> Check {
        Self::holders_row(&veld_core::instance::pty_dir(), daemon_up)
    }

    /// The body of [`Self::terminal_holders_check`], taking its directory rather
    /// than reading `VELD_PTY_DIR` — so a test can hand it a real listener in a
    /// tempdir and observe whether this knocks on it, which is the behaviour worth
    /// pinning and is not observable through the environment without a lock.
    fn holders_row(dir: &Path, daemon_up: bool) -> Check {
        let shown = tilde_path(dir);
        // The name a real socket would get: `socket_for` digests the session id to
        // a fixed 16 hex chars precisely so every session's path is the same length,
        // which is what makes one probe answer for all of them.
        let probe = dir.join("0000000000000000.sock");
        if veld_core::instance::socket_path_too_long(&probe).is_some() {
            // The label is written here rather than reused from
            // `socket_path_too_long`, whose message embeds the absolute path — right
            // for the holder's own bind failure, wrong for this command, where every
            // other displayed path goes through `tilde_path` and doctor output is
            // what people paste into an issue.
            return Check {
                pass: false,
                label: format!(
                    "No terminal can start: the holder socket path is {} bytes, over the \
                     {}-byte limit a unix socket allows ({}/<id>.sock). Set VELD_PTY_DIR \
                     to a shorter directory.",
                    probe.as_os_str().as_encoded_bytes().len(),
                    veld_core::instance::MAX_SOCKET_PATH,
                    shown,
                ),
            };
        }
        // `holder_sockets_in`, not a `read_dir` of its own: the directory is
        // `VELD_PTY_DIR`, which is a plain environment variable, and pointed one
        // level up at `~/.veld` a `.sock`-extension test counts `daemon.sock` and
        // `helper.sock` as terminals — and, worse, connects to them. A missing
        // directory is an empty list, which is the ordinary state: it appears with
        // the first terminal and nothing prunes it.
        let sockets = veld_core::instance::holder_sockets_in(dir);
        if sockets.is_empty() {
            return Check {
                pass: true,
                label: format!("No terminal holders ({shown})"),
            };
        }
        if daemon_up {
            // Counted, not probed — see the note above this function.
            return Check {
                pass: true,
                label: format!(
                    "{} terminal holder(s) ({shown}) — not probed while the daemon is running",
                    sockets.len()
                ),
            };
        }
        let (mut live, mut stale) = (0usize, 0usize);
        for path in sockets {
            // Connect-and-close. Safe here because no daemon is attached to these
            // holders: there is no connection for this one to displace.
            match std::os::unix::net::UnixStream::connect(&path) {
                Ok(stream) => {
                    drop(stream);
                    live += 1;
                }
                Err(_) => stale += 1,
            }
        }
        Check {
            // Not a failure: a stale socket is swept at the next daemon start.
            pass: true,
            label: if stale == 0 {
                format!("{live} terminal holder(s) running ({shown})")
            } else {
                format!("{live} terminal holder(s) running, {stale} stale socket(s) ({shown})")
            },
        }
    }

    // -- Tip -----------------------------------------------------------------

    fn gather_tip(&mut self) {
        let all_pass = self.checks.iter().all(|c| c.pass);
        if self.config_mode == "privileged" && all_pass {
            self.tip = "All checks passed.".to_string();
        } else if !all_pass {
            self.tip = "Some checks failed — see above for details.".to_string();
        } else {
            self.tip = String::new(); // Mode section already shows the upgrade hint
        }
    }

    // -- Output --------------------------------------------------------------

    fn print(&self) {
        println!("{}", output::bold("Veld Doctor"));
        println!();

        // Before anything else: see the field's own comment for why this outranks
        // the installation block it would otherwise sit inside.
        if let Some(state) = &self.update_in_progress {
            println!("  {}", output::bold("Update in progress"));
            println!("    {}", state.describe(chrono::Utc::now()));
            if let Some(tty) = &state.tty {
                println!("    {}", output::dim(&format!("Terminal: {tty}")));
            }
            println!(
                "    {}",
                output::dim(
                    "Versions and service status below may disagree with each other until it \
                     finishes. Follow it with `veld update --status`."
                )
            );
            println!();
        }

        // Installation
        println!("  {}", output::bold("Installation"));
        println!(
            "    {:<14}{} (v{})",
            "Binary:", self.binary_path, self.binary_version
        );
        println!(
            "    {:<14}{} ({})",
            "Helper:", self.helper_path, self.helper_version
        );
        println!(
            "    {:<14}{} ({})",
            "Daemon:", self.daemon_path, self.daemon_version
        );
        if self.caddy_exists {
            println!("    {:<14}{}", "Caddy:", self.caddy_path);
        } else {
            println!("    {:<14}{} (not found)", "Caddy:", self.caddy_path);
        }
        if !self.desktop_app.is_empty() {
            println!("    {:<14}{}", "Desktop app:", self.desktop_app);
        }
        println!("    {:<14}{}", "Lib dir:", self.lib_dir);
        println!("    {:<14}{}", "Config:", self.config_path);
        println!("    {:<14}{}", "Daemon log:", self.daemon_log);
        println!("    {:<14}{}", "Caddy log:", self.caddy_log);
        println!();

        // Mode (prominent)
        println!("  {}", output::bold("Mode"));
        match self.config_mode.as_str() {
            "privileged" => {
                println!(
                    "    {} {}",
                    output::checkmark(),
                    output::green("Privileged — clean URLs on ports 80/443")
                );
            }
            "unprivileged" => {
                println!(
                    "    {} Unprivileged — HTTPS on port 18443",
                    output::cyan("●")
                );
                println!(
                    "      {}",
                    output::dim("Run `veld setup privileged` for clean URLs without :18443")
                );
            }
            "auto" => {
                println!(
                    "    {} Auto-bootstrapped — HTTPS on port 18443",
                    output::cyan("●")
                );
                println!(
                    "      {}",
                    output::dim("Run `veld setup privileged` for clean URLs without :18443")
                );
            }
            _ => {
                println!(
                    "    {} {}",
                    output::cross(),
                    output::red(
                        "Not configured — run `veld setup unprivileged` or `veld setup privileged`"
                    )
                );
            }
        }
        println!();

        // Services
        println!("  {}", output::bold("Services"));
        println!(
            "    {:<14}{}",
            "Helper:",
            colorize_status(&self.helper_status)
        );
        println!(
            "    {:<14}{}",
            "Daemon:",
            colorize_status(&self.daemon_status)
        );
        if !self.keep_awake.is_empty() {
            println!("    {:<14}{}", "Keep awake:", self.keep_awake);
        }
        println!(
            "    {:<14}{}",
            "Caddy:",
            colorize_status(&self.caddy_status)
        );
        println!("    {:<14}{}", "CA:", colorize_status(&self.ca_status));
        if !self.cert_status.is_empty() {
            println!(
                "    {:<14}{}",
                "Certificate:",
                colorize_status(&self.cert_status)
            );
        }
        println!();

        // Checks
        println!("  {}", output::bold("Checks"));
        for check in &self.checks {
            if check.pass {
                println!("    {} {}", output::checkmark(), check.label);
            } else {
                println!("    {} {}", output::cross(), output::red(&check.label));
            }
        }
        println!();

        // Tip (only if there's something to say)
        if !self.tip.is_empty() {
            println!("  {}", output::dim(&self.tip));
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let checks: Vec<serde_json::Value> = self
            .checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "pass": c.pass,
                    "label": c.label,
                })
            })
            .collect();

        serde_json::json!({
            "installation": {
                "binary_path": self.binary_path,
                "binary_version": self.binary_version,
                "helper_path": self.helper_path,
                "helper_version": self.helper_version,
                "daemon_path": self.daemon_path,
                "daemon_version": self.daemon_version,
                "caddy_path": self.caddy_path,
                "caddy_exists": self.caddy_exists,
                "desktop_app": self.desktop_app,
                "lib_dir": self.lib_dir,
                "config_path": self.config_path,
                "config_mode": self.config_mode,
                "daemon_log": self.daemon_log,
                "caddy_log": self.caddy_log,
            },
            "services": {
                "helper": self.helper_status,
                "daemon": self.daemon_status,
                "caddy": self.caddy_status,
                "ca": self.ca_status,
                // Empty string when there was no HTTPS port to ask, for the
                // same reason as `keep_awake` above.
                "certificate": self.cert_status,
                // Empty string rather than null when nothing is held: this is a
                // human-readable phrase, and a consumer branching on it wants
                // "is there one" rather than a shape to destructure.
                "keep_awake": self.keep_awake,
            },
            "checks": checks,
            // `null` when nothing is updating, so a consumer can branch on
            // presence rather than on a sentinel.
            "update": self.update_in_progress.as_ref().map(|s| serde_json::json!({
                "pid": s.pid,
                "origin": s.origin.as_str(),
                "version": s.version,
                "phase": s.phase.as_str(),
                "started_at": s.started_at.to_rfc3339(),
                "tty": s.tty,
            })),
            "tip": self.tip,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The command that restarts the daemon on this machine.
///
/// Spelled out rather than "restart the daemon", because there is no
/// `veld daemon restart` to name and the service manager's own incantation is not
/// something a user should have to derive — on macOS it needs the uid of the gui
/// domain the agent was bootstrapped into.
fn restart_daemon_hint() -> String {
    if cfg!(target_os = "macos") {
        let uid = current_uid().unwrap_or_else(|| "$(id -u)".to_owned());
        format!("launchctl kickstart -k gui/{uid}/dev.veld.daemon")
    } else {
        "systemctl --user restart veld-daemon".to_owned()
    }
}

/// Which `veld setup <mode>` to suggest.
///
/// `privileged` for a machine that is in that mode, since suggesting the other one
/// would change the install rather than repair it. `unprivileged` for **everything
/// else**, and that is a choice rather than a passthrough: `auto` and the literal
/// "not configured" `read_mode` returns for a machine that never ran setup both land
/// here, and neither is a mode `veld setup` takes. `unprivileged` is the no-sudo mode
/// such a machine should be in, and it is what install.sh tells it to run.
fn setup_mode_arg(mode: &str) -> &'static str {
    match mode {
        "privileged" => "privileged",
        _ => "unprivileged",
    }
}

/// The `Daemon log:` row — where the daemon's diagnostics go, or why they go
/// nowhere.
///
/// On macOS a launchd job's stdout and stderr are discarded unless its plist names
/// a file, and the daemon's plist did not until this row's sibling change added it.
/// So the honest answer for an install set up by an older version is "not
/// configured", with the command that fixes it — anything else sends someone to
/// `tail` a file that will never exist. systemd captures a unit's output on its
/// own, so on Linux the answer is the query rather than a path.
///
/// `mode` is the setup mode from `setup.json`; `veld setup` is per-mode and running
/// the wrong one would change the install rather than repair it.
fn daemon_log_row(daemon_bin: &Path, mode: &str) -> String {
    if cfg!(target_os = "linux") {
        return "journalctl --user -u veld-daemon".to_owned();
    }
    if !cfg!(target_os = "macos") {
        return "n/a".to_owned();
    }
    // **Skipped under sudo**, for the reason the terminal-shim check is: every path
    // this row reads is `HOME`-derived and none of it is `SUDO_USER`-aware. As root the
    // plist at `/var/root/Library/LaunchAgents` is absent and `read_mode` finds no
    // `setup.json`, so a healthy privileged install would be reported as "not
    // captured" *and* handed `veld setup unprivileged` — a command that converts the
    // install rather than repairing it. A row that cannot see the user's home says so
    // instead of guessing.
    if std::env::var("SUDO_UID").is_ok_and(|u| !u.is_empty()) {
        return "not checked under sudo (run `veld doctor` without it)".to_owned();
    }
    // The path the plist actually names, rather than "does it mention the one we
    // would derive". The two differ on a developer machine — `just
    // dev-install-daemon` copies the binary to wherever the plist already pointed —
    // and, more importantly, on a machine where `veld setup` deliberately wrote no
    // log keys because the job could not have created the file.
    match daemon_plist()
        .as_deref()
        .and_then(|xml| launchd_string_after_key(xml, "StandardErrorPath"))
    {
        Some(path) => tilde_path(Path::new(&path)),
        None => format!(
            "not captured — run `veld setup {}` to enable it ({})",
            setup_mode_arg(mode),
            veld_core::paths::daemon_log_path()
                .map(|p| tilde_path(&p))
                .unwrap_or_else(|| daemon_bin.display().to_string())
        ),
    }
}

/// The daemon LaunchAgent's plist, as text.
fn daemon_plist() -> Option<String> {
    let path = dirs::home_dir()?.join("Library/LaunchAgents/dev.veld.daemon.plist");
    std::fs::read_to_string(path).ok()
}

/// The directory holding the daemon binary launchd was told to run.
///
/// `None` when there is no plist to read or it names nothing — the caller then falls
/// back to `lib_dir()`, which is a guess, and says so by being the fallback.
fn daemon_program_dir() -> Option<PathBuf> {
    let xml = daemon_plist()?;
    let program = launchd_string_after_key(&xml, "ProgramArguments")?;
    Path::new(&program).parent().map(Path::to_path_buf)
}

/// The first `<string>` value after `<key>NAME</key>` in a plist.
///
/// A deliberate two-token scan rather than a plist parser: these are two optional
/// rows of a diagnostic, the file is one veld wrote itself, and a dependency to read
/// it would be paid for by every `veld` invocation. `None` covers every shape this
/// cannot read — no key, no `<string>` after it, an empty value, a binary plist —
/// and every caller treats `None` as "unknown", which is also the right answer when
/// the file is unreadable.
///
/// Works for `ProgramArguments` only because its first `<string>` is the program;
/// that is the same reading launchd does, and an argument-bearing daemon plist is
/// not something veld writes.
fn launchd_string_after_key(xml: &str, key: &str) -> Option<String> {
    let after_key = xml.split(&format!("<key>{key}</key>")).nth(1)?;
    let open = after_key.find("<string>")? + "<string>".len();
    let close = after_key[open..].find("</string>")? + open;
    let value = after_key[open..close].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Replace the home directory prefix with `~`.
/// How a *gated* helper says where its uid came from. Kept short — the row is
/// already a pass, and the interesting half is the uid.
fn gate_source_label(source: Option<GateSource>) -> &'static str {
    match source {
        Some(GateSource::Flag) => "from the service definition",
        Some(GateSource::LibDirOwner) => "derived from the install directory's owner",
        // Spelled out rather than a `_` arm, so a new `GateSource` fails to
        // build here until somebody decides how a gated helper reporting it
        // should read. The three below cannot accompany a uid, and `None` is a
        // source only a newer helper knows — both say nothing rather than guess.
        Some(
            GateSource::RefusedRootLibDir | GateSource::UnreadableLibDir | GateSource::Unprivileged,
        )
        | None => "source unknown",
    }
}

/// Appended to every *failing* gate row.
///
/// The row runs for whoever types `veld doctor`, because the reader a
/// newly-derived gate locks out is by construction not the installing user (see
/// [`Diagnostics::check_helper_uid_gate`]). The cost is that a second reader —
/// someone on a shared machine whose privileged veld legitimately belongs to
/// another account — sees the same red row, and every remedy it could offer is
/// wrong for them: `veld setup privileged` resolves the binary through
/// `which_self`, so following it would leave a root LaunchDaemon exec'ing a
/// binary out of *their* home directory and gate the socket to them, locking the
/// real owner out. `veld update` is merely futile — it cannot touch somebody
/// else's root service, so the row would stay red forever.
///
/// Nothing here can tell the two readers apart, so every failing row says so.
/// One sentence of noise for the owner is the right price for not handing the
/// other reader a hijack.
const NOT_YOUR_INSTALL: &str = " If this machine's privileged veld belongs to another account, \
                                none of this is yours to fix — leave it to its owner.";

/// What the privileged helper told `veld doctor` about its gate.
///
/// A type rather than a bare `Option<Value>` so the *refused* answer — which
/// arrives as a connection error, not as data — travels the same path as every
/// other outcome and cannot be forgotten by a future branch.
enum GateReport {
    /// The gate rejected this caller before it could ask anything, and said so.
    Refused,
    /// Something answered on the system socket but the exchange did not
    /// complete. Indistinguishable from [`Self::Refused`] arriving without its
    /// (deliberately best-effort) message, so the row must not assert either.
    Unreadable,
    /// The helper's `status` payload.
    Status(serde_json::Value),
}

/// The gate row, decided purely from what the helper reported.
///
/// Split from [`Diagnostics::check_helper_uid_gate`] so every outcome is
/// reachable in a test without a root helper and a socket. That matters more
/// here than in most rows: this one is the only place an exposed root socket is
/// reported at all, and its failure mode is a *confident wrong sentence* rather
/// than a crash — which no amount of running `veld doctor` on a healthy machine
/// would reveal.
///
/// Always produces a row: by the time a [`GateReport`] exists, the helper has
/// said something, and every something has an honest rendering. The decision to
/// stay silent belongs to the caller, which makes it before it has a report at
/// all — see [`Diagnostics::check_helper_uid_gate`].
fn gate_check(report: &GateReport, invoking: Option<u64>) -> Check {
    let data = match report {
        GateReport::Refused => {
            let me = invoking
                .map(|u| format!("uid {u}"))
                .unwrap_or_else(|| "this user".into());
            return Check {
                pass: false,
                label: format!(
                    "Helper socket is gated to a DIFFERENT uid — it refused {me}, so no \
                     `veld` command of yours can drive it. If this machine's privileged \
                     veld belongs to another account, that is working as intended and \
                     there is nothing for you to fix here — do NOT run `veld setup \
                     privileged`, which would repoint the system service at your install \
                     and lock the owner out. If the install IS yours, its directory is \
                     owned by another account (a restored backup, or a renumbered user); \
                     `veld setup privileged` writes the correct uid explicitly."
                ),
            };
        }
        GateReport::Unreadable => {
            return Check {
                pass: false,
                label: format!(
                    "Helper socket uid gate cannot be confirmed — something is listening on \
                     the privileged helper's socket but the check did not complete. If the \
                     gate refused this connection, no `veld` command of yours can drive that \
                     helper; run `veld doctor` again to tell a refusal from a restart.\
                     {NOT_YOUR_INSTALL}"
                ),
            };
        }
        GateReport::Status(data) => data,
    };

    // Three states, and conflating any two of them tells somebody something
    // false. Absent is a helper predating #337, which may still be gated by an
    // `--allow-uid` its plist carries, so it is "cannot tell", never "not
    // gated". `Null` is a current helper reporting no gate. A number is a gate.
    let Some(reported) = data.get(veld_core::helper_gate::ALLOW_UID_FIELD) else {
        return Check {
            pass: false,
            label: format!(
                "Helper socket uid gate cannot be confirmed — {}{NOT_YOUR_INSTALL}",
                unreportable_gate_reason()
            ),
        };
    };
    let source = data
        .get(veld_core::helper_gate::ALLOW_UID_SOURCE_FIELD)
        .and_then(|v| v.as_str())
        .and_then(GateSource::from_wire);

    // `null` is the only shape that means "ungated". Anything else that is not a
    // number — a uid emitted as a string by some future helper, say — is a
    // response this build cannot read, and the field-absent case above already
    // established that "cannot read" must never become the definite claim that
    // the socket is open. `as_u64()` alone would collapse the two.
    if !reported.is_null() && reported.as_u64().is_none() {
        return Check {
            pass: false,
            label: format!(
                "Helper socket uid gate cannot be confirmed — the helper reported `{}` as its \
                 allowed uid, which this version of `veld` cannot read. Run `veld update` so \
                 the CLI and the helper match.{NOT_YOUR_INSTALL}",
                veld_core::helper_gate::ALLOW_UID_FIELD
            ),
        };
    }

    match reported.as_u64() {
        // Gated to root only. Reachable solely from a hand-written
        // `--allow-uid 0`, which the helper honours as the deliberate
        // instruction it is — but it means no `veld` command can drive the
        // helper, and only a root reader ever gets far enough to see this.
        Some(0) => Check {
            pass: false,
            label: format!(
                "Helper socket gated to uid 0 — that admits only root, so the `veld` CLI \
                 cannot drive its own helper. Run `veld setup privileged` to write the \
                 installing user's uid.{NOT_YOUR_INSTALL}"
            ),
        },
        // Gated, but not to the user reading this. Only reachable as root
        // (every other uid is refused before it can ask), which is exactly
        // when it is worth saying: `sudo veld doctor` is where somebody
        // investigating a locked-out CLI ends up.
        Some(uid) if gate_locks_out(uid, invoking) => Check {
            pass: false,
            label: format!(
                "Helper socket gated to uid {uid}, but you are uid {} — your `veld` commands \
                 cannot drive it. Run `veld setup privileged` to write the correct \
                 uid.{NOT_YOUR_INSTALL}",
                invoking.unwrap_or(0)
            ),
        },
        Some(uid) => Check {
            pass: true,
            label: format!(
                "Helper socket gated to uid {uid} ({})",
                gate_source_label(source)
            ),
        },
        None => Check {
            pass: false,
            label: format!(
                "Helper socket NOT gated to a uid — any local process can drive the root \
                 helper, including `shutdown`, which stops Caddy and drops every live \
                 URL. {}{NOT_YOUR_INSTALL}",
                ungated_reason(source)
            ),
        },
    }
}

/// Whether a helper gated to `reported` locks out the user running this CLI.
///
/// A free function with its own test because the `invoking == Some(0)` escape is
/// the part somebody will "simplify". A **plain root shell** answers 0 here and
/// is not a claim about which user the install belongs to — root is admitted by
/// every gate anyway (`peer_allowed`), so treating 0 as a uid to match against
/// would have bare `sudo veld doctor` report itself locked out of a perfectly
/// healthy helper. `SUDO_UID` is what lets a root invocation know who it stands
/// in for, and [`invoking_uid`] prefers it.
///
/// A `reported` of 0 is not this function's business — it fails the row on its
/// own, before this is reached.
fn gate_locks_out(reported: u64, invoking: Option<u64>) -> bool {
    invoking.is_some_and(|me| me != 0 && me != reported)
}

/// Why an *ungated* privileged helper has no uid, and what to do about it.
///
/// Every branch names a remedy that actually works, because a red row with no
/// exit is a row users learn to ignore.
///
/// `None` here means the helper reported a source string this build does not
/// know — a helper newer than the CLI — **not** a helper too old to report one.
/// Those two are different rows entirely; the second is
/// [`unreportable_gate_reason`]. Reaching this function at all means the helper
/// said, in so many words, that its socket is ungated.
///
/// Each string is a complete sentence, because it is appended to one.
fn ungated_reason(source: Option<GateSource>) -> &'static str {
    match source {
        Some(GateSource::RefusedRootLibDir) => {
            "The helper's install directory is root-owned (a system-paths install), so the \
             installing user cannot be derived — gating to root would admit only root and \
             lock your own CLI out. Run `veld setup privileged` to write the uid explicitly."
        }
        Some(GateSource::UnreadableLibDir) => {
            "The helper's install directory could not be read, so the installing user could \
             not be derived. Run `veld setup privileged` to write the uid explicitly."
        }
        // `Unprivileged` means the helper does not consider itself the system
        // daemon even though it answered on the system socket — a `--socket-path`
        // that does not match what setup writes. `Flag`/`LibDirOwner` cannot
        // reach this branch: both carry a uid. `None` is a source only a newer
        // helper knows; the remedy is the same one, and it is better than
        // guessing at a cause.
        Some(GateSource::Flag | GateSource::LibDirOwner | GateSource::Unprivileged) | None => {
            "Run `veld setup privileged` to write the uid explicitly."
        }
    }
}

/// What to say when the helper does not report the gate at all.
///
/// The field being **absent** means a helper predating #337. It may still be
/// perfectly gated by an `--allow-uid` its service definition carries — every
/// install that ran `veld setup privileged` on v16.58.x has one — so this must
/// never claim the socket is open, only that the answer is unavailable.
/// `veld update` settles it either way: the new helper gates itself and says so,
/// with no sudo and nothing to configure. That is #338's bar, and this row is
/// how it gets checked.
///
/// Lower-case and mid-sentence: it is appended to a clause, not started after
/// a full stop like [`ungated_reason`]'s strings.
fn unreportable_gate_reason() -> &'static str {
    "this helper predates the check and cannot report it. Its socket may or may not be gated \
     by an `--allow-uid` in its service definition. Run `veld update`; the new helper gates \
     itself and reports it."
}

fn tilde_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(suffix) = path.strip_prefix(&home) {
            return format!("~/{}", suffix.display());
        }
    }
    path.display().to_string()
}

/// Query a binary's version by running `<path> --version`.
fn query_binary_version(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let out = Command::new(path).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let version = stdout.split_whitespace().last()?.to_string();
    if version.contains('.') {
        Some(format!("v{version}"))
    } else {
        None
    }
}

/// Read the mode from `~/.veld/setup.json`.
fn read_mode(path: &Path) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return "not configured".to_string(),
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return "not configured".to_string(),
    };
    value
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("not configured")
        .to_string()
}

/// What is holding this machine awake, as one phrase.
///
/// Read from the daemon rather than by looking for a `caffeinate` process: the
/// hold is an inhibitor wrapped around a pipe *this daemon* owns, so the daemon
/// is the only thing that can say why it exists — and "why" is the whole point
/// of the line. A share holding it is the case worth naming, because that is the
/// one nobody pressed a button for.
async fn check_keep_awake() -> String {
    let Ok(state) = veld_core::share::DaemonClient::new().caffeinate().await else {
        // No daemon, or one too old to answer. Neither is worth a row — the
        // daemon's own status line above already says if it is not running.
        return String::new();
    };
    if !state.active {
        return String::new();
    }
    let reason_is_sharing = state.reason == "sharing";
    let why = match state.reason.as_str() {
        "sharing" => "because a share is live",
        "both" => "you asked, and a share is live",
        _ => "you asked",
    };
    let left = match state.remaining_secs {
        // Whose deadline the number is, when it is the share's own. This row is
        // the one a terminal-only user reads hours later to find out why a Mac
        // will not sleep, so a countdown that looks like it came from the
        // keep-awake cap — and does not — is worth one clause.
        //
        // `"sharing"` only, never `"both"`: `remaining_secs` is the later of the
        // two deadlines, so under a manual hold this number is not the share's.
        Some(secs) if reason_is_sharing && state.sharing_bound_by_share => {
            format!(", {} left — your shares' own expiry", humanize_secs(secs))
        }
        Some(secs) => format!(", {} left", humanize_secs(secs)),
        None => ", no time limit".to_owned(),
    };
    let lid = if state.covers_lid {
        ""
    } else {
        " (a shut lid still sleeps it)"
    };
    // Every other row in this report that states a problem also names the thing
    // that fixes it; this one used to be the exception, and it is the row a
    // terminal-only user reaches while trying to *stop* the hold. There is
    // deliberately no keep-awake subcommand to name (see `veld_core::agent`'s
    // reasoning about config-declared behaviour), so it names the surface.
    let how = if reason_is_sharing && state.sharing_bound_by_share {
        // The cap is not what is holding this machine, so sending the reader to
        // *Keep awake* alone would be advice for the wrong control — the number
        // above comes from the shares' lifetime.
        //
        // And deliberately NOT "shorten it in Settings → Sharing": a share's
        // `expires_at` is stamped when it is minted, so no setting shortens the
        // hold this row is printed beside. The only thing that ends it now is
        // ending the sharing; the setting is the default for the *next* share, and
        // `--ttl` outranks it anyway. Naming a control that cannot affect the
        // number next to it is the failure this whole change is about.
        " — stop sharing (veld unshare), or turn the hold off in Settings → Keep awake. Future shares: veld share --ttl, or Settings → Sharing"
    } else if reason_is_sharing {
        " — stop sharing, or turn it off in Settings → Keep awake"
    } else {
        " — turn it off from the cup in the top bar"
    };
    format!("held awake — {why}{left}{lid}{how}")
}

/// `"2d 3h"` / `"3h 12m"` / `"12m"` / `"under a minute"`. Mirrors `veld share`'s
/// own, plus a day branch: certificate lifetimes are measured in days, and
/// `"167h"` is a number a reader has to convert before it means anything.
fn humanize_secs(seconds: i64) -> String {
    if seconds < 60 {
        return "under a minute".to_owned();
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3600;
    if days > 0 {
        return match hours {
            0 => format!("{days}d"),
            h => format!("{days}d {h}h"),
        };
    }
    let minutes = (seconds % 3600) / 60;
    match (hours, minutes) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// The Services-block line for the certificate the HTTPS port is serving.
fn describe_cert(health: &veld_core::tls_health::TlsHealth) -> String {
    use veld_core::tls_health::TlsHealth;
    match health {
        // The overdue case first, and phrased so it neither starts with "valid"
        // nor claims health: `colorize_status` matches on the first word, so
        // without this the Services row printed a green "valid, expires in 20h"
        // directly above the red check built from the very same verdict.
        TlsHealth::Valid { expires_in, .. } if health.renewal_is_overdue() => format!(
            "RENEWAL OVERDUE — still valid for {}, but Caddy should have replaced it",
            humanize_secs(expires_in.as_secs() as i64)
        ),
        TlsHealth::Valid { expires_in, .. } => format!(
            "valid, expires in {}",
            humanize_secs(expires_in.as_secs() as i64)
        ),
        TlsHealth::Expired { expired_for } => format!(
            "EXPIRED {} ago",
            humanize_secs(expired_for.as_secs() as i64)
        ),
        TlsHealth::NotYetValid { valid_in } => format!(
            "NOT VALID for another {} — this machine's clock is behind",
            humanize_secs(valid_in.as_secs() as i64)
        ),
        TlsHealth::Unreachable { detail } => format!("no TLS answer ({detail})"),
        TlsHealth::Unreadable { detail } => format!("unreadable ({detail})"),
    }
}

/// The Checks-block line: the same verdict, plus what to do about it.
///
/// The repair named is a Caddy *restart*, not a reload, and that is not a
/// simplification — Caddy will not re-examine a certificate it already holds in
/// its cache, so reloading its config (which veld does on every route change)
/// cannot renew an expired one. Only a new process can.
/// `host` is named in every line, because this row measures **one** hostname and
/// each run URL carries a certificate of its own. A green row that read as a
/// verdict on every veld URL would be the same over-claiming this change exists to
/// remove. The helper's watchdog does check them all, once a minute.
fn cert_check_label(host: &str, health: &veld_core::tls_health::TlsHealth) -> String {
    use veld_core::tls_health::TlsHealth;
    match health {
        TlsHealth::Valid { expires_in, .. } if !health.renewal_is_overdue() => {
            format!(
                "HTTPS certificate for {host} valid (expires in {})",
                humanize_secs(expires_in.as_secs() as i64)
            )
        }
        // "tries", not "will": the helper restarts Caddy at most a few times before
        // it stops, and it stops precisely when restarting is not working — a state
        // this process cannot see, because the counter lives in the helper's
        // memory. Promising an imminent restart there would be the same kind of
        // confident wrong answer this whole check exists to replace.
        TlsHealth::Valid { expires_in, .. } => format!(
            "HTTPS certificate for {host} expires in {} and Caddy has not renewed it — \
             veld restarts Caddy to try to renew it, a few times, a couple of minutes apart. \
             If it persists, the reason is in the Caddy log",
            humanize_secs(expires_in.as_secs() as i64)
        ),
        TlsHealth::Expired { expired_for } => format!(
            "HTTPS certificate for {host} EXPIRED {} ago — browsers refuse veld URLs. \
             veld restarts Caddy to try to renew it, a few times, a couple of minutes apart. \
             If it persists, the reason is in the Caddy log",
            humanize_secs(expired_for.as_secs() as i64)
        ),
        TlsHealth::NotYetValid { valid_in } => format!(
            "HTTPS certificate for {host} is not valid for another {} — browsers refuse veld URLs \
             until this machine's clock is right, and restarting Caddy cannot fix a clock",
            humanize_secs(valid_in.as_secs() as i64)
        ),
        // Neutral about *why* nothing answered: `Unreachable` covers a port with
        // nothing listening as well as a Caddy that took the connection and never
        // replied, and `describe_cert` above says so. Asserting one of the two
        // here would make two lines of the same report disagree.
        TlsHealth::Unreachable { detail } => format!(
            "No TLS answer for {host} on the HTTPS port ({detail}) — Caddy may be failing to \
             issue a certificate at all; the reason is in the Caddy log"
        ),
        TlsHealth::Unreadable { detail } => {
            format!("HTTPS certificate for {host} could not be read ({detail}) — see the Caddy log")
        }
    }
}

/// Whether there is a recent, usable copy of the database — and how to use it.
///
/// **Passes on "there is a usable backup", not on "backups are switched on"**, and
/// the difference is the whole row. The real incident this feature answers had two
/// failures, not one: the file died, *and* the copies that survived it opened
/// perfectly while containing nothing anybody wanted. A row that reported the
/// setting would have said everything was fine on that machine, both times.
///
/// So the newest artifact is actually opened and checked here, its age is stated
/// rather than implied, and the daemon's last recorded attempt is reported when it
/// failed — a backup subsystem failing quietly is indistinguishable from one that
/// is working until the day it matters.
fn check_backups() -> Check {
    use veld_core::db::backup;

    let db = veld_core::db::Db::default_path()
        .ok()
        .filter(|p| p.exists())
        .and_then(|_| veld_core::db::Db::open().ok());

    // Fall back to the derived default when the settings cannot be read, exactly
    // as `veld backup` does — a database that will not open is the case this row
    // exists for, so it must not depend on one.
    let dir = db
        .as_ref()
        .and_then(|db| db.backup_prefs().dir)
        .or_else(backup::default_dir);
    let enabled = db.as_ref().map(|db| db.backup_prefs().enabled);

    let Some(dir) = dir else {
        return Check {
            pass: false,
            label: "No backup directory could be determined for the database".into(),
        };
    };

    // Reported whether or not it is the reason for a failure: a failure recorded
    // by the daemon explains a stale newest-backup that would otherwise look like
    // nothing was ever configured.
    let last_error = db
        .as_ref()
        .and_then(|db| db.kv_get(backup::LAST_RUN_KEY).ok().flatten())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter(|v| v["ok"] == serde_json::Value::Bool(false))
        .and_then(|v| v["error"].as_str().map(str::to_owned));

    let now = chrono::Utc::now();
    let artifacts = backup::list(&dir);
    // **`newest_usable` decides what "usable" means, and this row asks it** rather
    // than re-deriving half its rule. Re-deriving it is what went wrong: this
    // filter kept `taken_at <= now` after `newest_usable` was fixed to
    // de-prioritise a future-dated artifact rather than disqualify it, so on a
    // machine whose clock is behind its own backups doctor reported that nothing
    // could be restored while `veld backup restore` restored one perfectly well.
    //
    // **The deep check, for the one artifact this row reports on.** `inspect` is the
    // sweep's cheap `quick_check`, which does not compare index content against
    // table content — so an artifact `veld backup restore` would refuse could be the
    // one doctor called healthy. That is the incident's second failure in a new
    // costume, and it is worth one full scan of one file to close. The *count* stays
    // shallow: it is a survey, and deep-checking a folder of hundreds is what
    // `inspect`/`inspect_deep` were split apart to avoid.
    let newest = backup::newest_restorable(&dir, now);
    let usable = artifacts
        .iter()
        .filter(|a| backup::inspect(&a.path).is_ok())
        .count();

    // How stale is too stale. Generous on purpose — a laptop that slept through
    // several intervals is healthy, and a check that cries wolf is a check people
    // learn to skip — but bounded, because the whole point of this row is the state
    // where backups silently stopped.
    let interval = db
        .as_ref()
        .map(|db| db.backup_prefs().interval_minutes)
        .unwrap_or(veld_core::db::DEFAULT_BACKUP_INTERVAL_MINUTES);
    let stale_after =
        chrono::Duration::minutes(interval.saturating_mul(4)).max(chrono::Duration::hours(12));

    let dir_note = tilde_path(&dir);
    match newest.as_ref() {
        Some(a) => {
            let age = now - a.taken_at;
            // **A backup that exists is not a backup that is current**, and passing
            // on existence alone is the failure this whole feature was filed
            // against: the copies that survived the real incident opened fine and
            // were weeks old. Nothing else notices a scheduler that stopped — a
            // daemon that is not running records no failure — so if this row does
            // not say it, nobody does.
            // **A backup dated in the future makes "how old is it" unanswerable, so
            // it must not be answered.** `age` goes negative, `describe_age` clamps
            // it to "0 minute(s) old", and the staleness test can then never fire —
            // this row turns permanently green on a machine whose clock disagrees
            // with its own backups, which is exactly the state it exists to report.
            // Reported as its own condition rather than folded into staleness,
            // because the fix is to look at the clock, not at the backups.
            let ahead = a.taken_at > now;
            let stale = enabled != Some(false) && !ahead && age > stale_after;
            // **The word and the verdict are computed from the same condition.**
            // They were not: a fresh backup that is world-readable failed the check
            // while the label still opened with "OK", and both fields go out over
            // `--json` verbatim — so an agent matching on the text saw a pass on
            // precisely the exposed-secrets case this row exists to raise.
            let pass = !stale && !ahead && a.owner_only;
            let state = match (stale, ahead, a.owner_only) {
                (true, _, _) => "STALE",
                (_, true, _) => "CLOCK",
                (_, _, false) => "EXPOSED",
                _ => "OK",
            };
            let mut label = format!(
                "Database backup {state} ({} of {} usable in {dir_note}, newest {}, schema v{})",
                usable,
                artifacts.len(),
                output::describe_age(age),
                a.schema_version,
            );
            if ahead {
                label.push_str(
                    " — dated in the future, so this machine's clock disagrees with its \
                     own backups and their age cannot be judged. The backups are still \
                     restorable; check the clock",
                );
            }
            if stale {
                label.push_str(
                    " — nothing has written one since; check the daemon is running \
                     (`veld doctor` Services above) or run `veld backup now`",
                );
            }
            if !a.owner_only {
                // The mode veld asks for is not always the mode it gets: a FAT or
                // SMB volume cannot express one, and an artifact carries the same
                // secrets the database is 0600 for.
                label.push_str(" — readable by more than you; a backup carries relay tokens");
            }
            if let Some(error) = last_error {
                label.push_str(&format!(" — last attempt failed: {error}"));
            }
            Check { pass, label }
        }
        None if enabled == Some(false) => Check {
            // Not a failure: somebody turned it off on purpose, and a red row for a
            // deliberate choice is how a check earns being ignored.
            pass: true,
            label: format!("Database backups are switched off (backup.enabled) — {dir_note}"),
        },
        None => {
            let why = last_error
                .map(|e| format!(" — last attempt failed: {e}"))
                .unwrap_or_else(|| {
                    " — the daemon writes one on the backup.intervalMinutes schedule; \
                     `veld backup now` takes one immediately"
                        .to_string()
                });
            // **"None" no longer implies "nothing is there."** `newest` is the deep
            // check and `usable` is the shallow one, so this arm is reachable with
            // artifacts present that `veld backup` lists as `ok`: every one passes
            // `quick_check` and none survives `integrity_check`. Saying "no usable
            // backup" flatly would have doctor and the listing contradicting each
            // other — the failure this row keeps being rewritten to avoid. Say which
            // check they failed instead.
            let label = if usable > 0 {
                format!(
                    "No restorable database backup in {dir_note} — {usable} of \
                     {} pass a quick check but none survives a full integrity check, \
                     so none can be restored{why}",
                    artifacts.len()
                )
            } else {
                format!("No usable database backup in {dir_note}{why}")
            };
            Check { pass: false, label }
        }
    }
}

/// Check daemon status via launchctl (macOS) or socket existence.
async fn check_daemon_status() -> String {
    // Try launchctl on macOS
    if cfg!(target_os = "macos") {
        if let Ok(out) = Command::new("launchctl").arg("list").output() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if line.contains("veld.daemon") || line.contains("veld-daemon") {
                    // Format: PID\tStatus\tLabel
                    let parts: Vec<&str> = line.split('\t').collect();
                    if let Some(pid_str) = parts.first() {
                        if let Ok(pid) = pid_str.trim().parse::<u32>() {
                            return format!("running (pid {pid})");
                        }
                    }
                    return "loaded (not running)".to_string();
                }
            }
        }
    }

    // Try daemon socket
    let daemon_sock = Some(veld_core::instance::daemon_socket());
    if let Some(ref sock) = daemon_sock {
        if sock.exists() {
            if tokio::net::UnixStream::connect(sock).await.is_ok() {
                return "running".to_string();
            }
            return "socket exists (not responding)".to_string();
        }
    }

    "not running".to_string()
}

/// Check Caddy status via its admin API.
async fn check_caddy_status() -> String {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return "unknown (HTTP client error)".to_string(),
    };

    // Check sentinel
    let sentinel_ok = client
        .get("http://localhost:2019/id/veld-sentinel")
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());

    if sentinel_ok {
        "running (admin API on 2019, sentinel OK)".to_string()
    } else {
        // Maybe admin API is up but no sentinel
        match client.get("http://localhost:2019/config/").send().await {
            Ok(r) if r.status().is_success() => {
                "running (admin API on 2019, sentinel missing)".to_string()
            }
            _ => "not running".to_string(),
        }
    }
}

/// Check CA trust status.
///
/// In privileged mode, Caddy runs as root and its `caddy-data/pki/` directory
/// is root-owned with mode 700. This means `path.exists()` and `metadata()`
/// both return false/Err when run as the normal user. To handle this, we check
/// the macOS keychain directly (which doesn't need file access) before falling
/// back to the cert file on disk.
fn check_ca_status() -> String {
    let ca_cert = veld_core::paths::caddy_data_dir()
        .join("pki")
        .join("authorities")
        .join("local")
        .join("root.crt");

    if cfg!(target_os = "macos") {
        // Try verify-cert if the file is readable.
        if ca_cert.exists() {
            if let Ok(out) = Command::new("security")
                .args(["verify-cert", "-c"])
                .arg(&ca_cert)
                .output()
            {
                if out.status.success() {
                    return "trusted (login keychain)".to_string();
                }
            }
        }

        // Check the keychain directly by certificate name. This works even when
        // the cert file on disk is unreadable (root-owned in privileged mode).
        // The CA may be named "Veld Local CA" (custom) or "Caddy Local Authority"
        // (Caddy default).
        for name in ["Veld Local CA", "Caddy Local Authority"] {
            if let Ok(out) = Command::new("security")
                .args(["find-certificate", "-c", name, "-a"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if out.status.success() && !stdout.is_empty() {
                    // Found in keychain — verify trust by extracting and checking.
                    if is_ca_trusted_in_keychain(name) {
                        return "trusted (login keychain)".to_string();
                    }
                    return "installed (may not be trusted)".to_string();
                }
            }
        }

        if ca_cert.exists() {
            return "not trusted (cert exists but not in keychain)".to_string();
        }

        return "not found".to_string();
    }

    if !ca_cert.exists() {
        return "not found".to_string();
    }

    // Fallback for non-macOS: cert file exists
    "present (trust status unknown)".to_string()
}

/// Check whether a CA certificate is actually trusted (not just present) in
/// the macOS keychain by running `security verify-cert` against a temp copy
/// extracted from the keychain.
fn is_ca_trusted_in_keychain(name: &str) -> bool {
    // Export the cert from the keychain to a temp file, then verify it.
    let tmp = std::env::temp_dir().join("veld-doctor-ca-check.pem");
    let export_ok = Command::new("security")
        .args(["find-certificate", "-c", name, "-p"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() && !out.stdout.is_empty() {
                std::fs::write(&tmp, &out.stdout).ok()
            } else {
                None
            }
        });

    if export_ok.is_none() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }

    let trusted = Command::new("security")
        .args(["verify-cert", "-c"])
        .arg(&tmp)
        .output()
        .is_ok_and(|out| out.status.success());

    let _ = std::fs::remove_file(&tmp);
    trusted
}

/// Try an HTTP GET and return true if status is success.
async fn http_get_ok(url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Try a TCP connection to host:port.
async fn tcp_connect_ok(host: &str, port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

/// Check that `test.localhost` resolves to 127.0.0.1.
fn resolve_localhost_dns() -> bool {
    match ("test.localhost", 80u16).to_socket_addrs() {
        Ok(addrs) => addrs
            .into_iter()
            .any(|a| a.ip() == std::net::Ipv4Addr::LOCALHOST),
        Err(_) => false,
    }
}

/// Colorize service status strings.
fn colorize_status(status: &str) -> String {
    if status.starts_with("running") || status.starts_with("trusted") || status.starts_with("valid")
    {
        output::green(status)
    } else if status.starts_with("not running")
        || status.starts_with("not found")
        || status.starts_with("not trusted")
        || status.starts_with("EXPIRED")
        // A certificate the clock rejects is refused by browsers exactly as an
        // expired one is. Yellow would invite a shrug at a total outage.
        || status.starts_with("NOT VALID")
        // Still servable, already proof that renewal stopped: the row has to
        // agree with the check below it, which is red.
        || status.starts_with("RENEWAL OVERDUE")
    {
        output::red(status)
    } else {
        output::yellow(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use veld_core::tls_health::TlsHealth;

    /// A week, as veld now asks Caddy to issue.
    const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);

    /// Certificate lifetimes are days, and `"167h"` is a number the reader has
    /// to convert before it says anything. Sub-day values keep the shape the
    /// keep-awake and share rows already print.
    #[test]
    fn durations_read_in_the_largest_useful_unit() {
        assert_eq!(humanize_secs(30), "under a minute");
        assert_eq!(humanize_secs(12 * 60), "12m");
        assert_eq!(humanize_secs(3 * 3600), "3h");
        assert_eq!(humanize_secs(3 * 3600 + 12 * 60), "3h 12m");
        assert_eq!(humanize_secs(2 * 86_400), "2d");
        assert_eq!(humanize_secs(6 * 86_400 + 23 * 3600), "6d 23h");
        // The boundary itself: a hair under a day is still hours.
        assert_eq!(humanize_secs(86_399), "23h 59m");
    }

    /// A certificate deep in its renewal window is still served, so a reader
    /// looking only at "does it work" would call it fine. The row has to say the
    /// other thing — that renewal stopped — because that is what breaks next.
    #[test]
    fn the_certificate_row_fails_before_the_certificate_does() {
        // A week-long leaf with a day left: Caddy should have replaced it two
        // days ago.
        let overdue = TlsHealth::Valid {
            expires_in: Duration::from_secs(20 * 3600),
            lifetime: WEEK,
        };
        assert!(overdue.serves_browsers());
        assert!(overdue.renewal_is_overdue());
        let label = cert_check_label("veld.localhost", &overdue);
        assert!(label.contains("has not renewed"));
        // Never an unconditional promise: the helper gives up after a few
        // fruitless restarts, and this process cannot tell whether it already has.
        assert!(label.contains("try to renew"));
        assert!(!label.contains("within about two minutes to"));

        let fine = TlsHealth::Valid {
            expires_in: Duration::from_secs(6 * 86_400),
            lifetime: WEEK,
        };
        assert_eq!(
            cert_check_label("veld.localhost", &fine),
            "HTTPS certificate for veld.localhost valid (expires in 6d)"
        );
        assert_eq!(describe_cert(&fine), "valid, expires in 6d");
    }

    /// The Services row and the Checks row are built from one verdict, so they
    /// must not disagree. A certificate deep in its renewal window is still
    /// servable — which is exactly why the row used to print a green "valid,
    /// expires in 20h" above a red "has not renewed" check.
    #[test]
    fn an_overdue_certificate_does_not_read_as_healthy_in_the_services_row() {
        let overdue = TlsHealth::Valid {
            expires_in: Duration::from_secs(20 * 3600),
            lifetime: WEEK,
        };
        assert!(overdue.renewal_is_overdue());
        let line = describe_cert(&overdue);
        assert!(
            !line.starts_with("valid"),
            "must not read as health: {line}"
        );
        assert!(
            colorize_status(&line).contains(&output::red(&line)),
            "the row must be as red as the check it sits above: {line}"
        );
    }

    /// The row names the hostname it measured. Each run URL has a certificate of
    /// its own, so a row that read as a verdict on all of them would over-claim
    /// exactly the way the all-green `veld doctor` in the original report did.
    #[test]
    fn the_certificate_row_names_the_host_it_checked() {
        let fine = TlsHealth::Valid {
            expires_in: Duration::from_secs(6 * 86_400),
            lifetime: WEEK,
        };
        assert!(cert_check_label("veld.localhost", &fine).contains("for veld.localhost"));
        let expired = TlsHealth::Expired {
            expired_for: Duration::from_secs(60),
        };
        assert!(cert_check_label("veld.localhost", &expired).contains("for veld.localhost"));
    }

    /// The Services line is what `colorize_status` colours, so its first word
    /// decides whether an expired certificate is printed in red.
    #[test]
    fn an_expired_certificate_reads_as_red() {
        let expired = TlsHealth::Expired {
            expired_for: Duration::from_secs(29 * 3600),
        };
        let line = describe_cert(&expired);
        assert_eq!(line, "EXPIRED 1d 5h ago");
        assert!(colorize_status(&line).contains(&output::red(&line)));
        assert!(cert_check_label("veld.localhost", &expired).contains("browsers refuse veld URLs"));
    }

    /// A clock behind the certificate's `notBefore` breaks browsers exactly as an
    /// expired one does, and the remedy is different: restarting Caddy reissues a
    /// certificate the clock rejects just the same. Both lines have to say so —
    /// and the Services row has to be **red**, because `colorize_status` matches on
    /// the first word and yellow would invite a shrug at a total outage.
    #[test]
    fn a_future_dated_certificate_blames_the_clock_and_still_reads_as_red() {
        let early = TlsHealth::NotYetValid {
            valid_in: Duration::from_secs(3 * 3600),
        };
        assert!(!early.serves_browsers());
        assert!(!early.renewal_is_overdue());
        let line = describe_cert(&early);
        assert!(line.contains("clock"));
        assert!(
            colorize_status(&line).contains(&output::red(&line)),
            "a certificate browsers refuse must not render as a warning: {line}"
        );
        let label = cert_check_label("veld.localhost", &early);
        assert!(label.contains("clock"));
        assert!(!label.contains("veld restarts Caddy"));
    }

    /// `veld doctor` must not knock on a holder that has a daemon attached.
    ///
    /// This command was the reported trigger for terminals dying: connecting to a
    /// holder is how a daemon arrives, and a holder that still runs the pre-fix
    /// binary — which every terminal open at update time does, until its shell
    /// exits — hands the session to anything that connects. So the row counts
    /// while the daemon is up and probes only when it is down, and the assertion
    /// is on the *listener*: whether anything reached it, not what the label says.
    #[test]
    fn the_holder_row_only_knocks_when_the_daemon_is_down() {
        let dir = tempfile::tempdir().unwrap();
        // A digest-shaped name, which is what `holder_sockets_in` looks for.
        let socket = dir.path().join("00000000deadbeef.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        // Something that is not a holder, beside it: it must be counted by
        // neither branch.
        std::fs::write(dir.path().join("daemon.sock"), b"").unwrap();

        let row = Diagnostics::holders_row(dir.path(), true);
        assert!(row.pass);
        assert!(
            row.label.contains("1 terminal holder(s)") && row.label.contains("not probed"),
            "a counted row must say it did not probe: {}",
            row.label
        );
        assert!(
            listener.accept().is_err(),
            "nothing may connect to a holder while its daemon is attached"
        );

        let row = Diagnostics::holders_row(dir.path(), false);
        assert!(row.pass);
        assert_eq!(
            row.label,
            format!("1 terminal holder(s) running ({})", tilde_path(dir.path())),
            "with no daemon there is nothing to displace, and the answer is worth having"
        );
        assert!(
            listener.accept().is_ok(),
            "the probe must actually reach the socket"
        );

        // And an empty directory is the ordinary state, not a failure.
        let empty = tempfile::tempdir().unwrap();
        assert!(Diagnostics::holders_row(empty.path(), true).pass);
        assert!(
            Diagnostics::holders_row(empty.path(), true)
                .label
                .starts_with("No terminal holders")
        );
    }

    #[test]
    fn the_log_path_is_read_out_of_a_plist() {
        let plist = "<dict>\n    <key>StandardOutPath</key>\n    <string>/a/out.log</string>\n \
                     <key>StandardErrorPath</key>\n    <string>/a/err.log</string>\n</dict>";
        assert_eq!(
            launchd_string_after_key(plist, "StandardErrorPath").as_deref(),
            Some("/a/err.log"),
            "the value must come from the named key, not the first <string> in the file"
        );
    }

    #[test]
    fn the_program_is_read_out_of_its_array() {
        // The shape `install_daemon` writes: the program is the first <string> of
        // ProgramArguments, and it must win over every later key in the file.
        let plist = "<dict>\n    <key>Label</key>\n    <string>dev.veld.daemon</string>\n    \
                     <key>ProgramArguments</key>\n    <array>\n        \
                     <string>/p/lib/veld/veld-daemon</string>\n    </array>\n    \
                     <key>StandardErrorPath</key>\n    <string>/h/.veld/veld-daemon.log</string>\n</dict>";
        assert_eq!(
            launchd_string_after_key(plist, "ProgramArguments").as_deref(),
            Some("/p/lib/veld/veld-daemon")
        );
    }

    #[test]
    fn a_plist_without_the_key_captures_nothing() {
        // The shape every install written before this change has: a complete,
        // valid plist with no output paths at all.
        let plist = "<dict>\n    <key>Label</key>\n    <string>dev.veld.daemon</string>\n</dict>";
        assert_eq!(launchd_string_after_key(plist, "StandardErrorPath"), None);
        // And the shapes that would panic a naive slice or answer with a lie.
        assert_eq!(launchd_string_after_key("", "StandardErrorPath"), None);
        assert_eq!(
            launchd_string_after_key("<key>StandardErrorPath</key>", "StandardErrorPath"),
            None
        );
        assert_eq!(
            launchd_string_after_key(
                "<key>StandardErrorPath</key><string></string>",
                "StandardErrorPath"
            ),
            None,
            "an empty value is not a log file"
        );
        assert_eq!(
            launchd_string_after_key(
                "<key>StandardErrorPath</key><string>/a.log",
                "StandardErrorPath"
            ),
            None,
            "an unterminated element is unreadable, not a path"
        );
    }

    /// Every outcome of the gate row, decided without a socket.
    ///
    /// The row is the only place an exposed root socket is reported anywhere, and
    /// its failure mode is a confident wrong sentence rather than a crash — so
    /// running `veld doctor` on a healthy machine proves nothing about the seven
    /// other branches. Each assertion below pins the one thing that must not
    /// drift: whether the row passes, and whether it makes a claim it cannot
    /// support.
    #[test]
    fn the_gate_row_states_only_what_the_helper_actually_reported() {
        use super::{GateReport, gate_check};

        let status = |v: serde_json::Value| GateReport::Status(v);
        let row = gate_check;

        // Gated to the reader: the only green outcome there is.
        let c = row(
            &status(serde_json::json!({ "allow_uid": 501, "allow_uid_source": "lib-dir-owner" })),
            Some(501),
        );
        assert!(c.pass);
        assert!(c.label.contains("gated to uid 501"));
        assert!(
            c.label
                .contains("derived from the install directory's owner")
        );

        // Explicitly ungated: the state #337 exists to remove. A definite claim
        // is correct HERE and nowhere else.
        let c = row(
            &status(serde_json::json!({
                "allow_uid": serde_json::Value::Null,
                "allow_uid_source": "refused-root-lib-dir",
            })),
            Some(501),
        );
        assert!(!c.pass);
        assert!(c.label.contains("NOT gated"));
        assert!(c.label.contains("root-owned"));

        // Field absent — a helper predating the report. It may well be gated by
        // its plist, so this must NOT read as an open socket.
        let c = row(
            &status(serde_json::json!({ "version": "16.58.1" })),
            Some(501),
        );
        assert!(!c.pass);
        assert!(!c.label.contains("NOT gated"));
        assert!(c.label.contains("cannot be confirmed"));
        assert!(c.label.contains("`veld update`"));

        // A shape this build cannot read is also "cannot confirm", never "open".
        let c = row(
            &status(serde_json::json!({ "allow_uid": "501", "allow_uid_source": "flag" })),
            Some(501),
        );
        assert!(!c.pass);
        assert!(!c.label.contains("NOT gated"));
        assert!(c.label.contains("cannot be confirmed"));

        // Gated to root only: honoured, but the CLI cannot drive it.
        let c = row(
            &status(serde_json::json!({ "allow_uid": 0, "allow_uid_source": "flag" })),
            Some(0),
        );
        assert!(
            !c.pass,
            "uid 0 admits only root and must never read as green"
        );
        assert!(c.label.contains("uid 0"));

        // Gated to somebody else. Only a root reader gets this far.
        let c = row(
            &status(serde_json::json!({ "allow_uid": 999, "allow_uid_source": "flag" })),
            Some(501),
        );
        assert!(!c.pass);
        assert!(c.label.contains("gated to uid 999"));
        assert!(c.label.contains("you are uid 501"));

        // Refused outright — the one failure mode deriving the uid introduces.
        // It must not send a shared machine's non-owner to rewrite the service.
        let c = row(&GateReport::Refused, Some(501));
        assert!(!c.pass);
        assert!(c.label.contains("refused uid 501"));
        assert!(
            c.label.contains("do NOT run `veld setup privileged`"),
            "a reader who is not the install's owner must be told to leave it alone"
        );

        // An unresolvable invoking uid still produces a usable sentence.
        let c = row(&GateReport::Refused, None);
        assert!(c.label.contains("refused this user"));

        // A refusal whose (best-effort) message lost the race arrives as a
        // broken exchange. It must not assert a refusal it cannot prove, and it
        // must not stay silent either — silence is how the one failure mode this
        // gate introduces would go unreported.
        let c = row(&GateReport::Unreadable, Some(501));
        assert!(!c.pass);
        assert!(c.label.contains("cannot be confirmed"));
        assert!(!c.label.contains("NOT gated"));

        // A source only a newer helper knows: gated is still gated, and the
        // provenance simply goes unnamed rather than being invented.
        let c = row(
            &status(serde_json::json!({ "allow_uid": 501, "allow_uid_source": "invented-later" })),
            Some(501),
        );
        assert!(c.pass);
        assert!(c.label.contains("source unknown"));
    }

    /// No failing gate row may hand a reader a remedy without saying it might
    /// not be theirs to run.
    ///
    /// The row runs for whoever types `veld doctor`, and on a shared machine
    /// that includes somebody whose privileged veld belongs to another account.
    /// For them `veld setup privileged` is a hijack — it repoints a root service
    /// at their own binary and gates the socket to them — and `veld update`
    /// cannot touch the other account's service at all. An earlier fix put the
    /// caveat on the refused branch only, and the ungated branch (which every
    /// local uid reaches, because an ungated helper refuses nobody) went on
    /// advising the hijack. This asserts the property across every branch rather
    /// than one at a time, so a new branch inherits the requirement.
    #[test]
    fn no_failing_gate_row_tells_a_stranger_to_take_over_the_service() {
        use super::{GateReport, gate_check};

        let reports = [
            GateReport::Refused,
            GateReport::Unreadable,
            GateReport::Status(serde_json::json!({ "version": "16.58.1" })),
            GateReport::Status(serde_json::json!({ "allow_uid": "501" })),
            GateReport::Status(serde_json::json!({ "allow_uid": 0 })),
            GateReport::Status(serde_json::json!({ "allow_uid": 999 })),
            GateReport::Status(serde_json::json!({
                "allow_uid": serde_json::Value::Null,
                "allow_uid_source": "refused-root-lib-dir",
            })),
        ];

        for report in &reports {
            for me in [Some(501u64), Some(0), None] {
                let check = gate_check(report, me);
                if check.pass {
                    continue;
                }
                assert!(
                    check.label.contains("belongs to another account"),
                    "a failing row offered a remedy with no owner caveat: {}",
                    check.label
                );
            }
        }
    }

    /// The uid-match rule, isolated from the socket call so the `root` escape
    /// cannot be "simplified" away without a red test.
    ///
    /// Dropping `me != 0` is the natural edit — the guard looks redundant — and
    /// it silently makes bare `sudo veld doctor` (uid 0, no `SUDO_UID`) accuse a
    /// perfectly healthy helper of locking the user out.
    #[test]
    fn a_gate_only_locks_out_a_user_it_could_actually_refuse() {
        use super::gate_locks_out;

        // The ordinary healthy case.
        assert!(!gate_locks_out(501, Some(501)));

        // A real mismatch. Only ever visible to root, because every other uid is
        // refused before it can ask — which is exactly why the row matters.
        assert!(gate_locks_out(999, Some(501)));

        // A plain root shell has no opinion about which user the install belongs
        // to, and root is admitted by every gate. Not a lockout.
        assert!(!gate_locks_out(501, Some(0)));

        // Uid unresolvable — say nothing rather than accuse.
        assert!(!gate_locks_out(501, None));
    }

    #[test]
    fn every_ungated_state_names_a_remedy() {
        use super::{GateSource, gate_source_label, ungated_reason, unreportable_gate_reason};

        // The case that matters: a helper too old to report the gate. `veld
        // update` alone fixes it — no sudo, nothing to configure. That is #338's
        // bar. It must NOT claim the socket is open: an install that ran
        // `veld setup privileged` on v16.58.x carries `--allow-uid` in its plist
        // and is genuinely gated, it just cannot say so.
        let unknown = unreportable_gate_reason();
        assert!(unknown.contains("`veld update`"));
        assert!(!unknown.contains("sudo"));
        assert!(
            unknown.contains("may or may not be"),
            "the unknown state must not be reported as a known-open socket: {unknown}"
        );

        // A source only a NEWER helper knows is not the same thing as a helper
        // too old to report one, and must not borrow its text: this helper said
        // outright that its socket is ungated.
        let newer = ungated_reason(None);
        assert!(newer.contains("`veld setup privileged`"));
        assert!(
            !newer.contains("predates"),
            "an unknown source from a newer helper must not read as an older one: {newer}"
        );

        for source in [
            GateSource::RefusedRootLibDir,
            GateSource::UnreadableLibDir,
            GateSource::Unprivileged,
        ] {
            let reason = ungated_reason(Some(source));
            assert!(
                reason.contains("`veld setup privileged`"),
                "{source:?} left the user with no remedy: {reason}"
            );
        }

        // A gated helper says where the uid came from; anything else — including
        // a source only a newer helper knows — says nothing rather than guessing.
        assert_eq!(
            gate_source_label(Some(GateSource::Flag)),
            "from the service definition"
        );
        assert_eq!(
            gate_source_label(Some(GateSource::LibDirOwner)),
            "derived from the install directory's owner"
        );
        assert_eq!(gate_source_label(None), "source unknown");
        assert_eq!(
            gate_source_label(Some(GateSource::RefusedRootLibDir)),
            "source unknown"
        );
    }
}
