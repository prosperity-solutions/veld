//! The ingest budget for `veld _log`: a token bucket in front of the database, and the
//! bookkeeping that keeps the *end* of a shed burst.
//!
//! # Why a budget at all
//!
//! `veld _log` writes every line a detached node prints. One crash-looping worker
//! (Redis "Connection is closed", pretty-printed JSON errors) wrote about 1.1 M lines an
//! hour for days — 7.75 M rows, a multi-GB database, and a `quick_check` that took
//! seconds. Retention is by age only, so nothing else bounds it.
//!
//! # What it does
//!
//! Each `veld _log` process owns one [`TokenBucket`]: [`LOG_RATE_PER_SEC`] lines per
//! second sustained, [`LOG_BURST`] in a burst. A line over budget is not stored; it is
//! recorded in a [`RateShed`]. The writer thread turns that record into:
//!
//! - a **notice on the run-level `internal` stream** saying how many lines the rate limit
//!   dropped. `internal`, not the child's own stream, for the same reason queue-full
//!   drops are reported there: the child can forge anything on its own stream, and a
//!   reader must be able to tell veld's claim from the program's. The wording names the
//!   rate limit so it is distinguishable from the database-not-keeping-up notice.
//! - the **last [`SHED_TAIL_LINES`] dropped lines**, written back as ordinary lines
//!   (original timestamps, original stream) once the burst is over or the process is
//!   stopping. A token bucket sheds the *tail* of a burst, and the tail of a crash
//!   loop's output is the fatal error that `veld start` and the orchestrator show the
//!   user. Keeping it costs at most [`SHED_TAIL_LINES`] rows per burst. A tail is kept
//!   only while it *is* the end: once a later line is admitted, the tail is discarded
//!   ([`LogGate::offer`]), because every log reader orders by row id, and rows written
//!   back after newer ones would read as the run's latest output.
//!
//! There is deliberately no de-duplication: identical lines are dropped or kept by the
//! budget alone, so what is stored is always a real prefix of what the node printed.
//!
//! The reader thread never waits on any of this: [`LogGate::offer`] is arithmetic,
//! plus a brief uncontended lock on [`RateShed`] only around a shed burst.
//!
//! Foreground ingest (`veld_core::process`'s `LineAssembler`/`LogTarget`, used by `veld
//! start` without `--detach` and by one-shot runs) is **not** covered; it has the same
//! shape of problem but a human watching it.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sustained lines per second one `veld _log` process stores.
///
/// An order of magnitude above anything a healthy server prints (the #332 incident's
/// mean was 4 rows/s and its busiest hour 17.5 rows/s), and below the 300 rows/s the
/// runaway worker produced — which, sustained, is the case it exists for.
pub const LOG_RATE_PER_SEC: f64 = 200.0;

/// Lines that may be stored in a burst before the sustained rate applies.
///
/// Sized so a start-up banner, a stack trace or a migration's progress output is never
/// touched: 2,000 lines is ten seconds of the sustained rate.
pub const LOG_BURST: f64 = 2_000.0;

/// Shed lines kept so the end of a burst survives.
pub const SHED_TAIL_LINES: usize = 50;

/// How long without a shed line counts as "the budget has returned".
///
/// One second is 200 tokens of refill, so a stream that merely hovers around the limit
/// keeps resetting this clock and never triggers a flush; only a pause does. That is what
/// bounds the flush's cost to [`SHED_TAIL_LINES`] per quiet second at worst.
pub const SHED_QUIET: Duration = Duration::from_secs(1);

/// The longest a notice waits while lines are still being shed, so a runaway process is
/// reported while it runs rather than only when it ends.
pub const SHED_NOTICE_INTERVAL: Duration = Duration::from_secs(10);

/// A log line as the pump carries it.
pub type Line = (chrono::DateTime<chrono::Utc>, String);

/// A token bucket with an injectable clock.
///
/// Time is passed in rather than read, so the tests drive it without sleeping.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    rate: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// A full bucket: `rate` tokens per second, at most `capacity` stored.
    pub fn new(rate: f64, capacity: f64, now: Instant) -> Self {
        Self {
            capacity,
            rate,
            tokens: capacity,
            last: now,
        }
    }

    /// The pump's bucket: [`LOG_RATE_PER_SEC`] sustained, [`LOG_BURST`] burst.
    pub fn for_log_pump(now: Instant) -> Self {
        Self::new(LOG_RATE_PER_SEC, LOG_BURST, now)
    }

    /// Spend one token. `false` means the line is over budget.
    ///
    /// A `now` earlier than the last call (a clock that is not monotonic) refills
    /// nothing rather than panicking or going negative.
    pub fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = self.last.max(now);
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The reader thread's side of the budget: the bucket, and whether the held tail is
/// still the end of the output.
pub struct LogGate {
    bucket: TokenBucket,
    /// Whether a line was shed since the last admitted one: the first line admitted
    /// after a shed burst makes that burst's held tail no longer the end.
    shed_since_admit: bool,
}

