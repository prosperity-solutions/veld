//! Moving an existing privileged install off its user-writable binary, with no
//! user action at all (#262, under #338's rule 1).
//!
//! # The problem this solves without a sudo prompt
//!
//! Every privileged install shipped so far runs its root LaunchDaemon out of
//! `$HOME/.local/lib/veld/veld-helper`. Relocating that binary needs root — and
//! #338's first rule is that no release in this chain may require a user to run
//! anything, because a security fix behind a manual `sudo` step reaches almost
//! nobody.
//!
//! The way out is that **the helper is already root**. It can create the
//! root-owned directory, put itself in it, and re-point its own service
//! definition, with nothing asked of anyone. That is what this module does, once,
//! on the first startup after the update that carries it.
//!
//! # Why a startup check here is not the startup check that does not work
//!
//! #262 is explicit that verifying the binary at process start cannot close the
//! escalation, because the process doing the checking would be the attacker's.
//! That is still true and this does not pretend otherwise. This is not a gate;
//! it is a *relocation*, and its security value is entirely in where the binary
//! ends up. A helper that has already been swapped will not run this code at
//! all — it runs the attacker's — and such a machine is no worse off than it is
//! today. What the relocation buys is that every machine which has *not* been
//! attacked stops being attackable.
//!
//! The signature check below is therefore about not laundering a payload, not
//! about detecting compromise: root must never take bytes out of a directory the
//! user can write and seal them into a directory the user cannot. Without it,
//! migration would be a service that installs the attacker's binary permanently
//! and as root, on their behalf.
//!
//! # Ordering, and what a crash leaves behind
//!
//! Binary and signature first, service definition last. Interrupted anywhere,
//! the machine is left with an intact definition pointing at a binary that
//! exists:
//!
//! * crash before the definition is rewritten → still the old path, still
//!   working, still vulnerable, and the next startup retries;
//! * crash after → the new path, and the binary is already there because it was
//!   written first.
//!
//! The reverse order has a state where launchd is pointed at a file that is not
//! there yet, which under `KeepAlive` is a permanent throttled crash loop with
//! no helper running and nothing left to repair it.

use std::path::{Path, PathBuf};

use tracing::{info, warn};
use veld_core::helper_gate::Gate;

/// Hidden argument that makes a helper process re-register the privileged
/// service and exit, instead of serving.
///
/// The re-registration cannot be done by the running helper itself: on macOS it
/// requires `launchctl bootout`, which kills the job — us — before
/// `bootstrap` could run. So migration spawns the freshly installed store binary
/// with this flag, detached into its own session so the `bootout` does not take
/// it down too, and that child does the swap from outside the job.
///
/// It runs `veld_core::setup`'s existing bootout → drain → bootstrap
/// choreography rather than a shell one-liner, because that code already handles
/// the exit-5 race, the stale-registration fallback, and sessions with no
/// bootstrap-capable domain — all of which a hand-rolled version would
/// rediscover in the field, on a root service.
pub const REREGISTER_FLAG: &str = "--reregister-service";

/// Re-register the privileged helper service from the definition already on
/// disk, then exit. The `--reregister-service` entry point.
pub async fn reregister_service() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let label = veld_core::setup::HELPER_LABEL_MACOS;
        let plist = veld_core::setup::helper_plist_path();
        veld_core::setup::bootout_and_drain_system_job(label).await;
        veld_core::setup::bootstrap_launchd_job("system", label, &plist, None, false).await?;
        info!("re-registered {label} from {}", plist.display());
    }
    #[cfg(not(target_os = "macos"))]
    {
        veld_core::setup::reload_and_restart_helper_unit().await?;
        info!("re-registered {}", veld_core::setup::HELPER_SERVICE_LINUX);
    }
    Ok(())
}

