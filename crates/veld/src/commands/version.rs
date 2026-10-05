use crate::output;
use std::path::Path;
use std::process::Command;

/// How long to wait for the running helper to answer before falling back to the
/// binaries on disk. Short: this sits in front of `veld start` and `veld status`,
/// and a helper that is up answers its status request in milliseconds.
const RUNNING_HELPER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Print version information for all Veld binaries.
pub async fn print_version() {
    let cli_version = env!("CARGO_PKG_VERSION");

    let daemon_version = find_and_query_version("veld-daemon");

    println!("{}", output::bold("Veld"));
    println!("  veld           {cli_version}");
    println!(
        "  veld-daemon    {}",
        format_version(&daemon_version, cli_version)
    );
    let helper = match running_helper_version().await {
        Some(v) => format!("{v} (running)"),
        None => format_version(&find_and_query_version("veld-helper"), cli_version),
    };
    println!("  veld-helper    {helper}");
}

/// The version the **running** helper reports over its socket, or `None` when
/// nothing answers in time (not running, an older helper without the field, a
/// socket this user cannot reach).
///
/// This is the number that matters for a skew check, and asking the files on
/// disk is a guess at it. The guess was wrong in the field: on a privileged
/// install whose service had been pointed back at `~/.local/lib/veld`, the
/// store-first candidate list read a stale v16.74.0 out of the root-owned
/// directory while v16.80.1 was the helper actually serving requests — and
/// `veld start` refused to run over a mismatch that did not exist, naming no
/// source and offering no way around it.
///
/// The socket is chosen by setup mode, as `veld update` does, rather than by
/// `connect()`'s fallthrough, which could latch onto a stale user-level helper
/// while the privileged one is down.
async fn running_helper_version() -> Option<String> {
    let socket = if crate::commands::read_setup_mode().as_deref() == Some("privileged") {
        veld_core::helper::system_socket_path()
    } else {
        veld_core::helper::user_socket_path()
    };
    let client = veld_core::helper::HelperClient::new(&socket);
    tokio::time::timeout(RUNNING_HELPER_TIMEOUT, client.version())
        .await
        .ok()?
        .ok()?
}

/// Format a version result for display.
fn format_version(result: &VersionResult, cli_version: &str) -> String {
    match result {
        VersionResult::Ok(v) => v.clone(),
        VersionResult::NotFound => format!("{cli_version} (assumed — binary not found)"),
        VersionResult::ExecFailed(path) => {
            format!("{cli_version} (assumed — could not execute {path})")
        }
    }
}

enum VersionResult {
    /// Successfully queried the version.
    Ok(String),
    /// Binary not found at any candidate path.
    NotFound,
    /// Binary exists but `--version` failed (e.g. macOS killed it).
    ExecFailed(String),
}

/// Find a binary by checking known paths and query its version.
///
/// If a binary exists at a candidate path but `--version` fails (e.g. macOS
/// Gatekeeper kills it), stop searching — don't fall through to potentially
/// stale copies at other locations.
fn find_and_query_version(binary_name: &str) -> VersionResult {
    let candidates = binary_candidates(binary_name);
    for path in &candidates {
        if !Path::new(path).exists() {
            continue;
        }
        // Binary exists at this path. Try to query its version.
        match query_binary_version(path) {
            Some(v) => return VersionResult::Ok(v),
            None => return VersionResult::ExecFailed(path.clone()),
        }
    }
    VersionResult::NotFound
}

/// Build list of candidate paths for a binary.
///
/// The privileged helper's root-owned directory comes **first** (#262), because
/// on a migrated install that is the copy the service actually runs. `install.sh`
/// keeps writing the lib-dir copy too, so the two agree after a successful
/// update and diverge only when the install handoff was refused — and in that
/// state the lib-dir copy reports the *new* version for a helper still running
/// the old one, which is the one answer `check_version_mismatch` must not give.
fn binary_candidates(binary_name: &str) -> Vec<String> {
    let mut paths = Vec::new();
    // **Privileged installs only.** A machine that was once privileged and is
    // now unprivileged still has the root-owned store on disk, and an
    // unconditional candidate would have `veld version` report that stale root
    // helper as this install's — which nothing updates any more, so
    // `check_version_mismatch` would print a permanent, false skew warning after
    // every update.
    // Name-gated as well as mode-gated: the store holds the helper and nothing
    // else, so probing it for `veld-daemon` is meaningless today and would be
    // wrong the moment the store gained a second file.
    if binary_name == "veld-helper"
        && crate::commands::read_setup_mode().as_deref() == Some("privileged")
    {
        paths.push(
            veld_core::paths::privileged_helper_dir()
                .join(binary_name)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join(".local")
                .join("lib")
                .join("veld")
                .join(binary_name)
                .to_string_lossy()
                .into_owned(),
        );
    }
    paths.push(format!("/usr/local/lib/veld/{binary_name}"));
    paths
}