impl LogGate {
    /// The pump's gate, with a full [`TokenBucket::for_log_pump`].
    pub fn for_log_pump(now: Instant) -> Self {
        Self {
            bucket: TokenBucket::for_log_pump(now),
            shed_since_admit: false,
        }
    }

    /// Spend budget on one line. `Some` hands it back to be stored; `None` means it was
    /// shed into `shed`.
    pub fn offer(&mut self, now: Instant, line: Line, shed: &RateShed) -> Option<Line> {
        if !self.bucket.try_take(now) {
            shed.record(now, line);
            self.shed_since_admit = true;
            return None;
        }
        if std::mem::take(&mut self.shed_since_admit) {
            shed.discard_tail();
        }
        Some(line)
    }
}

#[derive(Default)]
struct ShedState {
    /// Lines shed since the last notice.
    dropped: u64,
    /// When the oldest line counted in `dropped` was shed.
    unreported_since: Option<Instant>,
    /// When a line was last shed.
    last_drop: Option<Instant>,
    /// The most recent shed lines, oldest first.
    tail: VecDeque<Line>,
    /// Bumped by every [`RateShed::discard_tail`], so a claimed tail that was
    /// superseded while out is not [`RateShed::restore`]d.
    tail_epoch: u64,
}

/// What the reader thread sheds, handed to the writer thread.
///
/// Owned by the writer for reporting, for the same reason the queue-full counter is: a
/// notice sent through the queue could only be delivered when the queue has room.
#[derive(Default)]
pub struct RateShed {
    state: Mutex<ShedState>,
}

/// Everything [`RateShed::claim`] handed out, so a failed write can [`RateShed::restore`] it.
pub struct ShedClaim {
    /// Lines shed since the previous notice.
    pub dropped: u64,
    unreported_since: Option<Instant>,
    /// The kept tail, present only when the burst is over (or the process is ending).
    pub tail: Option<Vec<Line>>,
    tail_epoch: u64,
}

impl RateShed {
    fn lock(&self) -> std::sync::MutexGuard<'_, ShedState> {
        // A panic on the other thread must not take the reader down with it: this
        // process is the read end of a server's stdout pipe.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record one line the budget refused. Called from the reader thread.
    pub fn record(&self, now: Instant, line: Line) {
        let mut s = self.lock();
        s.dropped += 1;
        s.unreported_since.get_or_insert(now);
        s.last_drop = Some(now);
        if s.tail.len() == SHED_TAIL_LINES {
            s.tail.pop_front();
        }
        s.tail.push_back(line);
    }

    /// Forget the held tail because a newer line was admitted ahead of it.
    ///
    /// Written back now, the tail would get row ids after that newer line, and every
    /// log reader orders by id: the stale burst would read as the most recent output.
    /// Its lines stay counted in the drop notice. Called by [`LogGate::offer`], only for
    /// the first admitted line after a shed one. A tail the writer has already claimed
    /// is still written ahead of that line (one writer, and the line waits in the queue
    /// behind it); one whose write failed is not put back by [`RateShed::restore`].
    pub fn discard_tail(&self) {
        let mut s = self.lock();
        s.tail.clear();
        s.tail_epoch += 1;
    }

    /// Whether anything is waiting to be reported or written back.
    pub fn pending(&self) -> bool {
        let s = self.lock();
        s.dropped > 0 || !s.tail.is_empty()
    }

    /// Take what is ready to be written, or `None` if nothing is yet.
    ///
    /// A notice is ready when the burst is over (no line shed for [`SHED_QUIET`]), when
    /// [`SHED_NOTICE_INTERVAL`] has passed since the first unreported drop, or when
    /// `finishing`. The tail is handed out only when the burst is over or `finishing`:
    /// writing it back while lines are still being shed would hand the budget straight
    /// back to the process that exceeded it. And only once `queue_drained` (or
    /// `finishing`, when the reader is gone and the queue empty): lines admitted before
    /// the shed burst may still be queued, and the tail written ahead of them would get
    /// the lower row ids — the burst's last line would no longer read as the last.
    pub fn claim(&self, now: Instant, finishing: bool, queue_drained: bool) -> Option<ShedClaim> {
        let mut s = self.lock();
        let over = finishing
            || s.last_drop
                .is_none_or(|t| now.saturating_duration_since(t) >= SHED_QUIET);
        let release_tail = !s.tail.is_empty() && (finishing || (over && queue_drained));
        let notice_due = s.dropped > 0
            && (over
                || s.unreported_since
                    .is_some_and(|t| now.saturating_duration_since(t) >= SHED_NOTICE_INTERVAL));
        if !(notice_due || release_tail) {
            return None;
        }
        let claim = ShedClaim {
            dropped: std::mem::take(&mut s.dropped),
            unreported_since: s.unreported_since.take(),
            tail: release_tail.then(|| s.tail.drain(..).collect()),
            tail_epoch: s.tail_epoch,
        };
        Some(claim)
    }

