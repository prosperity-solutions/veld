//! `veld _helper-install` — hand a freshly downloaded helper to the running
//! root helper so it can install it into its root-owned directory (#262).
//!
//! # Why the installer cannot just copy the file any more
//!
//! It used to. `install.sh` wrote `veld-helper` into `$HOME/.local/lib/veld` and
//! the root LaunchDaemon ran it from there — which is the whole of #247: a
//! directory the installing user can write, exec'd as root at every boot. Now the
//! privileged helper is served from a directory only root can write, so the
//! unprivileged installer cannot put anything in it.
//!
//! Rather than reach for `sudo` — which #338's rule 1 forbids, because a security
//! fix behind a password prompt reaches almost nobody — the installer downloads to
//! a path it *can* write and asks the **already-root** helper to install it. The
//! helper verifies the org signature and refuses anything older than itself before
//! the bytes move, so the fact that the caller is untrusted costs nothing: all the
//! caller supplies is a filename.
//!
//! # Silent on every path that is not this one
//!
//! `install.sh` calls this unconditionally, so most invocations are on machines
//! this does not apply to: unprivileged installs, privileged installs still
//! serving from the lib dir, and machines with no helper running. All of those
//! exit 0 with nothing printed, because on them `install.sh`'s own copy is still
//! the mechanism and there is nothing for a user to act on. Only a machine that
//! *is* served from the root-owned store, and where the install failed, says
//! anything — and that one has to, because on it the copy `install.sh` made will
//! never be executed.

use std::path::PathBuf;

use crate::output;

/// Exit code, so `install.sh` can carry on regardless — see the module doc.
pub async fn run(binary: PathBuf) -> i32 {
    if setup_mode().as_deref() != Some("privileged") {
        return 0;
    }

    // The path the *service manager* actually serves, never a guess from
    // `paths::lib_dir()`. Whether this install has been migrated is a fact about
    // the plist, and a machine carrying both a `~/.local` and a `/usr/local`
    // tree would have a guess answer for the wrong one.
    let Some(program) = veld_core::setup::privileged_helper_program().await else {
        return 0;
    };
    if !veld_core::paths::is_privileged_helper_path(&program) {
        // Not migrated yet: the helper is still served from a directory the
        // installer can write, so the copy it already made is the update. The
        // helper's own startup migration moves this install on its next
        // restart, and the release after that takes this branch instead.
        return 0;
    }

    if !binary.is_file() {
        output::print_error(
            &format!("no helper binary to install at {}", binary.display()),
            false,
        );
        return 1;
    }

    let client = match veld_core::helper::HelperClient::connect_privileged().await {
        Ok(c) => c,
        Err(e) => {
            // The one case that genuinely needs a person: the binary lives where
            // only root can write it and no root helper is answering, so nothing
            // unprivileged can complete this update. Name the repair rather than
            // the error.
            output::print_error(
                &format!(
                    "the privileged veld-helper is not answering ({e}), and its binary lives in a \
                     root-owned directory this installer cannot write. Run \
                     `sudo veld setup privileged` to reinstall it."
                ),
                false,
            );
            return 1;
        }
    };

    match client.install_helper(&binary).await {
        Ok(response) => {
            let version = response
                .data
                .as_ref()
                .and_then(|d| d.get("installed_version"))
                .and_then(|v| v.as_str())
                .unwrap_or("the new version");
            output::print_info(&format!(
                "veld-helper {version} installed into {}",
                veld_core::paths::privileged_helper_dir().display()
            ));
            remove_stale_lib_dir_helper();
            0
        }
        Err(e) => {
            output::print_error(
                &format!("the privileged veld-helper refused the new binary: {e}"),
                false,
            );
            1
        }
    }
}

/// The recorded setup mode, resolved for the user who actually ran the install.
///
/// `read_setup_mode` reads `$HOME/.veld/setup.json`, and under `sudo` that
/// `$HOME` is root's — which has no `setup.json`, so a root-run `install.sh`
/// would answer "not privileged", return 0, and silently leave the root-owned
/// store on the old binary while the CLI and daemon moved to the new release.
/// Nothing would print, and `watch_own_binary` watches the store, so nothing
/// would restart either.
///
/// `SUDO_USER` is how the rest of `veld-core::setup` resolves the real user for
/// exactly this reason.
fn setup_mode() -> Option<String> {
    if let Some(mode) = super::read_setup_mode() {
        return Some(mode);
    }
    let sudo_user = std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty())?;
    let home = std::path::PathBuf::from(if cfg!(target_os = "macos") {
        format!("/Users/{sudo_user}")
    } else {
        format!("/home/{sudo_user}")
    });
    let content = std::fs::read_to_string(home.join(".veld").join("setup.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("mode")
        .and_then(|m| m.as_str())
        .map(str::to_owned)
}

/// Delete the helper left in the user-writable lib dir.
///
/// `install.sh` still copies it there — it copies the whole tarball's payload
/// and does not know which install shape it is on — and on a migrated machine
/// that copy is inert: nothing execs it, the plist names the store and the binary
/// watcher watches the store. What it *is* is the findable target #262 set out to
/// remove ("a root auto-restarting binary in a user directory" is exactly what an
/// attacker or a coding agent greps for), plus a second file that will drift out
/// of step with the real one and confuse the next person to look.
///
/// Deliberately **after** a successful install and not before: until the store
/// has the new binary, the lib-dir copy is still what a not-yet-migrated helper
/// would relaunch onto, and deleting it early would leave launchd with a path
/// that has nothing at it.
///
/// Best-effort and silent. Failing to remove a file nothing runs is not worth
/// failing an update over.
fn remove_stale_lib_dir_helper() {
    let stale = veld_core::paths::lib_dir().join("veld-helper");
    if veld_core::paths::is_privileged_helper_path(&stale) {
        return;
    }
    let _ = std::fs::remove_file(veld_core::signing::sig_path_for(&stale));
    let _ = std::fs::remove_file(&stale);
}
