//! The point of the holder split, tested against real processes.
//!
//! The unit tests in `src/pty.rs` run their holder as a task inside the test
//! binary, because `current_exe()` there is the test harness and not
//! `veld-daemon`. That covers the protocol and every client-facing invariant, but
//! it cannot cover the one property the feature exists for: a shell outliving the
//! daemon process. This does, by driving the real binary and killing it.
//!
//! `SIGKILL`, deliberately, not a graceful shutdown — "plan for the worst" means
//! the interesting case is the daemon that never got to run any cleanup at all. A
//! design that only survived an orderly restart would not survive a crash, and a
//! crash is what a user hits at the least convenient moment.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Message as WsMessage, http};

/// Generous: a login shell on a cold CI box sources a lot of rc files.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the daemon gets to answer `/api/health` after being started.
const BOOT_TIMEOUT: Duration = Duration::from_secs(30);

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One daemon instance's environment: its own database, socket, port and holder
/// directory, so the test never touches the developer's installed veld.
struct Instance {
    dir: tempfile::TempDir,
    port: u16,
}

impl Instance {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // Bind to 0 and drop it: the daemon needs a port nobody else holds, and
        // the kernel's choice is better than a guess. The gap between releasing
        // it and the daemon binding it is a race in theory; in practice the
        // kernel does not immediately recycle a just-closed listener's port, and
        // the alternative (a fixed port) collides with a developer's real daemon.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        Self { dir, port }
    }

    fn db_path(&self) -> PathBuf {
        self.dir.path().join("veld.db")
    }

    /// Short, because a holder socket path lives under here and
    /// `sockaddr_un::sun_path` is 104 bytes on macOS.
    fn pty_dir(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Start a daemon against this instance's state. Returns the child so the
    /// test can kill it.
    fn start_daemon(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_veld-daemon"))
            .env("VELD_DB_PATH", self.db_path())
            .env("VELD_DAEMON_SOCK", self.dir.path().join("daemon.sock"))
            .env("VELD_DAEMON_PORT", self.port.to_string())
            .env("VELD_PTY_DIR", self.pty_dir())
            // Keep the daemon's own log out of the test output unless something
            // is being debugged.
            .env(
                "RUST_LOG",
                std::env::var("VELD_TEST_LOG").as_deref().unwrap_or("warn"),
            )
            .stdin(Stdio::null())
            // Both inherited: the daemon's own subscriber writes to stdout while
            // a holder's writes to stderr, and a failing test needs to see both.
            // `cargo test` swallows them unless run with `--nocapture`.
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn veld-daemon")
    }
}

/// Register a worktree so `POST /api/pty/tickets` can resolve one, and return its
/// id. The ticket endpoint reads the registry, which is the only reason this test
/// needs a database at all.
fn register_worktree(db_path: &Path, repo_root: &Path) -> i64 {
    let db = veld_core::db::Db::open_at(db_path).expect("open db");
    db.upsert_repo(repo_root, "testrepo").expect("upsert repo");
    let worktrees = db
        .sync_worktrees(
            repo_root,
            &[veld_core::db::DiscoveredWorktree {
                path: repo_root.display().to_string(),
                branch: "main".to_owned(),
                is_main: true,
            }],
        )
        .expect("sync worktrees");
    worktrees.first().expect("one worktree").id
}

