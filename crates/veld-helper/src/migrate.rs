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

/// How many times the re-registration will try before giving up.
///
/// It is not a network call and it does not normally fail, so this is not about
/// flakiness — it is about what failure *costs* here. On macOS the first thing
/// this does is `bootout`, which removes the job; if the `bootstrap` after it
/// does not land, the machine has **no privileged helper at all** until the next
/// boot. Retrying is cheap and that outcome is not.
const REREGISTER_ATTEMPTS: u32 = 3;

/// Re-register the privileged helper service from the definition already on
/// disk, then exit. The `--reregister-service` entry point.
pub async fn reregister_service() -> anyhow::Result<()> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 1..=REREGISTER_ATTEMPTS {
        match reregister_once().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(
                    attempt,
                    error = %format!("{e:#}"),
                    "re-registering the privileged helper failed; retrying"
                );
                last = Some(e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    let error = last.expect("the loop runs at least once");
    // Loud, and it names the state rather than the call that failed: the service
    // definition on disk is already correct, so a reboot alone repairs this.
    // Whoever reads this log needs to know that before they reach for anything
    // more drastic.
    tracing::error!(
        error = %format!("{error:#}"),
        "could not re-register the privileged helper after {REREGISTER_ATTEMPTS} attempts. Its \
         service definition already points at the root-owned directory, so a reboot will start \
         it there; `sudo veld setup privileged` does the same without one"
    );
    Err(error)
}

/// One re-registration attempt.
async fn reregister_once() -> anyhow::Result<()> {
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
    let exe = std::env::current_exe().ok();
    let facts = Facts {
        privileged,
        exe_is_in_store: exe
            .as_deref()
            .is_some_and(veld_core::paths::is_privileged_helper_path),
        exe_dir_is_locked: exe
            .as_deref()
            .and_then(Path::parent)
            .is_some_and(veld_core::helper_store::is_root_owned_and_locked),
        // Only asked once the cheap checks have already decided this could be a
        // migration: it shells out to `launchctl`/`systemctl` and is bounded by a
        // timeout, so it is the expensive fact and belongs last.
        service_manager_owns_us: Decision::needs_service_manager_answer(privileged, &exe)
            && crate::service_manager_owns_us().await,
        gate_uid: gate.uid(),
    };

    match Decision::from(&facts) {
        Decision::NotACandidate => {}
        Decision::Migrate { allow_uid } => {
            let Some(exe) = exe else { return };
            match relocate(&exe, caddy_bin, allow_uid).await {
                Ok(store_bin) => {
                    info!(
                        from = %exe.display(),
                        to = %store_bin.display(),
                        gated = allow_uid.is_some(),
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
                        "could not move the privileged helper into a root-owned directory; it \
                         keeps running from its current location and this will be retried on the \
                         next start"
                    );
                }
            }
        }
    }
}

/// Everything the migration decision depends on, gathered so the decision itself
/// is a pure function.
///
/// Split out because the decision has five branches whose *order* is
/// load-bearing and none of which could be tested while they were `return`s
/// interleaved with `current_exe()`, a `launchctl` call and a filesystem walk.
/// A future edit that reorders them — putting the service-manager query before
/// the cheap checks, say, or the gate before "am I already in the store" —
/// would change what happens on real machines with every existing test still
/// green.
#[derive(Debug, Clone, Copy)]
struct Facts {
    /// This helper is the root system daemon.
    privileged: bool,
    /// Its binary is already the one in the root-owned store.
    exe_is_in_store: bool,
    /// Its binary sits in a directory the installing user cannot write —
    /// including every ancestor. See
    /// `veld_core::helper_store::is_root_owned_and_locked`.
    exe_dir_is_locked: bool,
    /// A service manager reports *this process* as the registered helper.
    service_manager_owns_us: bool,
    /// The peer-uid gate this process resolved, or `None` for an ungated socket.
    gate_uid: Option<u32>,
}

/// What to do about this install.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Nothing: not privileged, already moved, already safe, or unmanaged.
    NotACandidate,
    /// Relocate, writing `allow_uid` into the rewritten service definition.
    Migrate { allow_uid: Option<u32> },
}

