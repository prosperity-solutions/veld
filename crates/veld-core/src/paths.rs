use std::path::{Path, PathBuf};

/// Resolve the veld lib directory.
///
/// Resolution order:
/// 1. `VELD_LIB_DIR` env var (for local dev — points at `target/debug/`)
/// 2. `~/.local/lib/veld` (user-level, default for new installs)
/// 3. `/usr/local/lib/veld` (legacy system installs)
pub fn lib_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("VELD_LIB_DIR") {
        return PathBuf::from(dir);
    }
    // Prefer user-local directory (new default).
    let user_dir = dirs::home_dir().map(|h| h.join(".local").join("lib").join("veld"));
    if let Some(ref ud) = user_dir {
        if ud.exists() {
            return ud.clone();
        }
    }
    // Fall back to system directory for existing installs.
    let system_dir = PathBuf::from("/usr/local/lib/veld");
    if system_dir.exists() {
        return system_dir;
    }
    // Default to user-local directory (never try to create system dir).
    user_dir.unwrap_or(system_dir)
}

/// Where the **daemon's** log lives: in the user's own `~/.veld`, never in the
/// install prefix.
///
/// launchd discards a job's stdout and stderr unless the plist names a file — but a
/// job whose named file it **cannot create does not run at all**: it exits
/// `EX_CONFIG` (78) before the program is reached, and with `KeepAlive` that is a
/// permanent throttled retry. Measured, not inferred, with a throwaway agent whose
/// `StandardOutPath` sat in a `0555` directory.
///
/// That is why the daemon does not log beside its binary the way the helper does.
/// The daemon is a **user** LaunchAgent in both setup modes, and on a legacy
/// `/usr/local` install its lib dir is `root:wheel 0755` — so naming
/// `<lib dir>/veld-daemon.log` there would have bricked the daemon on exactly the
/// machines that were working before. `~/.veld` is the user's, is where every other
/// piece of per-user veld state already lives (sockets, holder directories,
/// `spawn-logs`), and is removed wholesale by `veld uninstall`.
///
/// `home` is passed rather than read because `veld setup` can be running under
/// `sudo`, where `dirs::home_dir()` is root's and a log written there would be
/// unwritable by the job that needs it.
pub fn daemon_log_path_in(home: &Path) -> PathBuf {
    home.join(".veld").join("veld-daemon.log")
}

/// [`daemon_log_path_in`] for this user.
pub fn daemon_log_path() -> Option<PathBuf> {
    dirs::home_dir().as_deref().map(daemon_log_path_in)
}

/// Restrict a file to its owner.
///
/// Here rather than inline at the one call site because "a file veld writes
/// diagnostics into is owner-only" is a rule, and a rule with one caller today is
/// still the thing the second caller should reach for.
#[cfg(unix)]
pub fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn set_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Where a service binary's log lives: beside the binary, named after it.
///
/// The **helper's** rule, and only the helper's — see [`daemon_log_path_in`] for why
/// the daemon cannot share it. In privileged mode the helper is a root
/// `LaunchDaemon`, so root writing to a root-owned lib dir is exactly right, and
/// this is where `veld-helper.log` has always been.
///
/// `/tmp` when the binary has no parent, which only a relative bare filename
/// produces.
pub fn service_log_path(service_bin: &Path) -> PathBuf {
    let name = service_bin
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "veld-service".to_owned());
    match service_bin.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(format!("{name}.log")),
        _ => PathBuf::from(format!("/tmp/{name}.log")),
    }
}

/// Every path the `veld` CLI belonging to a binary in `exe_dir` could be at, in
/// the order they are tried. [`cli_for_exe`] takes the first that exists; this is
/// the same list, exposed so a diagnostic can say where it looked.
///
/// A pure function of the path: no environment, no filesystem. `install.sh` splits
/// a release across two directories — the CLI into `<prefix>/bin` (on `PATH`) and
/// the daemon and helper into `<prefix>/lib/veld` — so a **sibling** `veld` exists
/// in a build tree and in no install at all, which is why "beside me" alone
/// answered "no CLI" on every installed machine.
///
/// The second candidate is derived from the shape of the directory rather than
/// from [`lib_dir`]: a binary in something named `<prefix>/lib/veld` *is* installed
/// under `<prefix>`, wherever that prefix is, and its CLI is `<prefix>/bin/veld`.
/// That covers both install prefixes (`~/.local` and `/usr/local`) without naming
/// either — and, being structural, it says nothing about a dev build. A
/// `target/debug` daemon whose CLI has not been built must report *no* CLI rather
/// than reach for `~/.local/bin/veld`, which would drive the installed instance's
/// database and daemon from a dev instance's terminal.
pub fn cli_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![exe_dir.join("veld")];
    let in_lib_veld = exe_dir.file_name().is_some_and(|n| n == "veld")
        && exe_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "lib");
    if in_lib_veld {
        if let Some(prefix) = exe_dir.parent().and_then(Path::parent) {
            out.push(prefix.join("bin").join("veld"));
        }
    }
    out
}