async fn wait_for_health(instance: &Instance) {
    let client = reqwest::Client::new();
    let url = format!("{}/api/health", instance.base());
    let deadline = Instant::now() + BOOT_TIMEOUT;
    loop {
        if let Ok(res) = client.get(&url).send().await {
            if res.status().is_success() {
                return;
            }
        }
        assert!(Instant::now() < deadline, "daemon never became healthy");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Mint a ticket, returning it together with whether the daemon reports the
/// session as resumed — which is the daemon's own answer to "is that shell still
/// there", and therefore the assertion this whole test is about.
async fn mint(instance: &Instance, worktree_id: i64, session_id: &str) -> (String, bool) {
    let res = reqwest::Client::new()
        .post(format!("{}/api/pty/tickets", instance.base()))
        // The CSRF gate the WebSocket cannot have.
        .header("X-Veld-Request", "1")
        .json(&serde_json::json!({
            "worktree_id": worktree_id,
            "session_id": session_id,
        }))
        .send()
        .await
        .expect("mint request");
    let status = res.status();
    let body: serde_json::Value = res.json().await.expect("ticket json");
    assert!(status.is_success(), "ticket mint failed: {body}");
    (
        body["ticket"].as_str().expect("ticket").to_owned(),
        body["resumed"].as_bool().expect("resumed"),
    )
}

async fn attach(instance: &Instance, ticket: &str) -> Client {
    let mut req = format!(
        "ws://127.0.0.1:{}/api/pty/attach?ticket={ticket}&cols=90&rows=30",
        instance.port
    )
    .into_client_request()
    .expect("request");
    req.headers_mut().insert(
        http::header::ORIGIN,
        instance.base().parse().expect("origin"),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("websocket handshake");
    ws
}

/// Read until `want` appears in the terminal's output.
async fn read_until(ws: &mut Client, want: &str) -> String {
    use futures_util::StreamExt;
    let mut seen = String::new();
    loop {
        let msg = tokio::time::timeout(STEP_TIMEOUT, ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {want:?}; saw: {seen:?}"))
            .expect("socket closed early")
            .expect("socket error");
        match msg {
            WsMessage::Binary(b) => {
                seen.push_str(&String::from_utf8_lossy(&b));
                if seen.contains(want) {
                    return seen;
                }
            }
            WsMessage::Text(t) => {
                assert!(
                    !t.contains(r#""type":"exit""#),
                    "the shell exited before {want:?}; saw: {seen:?}"
                );
            }
            _ => {}
        }
    }
}

async fn send(ws: &mut Client, line: &str) {
    use futures_util::SinkExt;
    ws.send(WsMessage::Binary(line.as_bytes().to_vec().into()))
        .await
        .expect("send");
}

/// Count the holder sockets currently on disk.
fn holder_sockets(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("sock"))
                .count()
        })
        .unwrap_or(0)
}

async fn wait_for_sockets(dir: &Path, want: usize) {
    let deadline = Instant::now() + STEP_TIMEOUT;
    while holder_sockets(dir) != want {
        assert!(
            Instant::now() < deadline,
            "expected {want} holder socket(s) in {dir:?}, found {}",
            holder_sockets(dir)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A shell — and the state inside it — survives the daemon being killed outright.
#[tokio::test]
async fn a_shell_survives_the_daemon_being_killed() {
    let instance = Instance::new();
    let worktree_id = register_worktree(&instance.db_path(), instance.dir.path());
    let session_id = uuid::Uuid::new_v4().simple().to_string();

    let mut daemon = instance.start_daemon();
    wait_for_health(&instance).await;

    let (ticket, resumed) = mint(&instance, worktree_id, &session_id).await;
    assert!(
        !resumed,
        "a session that has never existed cannot be resumed"
    );
    let mut ws = attach(&instance, &ticket).await;

    // A shell variable is the proof: it survives only if this is the same shell
    // *process* later on, not a fresh one started in the same directory.
    send(&mut ws, "VELD_MARK=survived; printf 'set%s\\n' '-ok'\n").await;
    read_until(&mut ws, "set-ok").await;
    wait_for_sockets(&instance.pty_dir(), 1).await;

    // The worst case: no shutdown path runs at all.
    daemon.kill().expect("kill daemon");
    daemon.wait().expect("reap daemon");
    // The holder is not the daemon's to take with it.
    assert_eq!(
        holder_sockets(&instance.pty_dir()),
        1,
        "the holder must outlive the daemon"
    );

    let mut daemon = instance.start_daemon();
    wait_for_health(&instance).await;

    let (ticket, resumed) = mint(&instance, worktree_id, &session_id).await;
    assert!(
        resumed,
        "the new daemon must adopt the surviving session, not start a new one"
    );
    let mut ws = attach(&instance, &ticket).await;
    send(&mut ws, "printf 'mark=%s\\n' \"$VELD_MARK\"\n").await;
    // This is the whole feature: the shell that was running before the daemon
    // died is the shell answering now, with its variables intact.
    //
    // One read, and both assertions come out of what it returned. Reading twice
    // for the same marker would hang on the second call, which has nothing left
    // to find — the timeout that cost an afternoon while the feature underneath
    // it worked.
    let seen = read_until(&mut ws, "mark=survived").await;
    // The replay came from the *holder*: this daemon never saw the output that
    // produced it, so `set-ok` can only have arrived in the scrollback snapshot
    // the holder handed over at adoption.
    assert!(
        seen.contains("set-ok"),
        "the pre-restart scrollback must be replayed, saw: {seen:?}"
    );

    // Closing the tab is still what ends a shell.
    reqwest::Client::new()
        .delete(format!("{}/api/pty/sessions/{session_id}", instance.base()))
        .header("X-Veld-Request", "1")
        .send()
        .await
        .expect("delete session");
    wait_for_sockets(&instance.pty_dir(), 0).await;

    daemon.kill().expect("kill daemon");
    daemon.wait().expect("reap daemon");
}

/// A holder whose socket file is left behind must not haunt every later boot.
#[tokio::test]
async fn a_stale_socket_is_cleaned_up_at_startup() {
    let instance = Instance::new();
    register_worktree(&instance.db_path(), instance.dir.path());
    std::fs::create_dir_all(instance.pty_dir()).expect("create pty dir");
    // A file where a socket should be: what a `kill -9`'d holder leaves, since
    // only the holder itself removes its socket.
    let stale = instance.pty_dir().join("dead0000dead0000.sock");
    std::fs::write(&stale, b"").expect("write stale socket");

    let mut daemon = instance.start_daemon();
    wait_for_health(&instance).await;

    // Adoption runs before the port is served, so by the time health answers the
    // sweep has already happened.
    assert!(
        !stale.exists(),
        "a socket nobody answers must be removed, not probed on every boot"
    );

    daemon.kill().expect("kill daemon");
    daemon.wait().expect("reap daemon");
}
