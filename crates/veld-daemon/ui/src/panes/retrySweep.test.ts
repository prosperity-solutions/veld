import { describe, expect, it } from "vitest";
import { RETRY_SWEEP_MS, isStalled, planSweep } from "./retrySweep";

describe("isStalled", () => {
  /**
   * `error` is a socket that dropped and a shell the daemon is still holding.
   * Everything else either has an answer already or has something in flight.
   */
  it("is only a terminal whose retry budget is spent", () => {
    expect(isStalled("error", false)).toBe(true);

    // A cycle is still running: it will make the next attempt itself, and
    // `reconnectTerminal` would cancel it and re-arm the budget from zero —
    // turning a backoff into a busy loop against a daemon that is not answering.
    expect(isStalled("error", true)).toBe(false);

    // `ended` is the one that would do visible damage: the shell exited, its code
    // is on the screen, and reconnecting replays that ending as though it had
    // just happened.
    expect(isStalled("ended", false)).toBe(false);
    expect(isStalled("live", false)).toBe(false);
    expect(isStalled("connecting", false)).toBe(false);
    // A config pane whose session is gone. It is offering Resume / Start fresh,
    // and a sweep must not answer that question on the user's behalf.
    expect(isStalled("idle", false)).toBe(false);
    expect(isStalled("absent", false)).toBe(false);
  });
});

describe("planSweep", () => {
  it("runs the first sweep of a page's life whatever the clock says", () => {
    // The sentinel, and the case a fake-timer clock starting near zero would
    // otherwise defer for the whole window.
    expect(planSweep(1, 0)).toEqual({ run: true, deferMs: null });
    expect(planSweep(Date.now(), 0)).toEqual({ run: true, deferMs: null });
  });

  it("defers a throttled sweep by what is left of the window, never drops it", () => {
    // The wake sequence: `visibilitychange` and the control socket coming up land
    // within a second or two of each other, and the *first* is the one made while
    // the daemon may still be down. Dropping the second discards the only trigger
    // that is evidence of anything.
    expect(planSweep(101_000, 100_000)).toEqual({ run: false, deferMs: RETRY_SWEEP_MS - 1000 });
    expect(planSweep(100_001, 100_000)).toEqual({ run: false, deferMs: RETRY_SWEEP_MS - 1 });
  });

  it("runs once the window has elapsed, and treats the boundary as elapsed", () => {
    expect(planSweep(100_000 + RETRY_SWEEP_MS, 100_000)).toEqual({ run: true, deferMs: null });
    expect(planSweep(100_000 + RETRY_SWEEP_MS + 1, 100_000)).toEqual({ run: true, deferMs: null });
  });

  it("runs rather than deferring when the clock appears to have gone backwards", () => {
    // A system time change between the two reads. Deferring by a negative would
    // fire immediately in a loop; the sign is what makes it a decision.
    expect(planSweep(99_000, 100_000)).toEqual({ run: true, deferMs: null });
  });

  it("takes the window as an argument so the constant is not the only thing tested", () => {
    expect(planSweep(1_500, 1_000, 400)).toEqual({ run: true, deferMs: null });
    expect(planSweep(1_500, 1_000, 5_000)).toEqual({ run: false, deferMs: 4_500 });
  });
});