/// The `veld` CLI belonging to a binary in `exe_dir`, or `None` if there is none.
///
/// Callers are the daemon — which bakes the answer into the terminal shims, passing
/// its own `current_exe()` directory — and `veld doctor`, which reports on them and
/// passes the directory of the daemon its plist actually names. One rule, so the two
/// cannot disagree about *what* a CLI is; they still have to be given the same
/// directory to agree about *which* one, and that is doctor's job, not this
/// function's.
///
/// Two more resolvers in this repo answer a related question differently and are
/// deliberately left alone here: `veld-daemon`'s `monitor::find_veld_binary` (which
/// falls back to `PATH` when restarting a run) and `management::spawn_veld`. Both
/// predate this and changing what they resolve would change when a run restarts —
/// tracked separately rather than folded into a shim fix.
pub fn cli_for_exe(exe_dir: &Path) -> Option<PathBuf> {
    cli_candidates(exe_dir)
        .into_iter()
        // `is_file` follows symlinks, which is deliberate: `~/.local/bin/veld` is a
        // real file today, but a symlink there is a normal way to manage a CLI and
        // points at one just as well.
        .find(|c| c.is_file())
}

pub fn caddy_bin() -> PathBuf {
    lib_dir().join("caddy")
}

pub fn caddy_data_dir() -> PathBuf {
    lib_dir().join("caddy-data")
}

pub fn dnsmasq_conf_dir() -> PathBuf {
    lib_dir().join("dnsmasq.d")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_layout_finds_the_cli_in_bin() {
        assert_eq!(
            cli_candidates(Path::new("/home/u/.local/lib/veld")),
            vec![
                PathBuf::from("/home/u/.local/lib/veld/veld"),
                PathBuf::from("/home/u/.local/bin/veld"),
            ],
            "the installed daemon has no sibling CLI — `<prefix>/bin/veld` is where it is"
        );
    }

    #[test]
    fn a_system_prefix_derives_its_own_bin_dir() {
        assert_eq!(
            cli_candidates(Path::new("/usr/local/lib/veld"))
                .last()
                .unwrap(),
            &PathBuf::from("/usr/local/bin/veld")
        );
    }

    #[test]
    fn a_dev_binary_is_never_pointed_at_the_installed_cli() {
        // The only answer for a build tree is its own sibling: reaching for
        // `~/.local/bin/veld` would drive the installed instance from a dev one.
        let dev = Path::new("/repo/target/debug");
        assert_eq!(cli_candidates(dev), vec![dev.join("veld")]);
        // `lib` alone is not the install shape either — the leaf must be `veld`.
        assert_eq!(
            cli_candidates(Path::new("/opt/thing/lib")),
            vec![PathBuf::from("/opt/thing/lib/veld")]
        );
    }

    #[test]
    fn an_installed_prefix_can_be_anywhere() {
        // The rule is the *shape* of the directory, not a known prefix, so a
        // relocated install resolves as well as a default one — and a test can
        // exercise the real path without being installed.
        assert_eq!(
            cli_candidates(Path::new("/tmp/t1/lib/veld"))
                .last()
                .unwrap(),
            &PathBuf::from("/tmp/t1/bin/veld")
        );
    }

    #[test]
    fn the_first_existing_candidate_wins() {
        let tmp = std::env::temp_dir().join(format!("veld-paths-{}", std::process::id()));
        let lib = tmp.join("lib/veld");
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        // Nothing installed yet: the feature is off rather than pointed at a
        // path that does not resolve.
        assert_eq!(cli_for_exe(&lib), None);
        std::fs::write(bin.join("veld"), "#!/bin/sh\n").unwrap();
        assert_eq!(cli_for_exe(&lib), Some(bin.join("veld")));
        // A sibling takes precedence — that is a dev tree or a self-contained
        // install, and either way it is the CLI that belongs to this binary.
        std::fs::write(lib.join("veld"), "#!/bin/sh\n").unwrap();
        assert_eq!(cli_for_exe(&lib), Some(lib.join("veld")));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn a_service_log_sits_beside_its_binary() {
        assert_eq!(
            service_log_path(Path::new("/home/u/.local/lib/veld/veld-daemon")),
            PathBuf::from("/home/u/.local/lib/veld/veld-daemon.log")
        );
        // A bare filename has no directory to log into; `/tmp` is the same
        // fallback the helper's plist has always used.
        assert_eq!(
            service_log_path(Path::new("veld-daemon")),
            PathBuf::from("/tmp/veld-daemon.log")
        );
    }
}