/// Query a binary's version by running `<path> --version` and extracting the
/// version string. Returns `None` if the binary doesn't exist or we can't
/// parse its output.
fn query_binary_version(path: &str) -> Option<String> {
    if !Path::new(path).exists() {
        return None;
    }

    let output = Command::new(path).arg("--version").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The binary prints "veld-helper 1.0.0" — take the last token.
    let version = stdout.split_whitespace().last()?.to_string();

    // Sanity check: version should contain a dot (e.g. "1.0.0").
    if version.contains('.') {
        Some(version)
    } else {
        None
    }
}

/// Where a helper version came from, so the mismatch message can say it.
#[derive(Debug, PartialEq)]
enum HelperVersionSource {
    /// Reported by the helper serving the socket.
    Running,
    /// Read from a binary on disk with `--version` — what a restart would run,
    /// not necessarily what runs now.
    OnDisk(String),
}

/// Check that the helper and daemon match the CLI version.
/// Returns `Ok(())` if everything is fine, or `Err(message)` with a
/// user-facing error string if there is a mismatch.
///
/// In privileged and unprivileged mode the helper is asked **first** (see
/// [`running_helper_version`]) and the binaries on disk are consulted only when
/// it does not answer; auto mode reads the disk only (see the body for why).
/// Either way the message
/// names which one was read, because "veld-helper is v16.74.0" with no source
/// left a user unable to tell a stale file from a stale process.
pub async fn check_version_mismatch() -> Result<(), String> {
    let cli_version = env!("CARGO_PKG_VERSION");
    let mut mismatches: Vec<String> = Vec::new();

    // The running helper is asked only where a service manager restarts it onto
    // a new file. Auto mode's helper is ephemeral: nothing restarts it after an
    // install, and only `veld update` stops it — which does nothing on a CLI that
    // is already current. Gating on it there would block `veld start` (the very
    // command that re-bootstraps it) behind advice that cannot work, so auto mode
    // keeps comparing against the file on disk, as before.
    let mode = crate::commands::read_setup_mode();
    let privileged = mode.as_deref() == Some("privileged");
    let running = if privileged || mode.as_deref() == Some("unprivileged") {
        running_helper_version().await
    } else {
        None
    };
    let mut running_helper_behind = false;
    let helper = helper_version_source(running, find_helper_on_disk);
    if let Some((v, source)) = helper {
        if let Some(line) = helper_mismatch(&v, &source, cli_version) {
            // Behind, not merely different: a helper *newer* than this CLI is a
            // stale `veld` earlier on PATH, and "the helper cannot restart"
            // would explain the wrong direction.
            running_helper_behind = matches!(source, HelperVersionSource::Running)
                && veld_core::setup::is_newer(cli_version, &v);
            mismatches.push(line);
        }
    }

    if let VersionResult::Ok(v) = find_and_query_version("veld-daemon") {
        if v != cli_version {
            mismatches.push(format!("veld-daemon is v{v} (expected v{cli_version})"));
        }
    }

    if mismatches.is_empty() {
        return Ok(());
    }
    // A running helper that is behind is usually one that refused to restart
    // onto its file, and `veld update` cannot fix that: on a CLI that is already
    // current it does nothing at all, so "run `veld update`" would loop. Name the
    // real remedy when that is the cause, and `veld doctor` when it might be.
    let remedy = if !running_helper_behind {
        "Run `veld update` to fix this.".to_string()
    } else if let Some((bin, cause)) = match privileged {
        // The system service is only this machine's helper in privileged mode;
        // a leftover registration on an unprivileged one is not what is behind.
        true => veld_core::setup::unverified_service_binary().await,
        false => None,
    } {
        format!(
            "The helper cannot restart onto {}: it is not signed with the org's key ({cause}). \
             To fix it, {}.",
            bin.display(),
            veld_core::setup::unverified_helper_remedy()
        )
    } else if privileged {
        // Only the privileged helper watches its own file and restarts onto it;
        // the unprivileged one is restarted by the installer, so there is no
        // window to wait out.
        "If you just updated, the helper restarts onto the new version within about 15 seconds \
         — retry then. Otherwise run `veld update`; if it says you are already on the latest \
         version, run `veld doctor` to see why the helper has not restarted."
            .to_string()
    } else {
        "Run `veld update`; if it says you are already on the latest version, run `veld doctor` \
         to see why the helper has not restarted."
            .to_string()
    };
    Err(format!(
        "Version mismatch detected: {}. {remedy}",
        mismatches.join(", ")
    ))
}

