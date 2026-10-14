//! The liveness lock an engine worker holds for as long as it runs.
//!
//! **Why a lock and not the PID.** The daemon has to tell a worker that is still
//! working from one that died, including across its own restart. A PID cannot
//! answer that: while the daemon was down the worker may have exited and its PID
//! been handed to something else, and "that PID is alive" would then keep an
//! action `running` forever — with recovery for its run switched off for good. An
//! `flock` is released by the kernel when the process holding it dies, however it
//! dies, and is never inherited by a recycled PID.
//!
//! One file per action under [`crate::instance::engine_dir`].

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

fn path_in(dir: &Path, action_id: &str) -> PathBuf {
    dir.join(format!("{action_id}.lock"))
}

fn try_lock(file: &File) -> std::io::Result<bool> {
    // SAFETY: flock only reads the fd.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// Held by the worker for its whole life. Dropping it (or the process dying)
/// releases the lock.
pub struct WorkerLock {
    _file: File,
    path: PathBuf,
}

impl WorkerLock {
    /// Take the lock for `action_id`. Fails if another process holds it.
    pub fn acquire(action_id: &str) -> std::io::Result<Self> {
        Self::acquire_in(&crate::instance::engine_dir(), action_id)
    }

    fn acquire_in(dir: &Path, action_id: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = path_in(dir, action_id);
        let existed = path.exists();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match try_lock(&file) {
            Ok(true) => {}
            Ok(false) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!("action {action_id} already has a worker"),
                ));
            }
            // A filesystem with no `flock` (some network home directories):
            // leave nothing behind that this call created.
            Err(e) => {
                // Only a file this call created: an existing one may be a live
                // worker's.
                if !existed {
                    let _ = std::fs::remove_file(&path);
                }
                return Err(e);
            }
        }
        Ok(Self { _file: file, path })
    }
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether a live worker holds `action_id`'s lock right now.
pub fn is_held(action_id: &str) -> bool {
    is_held_in(&crate::instance::engine_dir(), action_id)
}

fn is_held_in(dir: &Path, action_id: &str) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(path_in(dir, action_id))
    else {
        // No file: the worker never started or already cleaned up.
        return false;
    };
    // Taking it means nobody holds it; the probe's own lock is released when
    // `file` drops at the end of this function. An error is not "held": a
    // filesystem that cannot lock must not keep a dead action running for ever.
    match try_lock(&file) {
        Ok(taken) => !taken,
        Err(e) => {
            tracing::warn!(action = action_id, error = %e, "engine lock probe failed; treating as not held");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_while_alive_released_on_drop() {
        let dir = tempfile::TempDir::new().unwrap();
        let dir = dir.path();
        let id = "test-held";
        assert!(!is_held_in(dir, id));
        let lock = WorkerLock::acquire_in(dir, id).unwrap();
        assert!(is_held_in(dir, id));
        assert!(WorkerLock::acquire_in(dir, id).is_err());
        drop(lock);
        assert!(!is_held_in(dir, id));
    }
}
