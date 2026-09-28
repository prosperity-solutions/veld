//! Keeping blocking work — above all SQLite — off the runtime's worker threads.
//!
//! Every `Db` call is synchronous, and any of them can wait up to the
//! connection's 10-second `busy_timeout` whenever something else holds the write
//! lock: a GC pass, a migration, a `veld _log` writer, the CLI. A tokio worker
//! parked in that wait is one fewer to drive everything else, and there are only
//! as many workers as cores. Park them all and the daemon stops — **terminals
//! included**, because the PTY relay is ordinary tasks on the same runtime and its
//! socket I/O is driven by the same reactor. The holder keeps the shell alive, so
//! nothing is lost; keystrokes just queue and arrive in one burst seconds later.
//!
//! That is not hypothetical. Reproduced with a GC pass holding the write lock, a
//! keystroke's echo took **13.5 s** and a `GET /api/health`, which touches no
//! database at all, **12.5 s**, while the stats sampler, the health scan and the
//! UI's polled endpoints each sat in `Db::open()` waiting for the same lock. With
//! the database work here instead: 62 ms and 2 ms.
//!
//! Two shapes:
//!
//! - [`blocking`] for code that is synchronous all the way down — a request
//!   handler that only reads the database, the stats sample. Plain
//!   `spawn_blocking`.
//! - [`pass`] for a long background pass whose database calls are interleaved
//!   with real awaits (probes, signals, the helper socket) too finely to split —
//!   GC and the health scan. The whole future is driven by `Handle::block_on` on a
//!   blocking-pool thread: its awaits still work (the runtime's drivers serve them),
//!   but the pass as a whole occupies a thread from the blocking pool instead of a
//!   worker.
//!
//! A panic inside either is re-raised in the caller, so moving code here does not
//! change what a panic does — only which thread it happens on.
//!
//! [`blocking`] is **bounded** by [`SLOTS`]; [`pass`] is not. Parking a worker used
//! to be an accidental limit: with every worker stuck, the accept loop stopped too,
//! so a flood of requests waited in the kernel's backlog holding nothing. Off the
//! workers, every request would otherwise get a blocking thread and its own SQLite
//! connection for the whole busy wait — measured at 206 database fds for 100
//! requests during a held lock, against the 256-fd soft limit launchd gives the
//! daemon. Past that, the next `accept`, PTY open and `Db::open` all fail with
//! `EMFILE`. A request waiting for a slot holds only its socket. Passes are few and
//! long-lived (GC, the health scan), and must not queue behind the requests that
//! are waiting on *their* lock.

use std::future::Future;

/// How many [`blocking`] bodies run at once. Each is typically a `Db::open()` —
/// a connection plus its WAL and shared-memory files — so this is also a bound on
/// the daemon's database fds. Well above what the UI's polling needs concurrently,
/// well below the fd limit.
const MAX_SLOTS: usize = 16;

static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(MAX_SLOTS);

/// Run synchronous `f` on the blocking pool and return its result.
///
/// Waits for one of [`SLOTS`] first. The permit travels into the closure rather
/// than being held here, so a caller that is dropped mid-wait (a closed browser
/// tab) cannot free a slot the still-running body is using.
pub(crate) async fn blocking<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // `acquire` fails only once the semaphore is closed, which nothing does.
    let slot = SLOTS
        .acquire()
        .await
        .expect("offload slots are never closed");
    join(
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            f()
        })
        .await,
    )
}

/// Drive the future `f` builds to completion on a blocking-pool thread.
///
/// For background passes only. A request handler that needs this is a handler
/// whose database work should be separated from its awaits instead.
///
/// Dropping the returned future — the daemon aborting its background tasks on
/// shutdown — ends the pass at its next await, as it did when the pass ran on the
/// worker. A blocking-pool thread cannot be aborted, so without this the pass would
/// run on through a shutdown, and the runtime's drop would wait for it.
pub(crate) async fn pass<T, F, Fut>(f: F) -> T
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T>,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    // Held across the await below and dropped with this future, which is what
    // resolves `cancelled`.
    let (_cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
    join(
        tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                tokio::select! {
                    biased;
                    _ = cancelled => None,
                    v = f() => Some(v),
                }
            })
        })
        .await,
    )
    .expect("a pass is cancelled only once its caller is gone")
}

fn join<T>(joined: Result<T, tokio::task::JoinError>) -> T {
    match joined {
        Ok(v) => v,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        // A blocking task cannot be aborted; this is the runtime shutting down
        // underneath the caller, which is not coming back either way.
        Err(e) => panic!("blocking task did not complete: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    /// The property the module exists for: while offloaded work blocks, the
    /// runtime's workers keep serving other tasks. A worker parked directly in the
    /// same sleep would not answer the ping until it finished.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_blocked_offload_leaves_the_worker_free() {
        let slow = tokio::spawn(super::blocking(|| {
            std::thread::sleep(Duration::from_millis(800));
        }));
        let ping = tokio::time::timeout(Duration::from_millis(300), tokio::spawn(async {}));
        assert!(
            ping.await.is_ok(),
            "the only worker was parked by blocking work"
        );
        slow.await.unwrap();
    }

    /// A pass's awaits still complete — timers here, the helper socket and probe
    /// requests in production — and so does its blocking work, off the worker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_pass_can_await_and_block_without_parking_the_worker() {
        let slow = tokio::spawn(super::pass(|| async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            std::thread::sleep(Duration::from_millis(800));
            7
        }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let ping = tokio::time::timeout(Duration::from_millis(300), tokio::spawn(async {}));
        assert!(ping.await.is_ok(), "the only worker was parked by the pass");
        assert_eq!(slow.await.unwrap(), 7);
    }

    /// Aborting the task that awaits a pass stops the pass too, at its next await —
    /// the shutdown path relies on `abort()` meaning that.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn aborting_the_caller_ends_the_pass() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ticks = Arc::new(AtomicUsize::new(0));
        let counted = ticks.clone();
        let task = tokio::spawn(super::pass(move || async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                counted.fetch_add(1, Ordering::SeqCst);
            }
        }));
        tokio::time::sleep(Duration::from_millis(60)).await;
        task.abort();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let after_abort = ticks.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            after_abort,
            "the pass kept running"
        );
    }

    /// The fd bound: however many requests arrive during a held lock, no more
    /// than `MAX_SLOTS` bodies — connections — are live at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_more_than_the_slots_run_at_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (live, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let tasks: Vec<_> = (0..super::MAX_SLOTS * 3)
            .map(|_| {
                let (live, peak) = (live.clone(), peak.clone());
                tokio::spawn(super::blocking(move || {
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    live.fetch_sub(1, Ordering::SeqCst);
                }))
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak <= super::MAX_SLOTS, "{peak} bodies ran at once");
    }

    #[tokio::test]
    #[should_panic(expected = "boom")]
    async fn a_panic_is_raised_in_the_caller() {
        super::blocking(|| panic!("boom")).await
    }
}
