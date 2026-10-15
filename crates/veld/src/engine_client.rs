//! The CLI's one connection to the engine, which lives in the daemon.
//!
//! The CLI is a face: it asks the engine to do things and draws what comes back.
//! Its only job of its own is **bootstrapping** — if no daemon answers, start
//! one — and that is here too ([`EngineClient::connect`]).

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use veld_engine::api::{
    EngineHealth, EventEnvelope, PreflightRequest, PreflightResponse, ResolveActionRequest,
    ResolvedAction, SaveVarAnswers, SavedVarAnswers, SubmitAction, Submitted,
};

/// How long a freshly started daemon gets to answer.
const BOOT_WAIT: Duration = Duration::from_secs(15);

/// How long a dropped event stream is retried before giving up — long enough
/// to ride out the daemon restart a `veld self update` does mid-start.
const RESUME_WINDOW: Duration = Duration::from_secs(90);

pub struct EngineClient {
    http: reqwest::Client,
    base: String,
    /// The daemon's version, for the one error only it can explain.
    daemon_version: String,
}

#[derive(Debug)]
pub enum ClientError {
    /// The daemon answered with an error; the message is its own.
    Api {
        status: u16,
        message: String,
    },
    /// No daemon could be reached or started.
    Unreachable(String),
    Other(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api { message, .. } => f.write_str(message),
            Self::Unreachable(m) | Self::Other(m) => f.write_str(m),
        }
    }
}

impl ClientError {
    /// `503`: an update holds the lock. The CLI's own update gate exits 75 for
    /// this, and so does a refusal that arrives from the daemon.
    pub fn is_update_in_progress(&self) -> bool {
        matches!(self, Self::Api { status: 503, .. })
    }
}

