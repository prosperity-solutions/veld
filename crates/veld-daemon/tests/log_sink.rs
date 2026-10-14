//! `veld-daemon --log-sink`, the real binary, as the detached pipeline runs it.
//!
//! The unit tests in `veld_core::logging::pump` drive the writer directly; this
//! is the process boundary: the frozen argv is parsed, stdin is drained to EOF,
//! and the lines land in the database named by `VELD_DB_PATH`.

use std::io::Write;
use std::process::{Command, Stdio};

use veld_core::db::{Db, LogFilter, LogStream};

fn sink(db: &std::path::Path, args: &[&str]) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_veld-daemon"))
        .args(args)
        .env("VELD_DB_PATH", db)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn veld-daemon --log-sink")
}

#[test]
fn log_sink_stores_each_stdin_line_under_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("sink.db");
    let mut child = sink(
        &db_path,
        &[
            "--log-sink",
            "--project-root",
            "/proj",
            "--run",
            "dev",
            "--run-id",
            "rid",
            "--node",
            "api",
            "--variant",
            "local",
        ],
    );
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"first\nsecond\npartial").unwrap();
        // Dropped here: EOF.
    }
    assert!(child.wait().unwrap().success());

    let db = Db::open_at(&db_path).unwrap();
    let rows = db
        .tail_logs(
            std::path::Path::new("/proj"),
            "dev",
            &LogFilter {
                streams: Some(vec![LogStream::Server.as_str()]),
                ..Default::default()
            },
            100,
        )
        .unwrap();
    let lines: Vec<_> = rows.iter().map(|r| r.line.as_str()).collect();
    assert_eq!(lines, ["first", "second", "partial"]);
}

/// The sink is the read end of a node's stdout: an argv it cannot use (a newer
/// worker's extra flag, a missing one) must still drain and exit cleanly rather
/// than SIGPIPE the node.
#[test]
fn log_sink_survives_an_argv_it_does_not_understand() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = sink(
        &dir.path().join("sink.db"),
        &["--log-sink", "--from-the-future", "x", "--node", "api"],
    );
    {
        let mut stdin = child.stdin.take().unwrap();
        // More than a pipe buffer: a sink that stopped reading would block or die here.
        let chunk = vec![b'x'; 1024];
        for _ in 0..256 {
            stdin.write_all(&chunk).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
    }
    assert!(child.wait().unwrap().success());
}