    /// Put a claim back after the database refused it, so the next pass retries.
    ///
    /// Without this a failed notice write would consume the count, recreating the
    /// silence the accounting exists to remove — in exactly the case (a database that
    /// is not keeping up) where drops happen.
    pub fn restore(&self, claim: ShedClaim) {
        let mut s = self.lock();
        s.dropped += claim.dropped;
        s.unreported_since = match (s.unreported_since, claim.unreported_since) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // A tail discarded while it was out is no longer the end of the output.
        if let Some(tail) = claim.tail.filter(|_| claim.tail_epoch == s.tail_epoch) {
            // The claimed lines are older than anything recorded since.
            let newer: Vec<Line> = s.tail.drain(..).collect();
            s.tail.extend(tail);
            s.tail.extend(newer);
            while s.tail.len() > SHED_TAIL_LINES {
                s.tail.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize) -> Line {
        (chrono::Utc::now(), format!("line {n}"))
    }

    #[test]
    fn a_full_bucket_allows_the_whole_burst_and_then_refuses() {
        let t0 = Instant::now();
        let mut b = TokenBucket::for_log_pump(t0);
        for i in 0..LOG_BURST as usize {
            assert!(b.try_take(t0), "burst line {i} must be admitted");
        }
        assert!(!b.try_take(t0), "line {} is over the burst", LOG_BURST);
    }

    #[test]
    fn it_refills_at_the_sustained_rate() {
        let t0 = Instant::now();
        let mut b = TokenBucket::for_log_pump(t0);
        for _ in 0..LOG_BURST as usize {
            assert!(b.try_take(t0));
        }
        // 100 ms is 20 tokens at 200/s.
        let t1 = t0 + Duration::from_millis(100);
        let admitted = (0..100).filter(|_| b.try_take(t1)).count();
        assert_eq!(admitted, 20);
        // A long pause refills to the burst and no further.
        let t2 = t1 + Duration::from_secs(3600);
        let admitted = (0..10_000).filter(|_| b.try_take(t2)).count();
        assert_eq!(admitted, LOG_BURST as usize);
    }

    #[test]
    fn a_steady_stream_at_the_limit_is_never_refused() {
        let t0 = Instant::now();
        let mut b = TokenBucket::for_log_pump(t0);
        // Drain the burst, then offer exactly 200 lines per second for ten seconds.
        for _ in 0..LOG_BURST as usize {
            assert!(b.try_take(t0));
        }
        let mut refused = 0;
        for i in 1..=2_000u64 {
            if !b.try_take(t0 + Duration::from_millis(i * 5)) {
                refused += 1;
            }
        }
        assert_eq!(refused, 0);
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_refill_or_panic() {
        let t0 = Instant::now() + Duration::from_secs(10);
        let mut b = TokenBucket::new(1.0, 1.0, t0);
        assert!(b.try_take(t0));
        assert!(!b.try_take(t0 - Duration::from_secs(5)));
    }

    #[test]
    fn the_tail_keeps_the_most_recent_lines_in_order() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        for i in 0..SHED_TAIL_LINES + 25 {
            shed.record(t0, line(i));
        }
        let claim = shed
            .claim(t0 + SHED_QUIET, false, true)
            .expect("quiet, so claimable");
        assert_eq!(claim.dropped as usize, SHED_TAIL_LINES + 25);
        let tail = claim.tail.expect("burst over, so the tail comes with it");
        assert_eq!(tail.len(), SHED_TAIL_LINES);
        assert_eq!(tail[0].1, "line 25");
        assert_eq!(
            tail[SHED_TAIL_LINES - 1].1,
            format!("line {}", SHED_TAIL_LINES + 24)
        );
        assert!(!shed.pending());
    }

    /// A gate whose bucket is empty at `t0`, so the next line is shed.
    fn drained_gate(t0: Instant, shed: &RateShed) -> LogGate {
        let mut gate = LogGate::for_log_pump(t0);
        for i in 0..LOG_BURST as usize {
            assert!(gate.offer(t0, line(i), shed).is_some());
        }
        gate
    }

    #[test]
    fn a_line_admitted_after_a_shed_burst_discards_its_tail_but_not_its_count() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        let mut gate = drained_gate(t0, &shed);
        for i in 0..5 {
            assert!(gate.offer(t0, line(i), &shed).is_none(), "over budget");
        }
        // Refilled: this line is admitted, so the shed burst is no longer the end.
        let later = t0 + Duration::from_millis(100);
        assert!(gate.offer(later, line(99), &shed).is_some());
        let claim = shed
            .claim(later + SHED_QUIET, false, true)
            .expect("drops to report");
        assert_eq!(claim.dropped, 5);
        assert!(claim.tail.is_none_or(|t| t.is_empty()));
        assert!(!shed.pending());
    }

    #[test]
    fn a_shed_burst_with_nothing_admitted_after_it_keeps_its_tail() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        let mut gate = drained_gate(t0, &shed);
        for i in 0..5 {
            assert!(gate.offer(t0, line(i), &shed).is_none());
        }
        let claim = shed
            .claim(t0 + SHED_QUIET, false, true)
            .expect("burst is over");
        assert_eq!(claim.tail.map(|t| t.len()), Some(5));
    }

