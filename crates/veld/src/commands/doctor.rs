use crate::output;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

    // Services
    helper_status: String,
    daemon_status: String,
    caddy_status: String,
    ca_status: String,

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

        // Veld Desktop. Installed by default on macOS now, updated by
        // `veld update`, and able to be stale in a way nothing else here would
        // show — the app half can lag while every binary above is current.
        // Doctor is where a user is sent when something is wrong, so a stale
        // app belongs in the list rather than only in `veld desktop status`.
        self.desktop_app = match veld_core::setup::desktop_app_status() {
            Some((path, version)) => {
                let version = version.unwrap_or_else(|| "unknown version".to_string());
                let suffix = if version == cli_version {
                    String::new()
                } else {
                    format!(" — CLI is {cli_version}, run 'veld desktop update'")
                };
                format!("{} ({version}){suffix}", tilde_path(&path))
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
                    self.checks.push(Check {
                        pass: true,
                        label: format!("Database OK ({path}, schema v{version}, {size})"),
                    });
                }
                Err(e) => {
                    self.checks.push(Check {
                        pass: false,
                        label: format!("Database not usable at {path}: {e}"),
                    });
                }
            }
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

        // 5. Feedback server responding. Name the port so a contributor
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

        // 6. .localhost DNS resolves
        let dns_ok = resolve_localhost_dns();
        self.checks.push(Check {
            pass: dns_ok,
            label: if dns_ok {
                ".localhost DNS resolves".into()
            } else {
                ".localhost DNS does not resolve".into()
            },
        });

        // 7. No stale system install
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

        // 8. No stale binaries next to CLI (e.g. ~/.local/bin/veld-daemon
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

        // 9. Terminal holder processes.
        //
        // Each open terminal in `/ide` has a process of its own holding its PTY,
        // which is what lets a shell survive `veld update`. They are invisible to
        // every other check here — not the daemon, not a run — so "my terminal
        // died after an update" had nowhere to look. This names the directory and
        // counts what is in it; a socket nobody answers is a holder that is gone
        // and gets swept at the next daemon start.
        self.checks.push(self.terminal_holders_check(feedback_ok));

        // 10. The terminal-URL shims.
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
        println!(
            "    {:<14}{}",
            "Caddy:",
            colorize_status(&self.caddy_status)
        );
        println!("    {:<14}{}", "CA:", colorize_status(&self.ca_status));
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
            },
            "services": {
                "helper": self.helper_status,
                "daemon": self.daemon_status,
                "caddy": self.caddy_status,
                "ca": self.ca_status,
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
    if status.starts_with("running") || status.starts_with("trusted") {
        output::green(status)
    } else if status.starts_with("not running")
        || status.starts_with("not found")
        || status.starts_with("not trusted")
    {
        output::red(status)
    } else {
        output::yellow(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