impl EngineClient {
    /// Reach this instance's daemon, starting it if nothing answers, and make sure
    /// it drives the same database this CLI was built for.
    pub async fn connect() -> Result<Self, ClientError> {
        let mut client = Self {
            http: reqwest::Client::new(),
            base: veld_core::instance::daemon_base(),
            daemon_version: String::new(),
        };
        let health = match client.health().await {
            Some(h) => h,
            None => {
                start_daemon().await?;
                let deadline = tokio::time::Instant::now() + BOOT_WAIT;
                loop {
                    if let Some(h) = client.health().await {
                        break h;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        let port = veld_core::instance::daemon_port();
                        return Err(ClientError::Unreachable(format!(
                            "veld's daemon did not come up on port {port}. Its log: \
                             ~/.veld/daemon-{port}.log (or `veld self doctor` for an installed one)."
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        };
        client.check_same_database(&health)?;
        client.daemon_version = health.version;
        Ok(client)
    }

    async fn health(&self) -> Option<EngineHealth> {
        let resp = self
            .http
            .get(format!("{}/api/health", self.base))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        resp.json::<EngineHealth>().await.ok()
    }

    /// Refuse a daemon that drives a different database: it would start and stop
    /// runs this CLI's database never sees (#306 — a source-built CLI on the dev
    /// database talking to the installed daemon on the default port).
    fn check_same_database(&self, health: &EngineHealth) -> Result<(), ClientError> {
        let Some(theirs) = &health.db_path else {
            // An older daemon that does not say; nothing to compare.
            return Ok(());
        };
        let Ok(ours) = veld_core::db::Db::default_path() else {
            return Ok(());
        };
        if same_database(theirs, &ours) {
            return Ok(());
        }
        Err(ClientError::Other(format!(
            "The veld daemon on port {} uses the database {}, but this veld uses {}. It would \
             start and stop runs this one cannot see, so it refuses.\n  A source build has its \
             own instance: `veld orchestrator start --preset dev-headless`, then use the `veld-dev-<worktree>` \
             wrapper it prints.",
            veld_core::instance::daemon_port(),
            theirs.display(),
            ours.display()
        )))
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ClientError> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .header("X-Veld-Request", "1")
            .json(body)
            .send()
            .await
            .map_err(|e| ClientError::Unreachable(format!("veld's daemon did not answer: {e}")))?;
        decode(resp).await.map_err(|e| self.explain(e))
    }

    /// A `404` from `/api/engine/*` means the daemon predates the engine API —
    /// the window after `veld self update` replaced this binary but before the daemon
    /// restarted onto the new one. Say that, not "404 Not Found".
    fn explain(&self, e: ClientError) -> ClientError {
        match e {
            // Only a bare 404 — no route at all. The engine's own 404s carry a
            // message ("no veld.json in that directory"), which stays.
            ClientError::Api {
                status: 404,
                ref message,
            } if message == BARE_NOT_FOUND => ClientError::Api {
                status: 404,
                message: format!(
                    "The running veld daemon ({}) is older than this veld ({}) and does not have \
                     the engine yet. It restarts onto the new version on its own after an \
                     update; if it does not, run `veld self doctor`.",
                    self.daemon_version,
                    veld_core::VERSION
                ),
            },
            other => other,
        }
    }

    pub async fn preflight(
        &self,
        req: &PreflightRequest,
    ) -> Result<PreflightResponse, ClientError> {
        self.post("/api/engine/preflight", req).await
    }

    pub async fn submit(&self, body: &SubmitAction) -> Result<String, ClientError> {
        let s: Submitted = self.post("/api/engine/actions", body).await?;
        Ok(s.action_id)
    }

    pub async fn cancel(&self, action_id: &str) -> Result<(), ClientError> {
        let resp = self
            .http
            .post(format!(
                "{}/api/engine/actions/{action_id}/cancel",
                self.base
            ))
            .header("X-Veld-Request", "1")
            .send()
            .await
            .map_err(|e| ClientError::Unreachable(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            // The engine's own words when it gave any ("a stop cannot be
            // interrupted"); the status otherwise.
            decode::<serde_json::Value>(resp).await.map(drop)
        }
    }

    pub async fn save_var_answers(
        &self,
        req: &SaveVarAnswers,
    ) -> Result<SavedVarAnswers, ClientError> {
        self.post("/api/engine/var-answers", req).await
    }

    pub async fn resolve_action(
        &self,
        req: &ResolveActionRequest,
    ) -> Result<ResolvedAction, ClientError> {
        self.post("/api/engine/resolve-action", req).await
    }

    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ClientError> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .query(query)
            .send()
            .await
            .map_err(|e| ClientError::Unreachable(format!("veld's daemon did not answer: {e}")))?;
        decode(resp).await.map_err(|e| self.explain(e))
    }

    /// Follow an action's events until it finishes, calling `on_event` for each.
    ///
    /// A dropped connection is not the end: the daemon may be restarting under a
    /// `veld self update` while the worker carries on, so the stream is reopened from
    /// the last `seq` seen for up to [`RESUME_WINDOW`].
    pub async fn follow(
        &self,
        action_id: &str,
        mut on_event: impl FnMut(EventEnvelope) -> bool,
    ) -> Result<(), ClientError> {
        let mut after = 0i64;
        let mut lost_since: Option<tokio::time::Instant> = None;
        loop {
            let resp = self
                .http
                .get(format!(
                    "{}/api/engine/actions/{action_id}/events",
                    self.base
                ))
                .query(&[("after", after.to_string())])
                .send()
                .await;
            let mut resp = match resp {
                Ok(r) if r.status().is_success() => {
                    lost_since = None;
                    r
                }
                Ok(r) => {
                    return Err(ClientError::Api {
                        status: r.status().as_u16(),
                        message: format!("the engine lost action {action_id} ({})", r.status()),
                    });
                }
                Err(e) => {
                    let since = *lost_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() > RESUME_WINDOW {
                        return Err(ClientError::Unreachable(format!(
                            "lost the daemon while following action {action_id}: {e}"
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            let mut buf: Vec<u8> = Vec::new();
            let mut got_any = false;
            let clean_end = loop {
                match resp.chunk().await {
                    Ok(Some(bytes)) => {
                        buf.extend_from_slice(&bytes);
                        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=pos).collect();
                            let Ok(env) = serde_json::from_slice::<EventEnvelope>(&line) else {
                                continue;
                            };
                            got_any = true;
                            after = env.seq;
                            let is_finished =
                                matches!(env.event, veld_engine::api::EngineEvent::Finished { .. });
                            if !on_event(env) || is_finished {
                                return Ok(());
                            }
                        }
                    }
                    Ok(None) => break true,
                    // The daemon went away mid-stream: reopen from `after`.
                    Err(_) => break false,
                }
            };
            // The daemon holds a stream open while its action runs and closes it
            // once the action is over and everything has been sent. So a clean end
            // with nothing new means the action ended without a final event (its
            // worker was lost and the daemon has said so, or it predates this
            // reader); anything else is a daemon restart — reopen from `after`.
            if clean_end && !got_any {
                return Ok(());
            }
            if !clean_end {
                lost_since.get_or_insert_with(tokio::time::Instant::now);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

/// Whether two database paths name one file — spelled the same, or the same
/// after resolving symlinks (`/tmp` vs `/private/tmp` on macOS).
fn same_database(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// What [`decode`] reports for a 404 with no body (`status.to_string()`) — no
/// route at all, as opposed to the engine's own 404s, which carry a message.
const BARE_NOT_FOUND: &str = "404 Not Found";

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, ClientError> {
    let status = resp.status();
    if status.is_success() {
        return resp
            .json::<T>()
            .await
            .map_err(|e| ClientError::Other(format!("unreadable answer from the daemon: {e}")));
    }
    let body = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_else(|| {
            if body.is_empty() {
                status.to_string()
            } else {
                body
            }
        });
    Err(ClientError::Api {
        status: status.as_u16(),
        message,
    })
}

/// Start this instance's daemon. The service manager's job when there is one
/// (it owns restarts); otherwise the daemon binary, detached into its own session
/// so closing this terminal does not take it down — the case on CI and in any
/// shell where `veld self setup` never ran.
async fn start_daemon() -> Result<(), ClientError> {
    let installed = !veld_core::instance::daemon_port_is_explicit();
    if installed && kick_service_manager().await {
        return Ok(());
    }
    let bin = daemon_binary(installed);
    let log = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".veld")
        .join(format!("daemon-{}.log", veld_core::instance::daemon_port()));
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map(std::process::Stdio::from)
        .unwrap_or_else(|_| std::process::Stdio::null());
    let mut cmd = std::process::Command::new(&bin);
    // A clean environment, as launchd or systemd would give it. Every node this
    // daemon ever runs inherits its environment, so passing this terminal's
    // through would hand one shell's tokens to every later run of every project
    // — the exact leak "only declared `{ "env": … }` names cross" exists to stop.
    cmd.env_clear().envs(daemon_env(std::env::vars()));
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr);
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches only the child.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn().map_err(|e| {
        ClientError::Unreachable(format!(
            "could not start veld's daemon ({}): {e}",
            bin.display()
        ))
    })?;
    // No service manager knows this daemon, so `veld self update` and `veld self uninstall`
    // find it by this file.
    let _ = std::fs::write(
        veld_core::instance::bootstrapped_daemon_pidfile(),
        child.id().to_string(),
    );
    Ok(())
}

/// What a daemon the CLI starts by hand keeps of this terminal's environment:
/// what launchd/systemd would give a user service, plus the instance selectors
/// (`VELD_*`) that make a dev or CI instance a separate one, the `XDG_*_HOME`
/// directories its database resolves through, and `RUST_LOG` for its own
/// diagnostics. Nothing else — not an ssh agent socket (a forwarded `ssh -A`
/// agent would reach every node of every project), and not
/// even `LANG`/`LC_*`, which the launchd agent does not have either: a node's
/// locale must not depend on which face happened to start the daemon (declare
/// it in the node's `env`).
/// The environment a hand-started daemon gets: [`daemon_env_passes`], with every
/// path-valued `VELD_*` made absolute. A relative one means one file from here
/// and another from the daemon's directory (and its workers', which run in the
/// project): pin the one this CLI resolved.
fn daemon_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    const PATHS: [&str; 4] = [
        "VELD_DB_PATH",
        "VELD_ENGINE_DIR",
        "VELD_PTY_DIR",
        "VELD_DAEMON_SOCK",
    ];
    vars.filter(|(k, _)| daemon_env_passes(k))
        .map(|(k, v)| {
            let path = std::path::Path::new(&v);
            if PATHS.contains(&k.as_str()) && !v.is_empty() && path.is_relative() {
                if let Ok(abs) = std::path::absolute(path) {
                    return (k, abs.to_string_lossy().into_owned());
                }
            }
            (k, v)
        })
        .collect()
}

fn daemon_env_passes(key: &str) -> bool {
    matches!(
        key,
        "HOME"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "PATH"
            | "TMPDIR"
            // Where data lives on Linux (`dirs::data_dir`): without these the
            // daemon would open another database than this CLI's.
            | "XDG_RUNTIME_DIR"
            | "XDG_DATA_HOME"
            | "XDG_CONFIG_HOME"
            | "XDG_STATE_HOME"
            | "XDG_CACHE_HOME"
    ) || key.starts_with("VELD_")
        || key == "RUST_LOG"
}

/// Ask launchd/systemd to start an installed daemon. `true` when it accepted.
async fn kick_service_manager() -> bool {
    let status = match std::env::consts::OS {
        "macos" => {
            let uid = unsafe { libc::getuid() };
            tokio::process::Command::new("launchctl")
                .args(["kickstart", &format!("gui/{uid}/dev.veld.daemon")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
        }
        "linux" => {
            tokio::process::Command::new("systemctl")
                .args(["--user", "start", "veld-daemon"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
        }
        _ => return false,
    };
    status.is_ok_and(|s| s.success())
}

/// The daemon binary to start: the installed one for the installed instance, this
/// build's sibling for a dev instance — never the other way round, or a dev CLI
/// would boot the installed daemon onto its database.
fn daemon_binary(installed: bool) -> std::path::PathBuf {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("veld-daemon")))
        .filter(|p| p.exists());
    if !installed {
        if let Some(s) = sibling {
            return s;
        }
    }
    veld_core::setup::which_self("veld-daemon")
        .ok()
        .or(sibling)
        .unwrap_or_else(|| "veld-daemon".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_database_spelled_two_ways_is_the_same_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("veld.db");
        std::fs::write(&real, b"").unwrap();
        let link = dir.path().join("alias.db");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(same_database(&real, &real));
        assert!(same_database(&real, &link));
        assert!(!same_database(&real, &dir.path().join("other.db")));
    }

    /// The daemon a CLI starts by hand keeps the instance selectors and what a
    /// service manager would give it — and none of the terminal's secrets.
    #[test]
    fn a_hand_started_daemon_gets_no_terminal_secrets() {
        for kept in ["HOME", "PATH", "VELD_DB_PATH", "VELD_DAEMON_PORT"] {
            assert!(daemon_env_passes(kept), "{kept}");
        }
        for dropped in [
            "AWS_SECRET_ACCESS_KEY",
            "SSH_AUTH_SOCK",
            "SUDO_USER",
            "LANG",
            "LC_ALL",
            "GITHUB_TOKEN",
            "DATABASE_URL",
            "FOO",
        ] {
            assert!(!daemon_env_passes(dropped), "{dropped}");
        }
    }

    /// A route the daemon does not have is a version gap; the engine's own
    /// "not found" (a directory with no veld.json) is the engine's answer.
    #[test]
    fn only_a_missing_route_reads_as_an_old_daemon() {
        let client = EngineClient {
            http: reqwest::Client::new(),
            base: String::new(),
            daemon_version: "1.0.0".into(),
        };
        let bare = client.explain(ClientError::Api {
            status: 404,
            message: BARE_NOT_FOUND.into(),
        });
        assert!(bare.to_string().contains("older than this veld"), "{bare}");
        let own = client.explain(ClientError::Api {
            status: 404,
            message: "no veld.json in that directory".into(),
        });
        assert_eq!(own.to_string(), "no veld.json in that directory");
    }

    #[test]
    fn relative_instance_paths_are_pinned_for_the_daemon() {
        let env = daemon_env(
            [
                ("VELD_DB_PATH".to_owned(), "rel/x.db".to_owned()),
                ("VELD_ENGINE_DIR".to_owned(), "eng".to_owned()),
                ("VELD_DAEMON_PORT".to_owned(), "19500".to_owned()),
                ("GITHUB_TOKEN".to_owned(), "t".to_owned()),
            ]
            .into_iter(),
        );
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert!(std::path::Path::new(&get("VELD_DB_PATH").unwrap()).is_absolute());
        assert!(std::path::Path::new(&get("VELD_ENGINE_DIR").unwrap()).is_absolute());
        assert_eq!(get("VELD_DAEMON_PORT").as_deref(), Some("19500"));
        assert_eq!(get("GITHUB_TOKEN"), None);
    }

    /// `decode` spells a bodyless 404 with the status's `Display`, which is what
    /// `explain` matches.
    #[test]
    fn the_bare_404_text_is_what_decode_writes() {
        assert_eq!(reqwest::StatusCode::NOT_FOUND.to_string(), BARE_NOT_FOUND);
    }
}