/// Move this privileged helper into the root-owned store if it is not there
/// already, and re-point the service at it.
///
/// Best-effort and silent about the cases that are not migrations: an
/// unprivileged helper, one already in the store, one nothing manages, and a
/// locally built (unsigned) helper all return without doing anything. Only a
/// migration that was *attempted and failed* warns, because that is the one
/// state somebody may need to see in a log.
///
/// `caddy_bin` and `gate` are this process's own resolved configuration, and
/// passing them is what keeps the rewritten definition equivalent to the one it
/// replaces. The gate especially: a migrated helper that lost its `--allow-uid`
/// would come back ungated, so the uid is written down explicitly — see
/// [`relocate`].
pub async fn migrate_to_root_owned_dir(privileged: bool, caddy_bin: Option<&Path>, gate: &Gate) {
    if !privileged {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if veld_core::paths::is_privileged_helper_path(&exe) {
        return;
    }
    // Already unreachable to the user? Then there is nothing here to fix. A
    // legacy system-paths install keeps its helper in `/usr/local/lib/veld`,
    // created under `sudo` and root-owned — it never had the bug, and moving it
    // would spend a re-registration and a restart on a machine that is already
    // safe. Asking about the *directory* rather than about which install shape
    // this is keeps that judgement in one place and makes it true by inspection
    // rather than by a path list.
    if exe
        .parent()
        .is_some_and(veld_core::helper_store::is_root_owned_and_locked)
    {
        return;
    }
    // Nothing to re-point if no service manager owns this process: a
    // directly-spawned helper has no registration, and rewriting a plist for a
    // job that is not loaded would change what happens at the *next* boot
    // without anybody having asked for it.
    if !crate::service_manager_owns_us().await {
        return;
    }
    // A gate this process could not resolve cannot be carried across the move —
    // see the `--allow-uid` note in `relocate`. Migrating anyway would trade a *gated* helper
    // in a writable directory for an *ungated* one in a root-owned directory,
    // which is not obviously the better of the two and is certainly not a
    // trade to make silently. The install stays as it is and `veld doctor`
    // keeps reporting the gate row it already reports.
    let Some(allow_uid) = gate.uid() else {
        warn!(
            source = gate.source().as_str(),
            "not moving the privileged helper into a root-owned directory: its peer-uid gate \
             could not be resolved, and the move would leave the socket ungated. Run \
             `veld setup privileged` to write the gate explicitly"
        );
        return;
    };

    match relocate(&exe, caddy_bin, allow_uid).await {
        Ok(store_bin) => {
            info!(
                from = %exe.display(),
                to = %store_bin.display(),
                "moved the privileged helper into its root-owned directory; re-registering"
            );
            spawn_reregister(&store_bin);
        }
        Err(Skip(reason)) => {
            info!(
                reason,
                "not moving the privileged helper into a root-owned directory"
            );
        }
        Err(Failed(e)) => {
            warn!(
                error = %format!("{e:#}"),
                "could not move the privileged helper into a root-owned directory; it keeps \
                 running from its current location and this will be retried on the next start"
            );
        }
    }
}

/// A migration that did not happen: either deliberately (`Skip`) or because
/// something went wrong (`Failed`). Kept apart so the log can distinguish "this
/// install is not a candidate" from "this install is, and it did not work".
enum MigrationError {
    Skip(&'static str),
    Failed(anyhow::Error),
}
use MigrationError::{Failed, Skip};

/// Copy this helper into the store and re-point the service definition at it.
async fn relocate(
    exe: &Path,
    caddy_bin: Option<&Path>,
    allow_uid: u32,
) -> Result<PathBuf, MigrationError> {
    // Read and verify before anything is created. `Candidate` reads the bytes
    // once and installs *those*, which is what stops a swap between the check
    // and the copy — the swap being the entire reason this directory is moving.
    let candidate = veld_core::helper_store::Candidate::read(exe).map_err(|_| {
        Skip(
            "this helper has no readable org signature beside it (a local build, or an \
              install that predates signing)",
        )
    })?;
    let running = env!("CARGO_PKG_VERSION");
    candidate.verified_version().map_err(|_| {
        Skip(
            "this helper is not signed with the org's key, so moving it into a root-owned \
              directory would only make an unverified binary harder to replace",
        )
    })?;
    candidate.install(running).map_err(Failed)?;

    let store_bin = veld_core::paths::privileged_helper_bin();

    // The definition last, and only once the binary it names is on disk.
    //
    // **`allow_uid` written explicitly is the step that would silently undo
    // #337 if it were left out.** The peer-uid gate is *derived* from the owner
    // of the directory the helper binary sits in, precisely so that installs
    // predating the `--allow-uid` flag end up gated with nobody running
    // anything. Moving the binary into a root-owned directory makes that
    // derivation answer `0`, which is deliberately refused as a gate — it would
    // admit only root and lock the user's own CLI out — so a migrated helper
    // with no flag would come back **ungated**: the exact state #337 exists to
    // remove, arriving as a side effect of a change meant to harden it. The
    // caller has already resolved the uid; writing it down at the one moment the
    // derivation stops being able to find it is what carries the gate across.
    veld_core::setup::write_helper_service_definition(&store_bin, caddy_bin, allow_uid)
        .map_err(Failed)?;

    Ok(store_bin)
}

/// Start the freshly installed helper with [`REREGISTER_FLAG`], detached from
/// this process's session.
///
/// `setsid` is the load-bearing part. `launchctl bootout` tears down the job's
/// whole process group, and this child is a member of it — so without its own
/// session the child would be killed by the very `bootout` it is running, and
/// the service would be left unregistered until the next boot.
fn spawn_reregister(store_bin: &Path) {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(store_bin);
    command
        .arg(REREGISTER_FLAG)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and is the documented way to detach
    // a child from its parent's session; nothing else runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match command.spawn() {
        Ok(_) => {}
        Err(e) => warn!(
            error = %e,
            "could not start the re-registration helper; the service definition already points \
             at the root-owned directory, so this converges at the next boot"
        ),
    }
}