/// Which helper version the check compares against: the running helper's when
/// it answered, the file on disk otherwise.
///
/// The disk is the fallback and never the first answer, because the two differ
/// in exactly the case that matters — a helper that refused to restart onto a
/// new file is old, while the file says it is new. `disk` is a closure so the
/// files are not read when the helper answered.
fn helper_version_source(
    running: Option<String>,
    disk: impl FnOnce() -> Option<(String, String)>,
) -> Option<(String, HelperVersionSource)> {
    match running {
        Some(v) => Some((v, HelperVersionSource::Running)),
        None => disk().map(|(v, path)| (v, HelperVersionSource::OnDisk(path))),
    }
}

/// The helper's version as read from disk, with the path it came from.
///
/// [`find_and_query_version`]'s candidate order, so the fallback answers exactly
/// as the whole check used to — only now it can say which file it read.
fn find_helper_on_disk() -> Option<(String, String)> {
    binary_candidates("veld-helper")
        .into_iter()
        .find(|path| Path::new(path).exists())
        .and_then(|path| query_binary_version(&path).map(|v| (v, path)))
}

/// One mismatch line for the helper, or `None` when it matches.
fn helper_mismatch(
    version: &str,
    source: &HelperVersionSource,
    cli_version: &str,
) -> Option<String> {
    if version == cli_version {
        return None;
    }
    Some(match source {
        HelperVersionSource::Running => {
            format!("the running veld-helper is v{version} (expected v{cli_version})")
        }
        HelperVersionSource::OnDisk(path) => format!(
            "veld-helper at {path} is v{version} (expected v{cli_version}; the running helper \
             did not answer, so this is the file on disk)"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{HelperVersionSource, helper_mismatch, helper_version_source};

    /// The report's machine: the helper is still the old one, the file beside
    /// it is the new one. The running helper must win, and the disk must not
    /// even be read.
    #[test]
    fn the_running_helper_wins_over_the_file_on_disk() {
        let source = helper_version_source(Some("16.74.0".into()), || {
            panic!("the disk is read only when the helper does not answer")
        });
        assert_eq!(
            source,
            Some(("16.74.0".into(), HelperVersionSource::Running))
        );

        let source =
            helper_version_source(None, || Some(("16.81.1".into(), "/x/veld-helper".into())));
        assert_eq!(
            source,
            Some((
                "16.81.1".into(),
                HelperVersionSource::OnDisk("/x/veld-helper".into())
            ))
        );

        assert_eq!(helper_version_source(None, || None), None);
    }

    #[test]
    fn a_matching_helper_is_not_a_mismatch_whatever_the_source() {
        assert_eq!(
            helper_mismatch("1.2.3", &HelperVersionSource::Running, "1.2.3"),
            None
        );
        assert_eq!(
            helper_mismatch(
                "1.2.3",
                &HelperVersionSource::OnDisk("/x/veld-helper".into()),
                "1.2.3"
            ),
            None
        );
    }

    /// The field report: a mismatch that named neither the process nor the file,
    /// so a stale binary in the store read like a stale running helper.
    #[test]
    fn a_mismatch_names_where_the_version_came_from() {
        let running =
            helper_mismatch("16.74.0", &HelperVersionSource::Running, "16.80.1").expect("mismatch");
        assert!(
            running.contains("running veld-helper is v16.74.0"),
            "{running}"
        );

        let on_disk = helper_mismatch(
            "16.74.0",
            &HelperVersionSource::OnDisk("/var/db/veld-helper/veld-helper".into()),
            "16.80.1",
        )
        .expect("mismatch");
        assert!(
            on_disk.contains("/var/db/veld-helper/veld-helper"),
            "{on_disk}"
        );
        assert!(on_disk.contains("file on disk"), "{on_disk}");
    }
}