    #[test]
    fn a_tail_discarded_while_claimed_is_not_restored() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        let mut gate = drained_gate(t0, &shed);
        assert!(gate.offer(t0, line(0), &shed).is_none());
        // The writer claims the tail; before its failed write is restored, a newer
        // line is admitted.
        let claim = shed
            .claim(t0 + SHED_QUIET, false, true)
            .expect("burst is over");
        assert!(gate.offer(t0 + SHED_QUIET, line(1), &shed).is_some());
        shed.restore(claim);
        let again = shed
            .claim(t0 + SHED_QUIET * 2, false, true)
            .expect("count restored");
        assert_eq!(again.dropped, 1);
        assert!(again.tail.is_none_or(|t| t.is_empty()));
    }

    #[test]
    fn a_tail_waits_for_the_queue_to_drain_but_its_notice_does_not() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        shed.record(t0, line(0));
        // The burst is over, but lines admitted before it may still be queued: the
        // tail written now would land ahead of them.
        let notice = shed
            .claim(t0 + SHED_QUIET, false, false)
            .expect("the count is reported");
        assert_eq!(notice.dropped, 1);
        assert!(notice.tail.is_none(), "tail held until the queue drains");
        assert!(shed.pending());
        assert!(shed.claim(t0 + SHED_QUIET, false, false).is_none());
        let tail = shed
            .claim(t0 + SHED_QUIET, false, true)
            .expect("drained releases the tail");
        assert_eq!(tail.dropped, 0);
        assert_eq!(tail.tail.map(|t| t.len()), Some(1));
    }

    #[test]
    fn nothing_is_released_while_lines_are_still_being_shed() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        shed.record(t0, line(0));
        assert!(shed.pending());
        // Still inside the quiet window and well inside the notice interval.
        assert!(shed.claim(t0 + SHED_QUIET / 2, false, true).is_none());
        // Shedding continues, so the quiet clock restarts.
        shed.record(t0 + SHED_QUIET, line(1));
        assert!(shed.claim(t0 + SHED_QUIET * 3 / 2, false, true).is_none());
        assert!(shed.claim(t0 + SHED_QUIET * 2, false, true).is_some());
    }

    #[test]
    fn a_runaway_is_reported_while_it_runs_but_its_tail_is_held_back() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        let mut now = t0;
        // Shedding continuously for longer than the notice interval.
        while now < t0 + SHED_NOTICE_INTERVAL + Duration::from_secs(1) {
            shed.record(now, line(0));
            now += Duration::from_millis(100);
        }
        let claim = shed.claim(now, false, true).expect("a notice is due");
        assert!(claim.dropped > 0);
        assert!(
            claim.tail.is_none(),
            "writing the tail back mid-burst would undo the limit"
        );
        // The held-back lines are still there for the end of the burst.
        assert!(shed.pending());
        let end = shed
            .claim(now, true, true)
            .expect("finishing releases the tail");
        assert_eq!(end.dropped, 0);
        assert_eq!(end.tail.map(|t| t.len()), Some(SHED_TAIL_LINES));
    }

    #[test]
    fn finishing_releases_everything_immediately() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        shed.record(t0, line(0));
        let claim = shed.claim(t0, true, true).expect("finishing always claims");
        assert_eq!(claim.dropped, 1);
        assert_eq!(claim.tail.map(|t| t.len()), Some(1));
        assert!(shed.claim(t0, true, true).is_none(), "claimed exactly once");
    }

    #[test]
    fn a_restored_claim_is_retried_and_keeps_line_order() {
        let shed = RateShed::default();
        let t0 = Instant::now();
        shed.record(t0, line(1));
        shed.record(t0, line(2));
        let claim = shed.claim(t0, true, true).unwrap();
        // A new line is shed while the (failed) write was in flight.
        shed.record(t0, line(3));
        shed.restore(claim);
        let again = shed.claim(t0, true, true).unwrap();
        assert_eq!(again.dropped, 3);
        let texts: Vec<String> = again.tail.unwrap().into_iter().map(|l| l.1).collect();
        assert_eq!(texts, ["line 1", "line 2", "line 3"]);
    }
}