impl Decision {
    fn from(facts: &Facts) -> Self {
        // An unprivileged helper runs as the user and serves a binary the user
        // already owns: there is no boundary here to move.
        if !facts.privileged {
            return Self::NotACandidate;
        }
        // Already done.
        if facts.exe_is_in_store {
            return Self::NotACandidate;
        }
        // Already out of the user's reach. A legacy system-paths install whose
        // whole chain is root-owned never had the bug, and moving it would spend
        // a re-registration and a restart to change nothing.
        if facts.exe_dir_is_locked {
            return Self::NotACandidate;
        }
        // Nothing to re-point if no service manager owns this process: a
        // directly-spawned helper has no registration, and rewriting a
        // definition for a job that is not loaded would change what happens at
        // the *next* boot without anybody having asked for it.
        if !facts.service_manager_owns_us {
            return Self::NotACandidate;
        }
        // **A gate that could not be resolved does not block the move.**
        //
        // It once did, and that was wrong. The install shapes whose gate
        // resolves to nothing (`RefusedRootLibDir`, `UnreadableLibDir`) are
        // ungated *today*, so carrying `None` across reproduces exactly what
        // they already have — while the move still removes the escalation,
        // which is the larger of the two problems and the one this issue is
        // about. Refusing to migrate would have left the one install shape that
        // is both ungated and vulnerable in that state forever.
        //
        // What must never happen is inventing a uid: writing `--allow-uid` for
        // a uid nobody has admits only root and locks the user's own CLI out of
        // its helper. `None` means the flag is not written at all, and the
        // helper's own derivation then answers the same way it does now.
        Self::Migrate {
            allow_uid: facts.gate_uid,
        }
    }

    /// Whether the expensive service-manager query is worth making at all.
    ///
    /// Mirrors the cheap prefix of [`Self::from`]: if those already say
    /// `NotACandidate`, asking `launchctl` costs a subprocess and a timeout on
    /// every helper start for an answer that cannot change the outcome.
    fn needs_service_manager_answer(privileged: bool, exe: &Option<std::path::PathBuf>) -> bool {
        let Some(exe) = exe.as_deref() else {
            return false;
        };
        privileged
            && !veld_core::paths::is_privileged_helper_path(exe)
            && !exe
                .parent()
                .is_some_and(veld_core::helper_store::is_root_owned_and_locked)
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
    allow_uid: Option<u32>,
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
    // #337 if it were left out**, and `None` writing no flag at all is the other
    // half of that rule. The peer-uid gate is *derived* from the owner
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

#[cfg(test)]
mod tests {
    use super::{Decision, Facts};

    /// A privileged helper in a user-writable directory, managed, gated: the
    /// shape every existing default install has, and the whole reason #262
    /// exists.
    fn vulnerable() -> Facts {
        Facts {
            privileged: true,
            exe_is_in_store: false,
            exe_dir_is_locked: false,
            service_manager_owns_us: true,
            gate_uid: Some(501),
        }
    }

    #[test]
    fn the_default_privileged_install_is_migrated_and_keeps_its_gate() {
        assert_eq!(
            Decision::from(&vulnerable()),
            Decision::Migrate {
                allow_uid: Some(501)
            }
        );
    }

    /// **The regression this table exists to stop.** An install whose gate could
    /// not be resolved is ungated *today*; migrating it keeps it ungated and
    /// removes the escalation, which is strictly better. Blocking the move here
    /// — which an earlier version of this code did — left the one install shape
    /// that is both ungated and vulnerable in that state permanently.
    #[test]
    fn an_unresolved_gate_still_migrates_and_writes_no_flag() {
        let facts = Facts {
            gate_uid: None,
            ..vulnerable()
        };
        assert_eq!(
            Decision::from(&facts),
            Decision::Migrate { allow_uid: None }
        );
    }

    /// Never invent a uid. Writing a flag for a uid nobody has would admit only
    /// root and lock the user's own CLI out of its helper.
    #[test]
    fn a_migration_never_invents_a_uid() {
        let facts = Facts {
            gate_uid: None,
            ..vulnerable()
        };
        match Decision::from(&facts) {
            Decision::Migrate { allow_uid } => assert!(allow_uid.is_none()),
            other => panic!("expected a migration, got {other:?}"),
        }
    }

    #[test]
    fn every_reason_not_to_migrate_is_honoured() {
        for (label, facts) in [
            (
                "an unprivileged helper serves a binary its own user owns",
                Facts {
                    privileged: false,
                    ..vulnerable()
                },
            ),
            (
                "already in the store",
                Facts {
                    exe_is_in_store: true,
                    ..vulnerable()
                },
            ),
            (
                "already out of the user's reach (a legacy system-paths install)",
                Facts {
                    exe_dir_is_locked: true,
                    ..vulnerable()
                },
            ),
            (
                "nothing would relaunch it, so nothing should be re-pointed",
                Facts {
                    service_manager_owns_us: false,
                    ..vulnerable()
                },
            ),
        ] {
            assert_eq!(Decision::from(&facts), Decision::NotACandidate, "{label}");
        }
    }

    /// The cheap prefix and the real decision must agree about when the
    /// expensive `launchctl`/`systemctl` query is pointless — otherwise every
    /// helper start pays a subprocess and a timeout for an answer that cannot
    /// change the outcome.
    #[test]
    fn the_service_manager_is_not_queried_when_the_answer_cannot_matter() {
        assert!(!Decision::needs_service_manager_answer(false, &None));
        assert!(!Decision::needs_service_manager_answer(true, &None));
        // A helper that is not privileged is never a candidate, whatever its
        // path.
        assert!(!Decision::needs_service_manager_answer(
            false,
            &Some(std::path::PathBuf::from("/tmp/veld-helper"))
        ));
    }
}
