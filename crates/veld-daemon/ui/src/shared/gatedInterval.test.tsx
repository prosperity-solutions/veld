import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { gatedInterval } from "./gatedInterval";
import { HIDDEN_POLL_MS } from "./pollCadence";

let hidden = false;

function setHidden(next: boolean) {
  hidden = next;
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  hidden = false;
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "performance", "Date"] });
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("gatedInterval", () => {
  it("ticks at the visible cadence while shown", () => {
    const run = vi.fn();
    const stop = gatedInterval(run, 2000);
    vi.advanceTimersByTime(10_000);
    expect(run).toHaveBeenCalledTimes(5);
    stop();
  });

  it("does not run on start — the caller owns the leading call", () => {
    const run = vi.fn();
    const stop = gatedInterval(run, 2000);
    expect(run).not.toHaveBeenCalled();
    stop();
  });

  it("drops to the hidden cadence while hidden", () => {
    const run = vi.fn();
    const stop = gatedInterval(run, 2000);
    vi.advanceTimersByTime(2000);
    expect(run).toHaveBeenCalledTimes(1);
    setHidden(true);
    vi.advanceTimersByTime(HIDDEN_POLL_MS * 2);
    // Two hidden minutes: two polls, not sixty.
    expect(run.mock.calls.length - 1).toBeLessThanOrEqual(2);
    expect(run.mock.calls.length - 1).toBeGreaterThanOrEqual(1);
    stop();
  });

  it("catches up on show when the last run is older than a tick", () => {
    const run = vi.fn();
    const stop = gatedInterval(run, 2000);
    setHidden(true);
    vi.advanceTimersByTime(30_000);
    expect(run).not.toHaveBeenCalled();
    setHidden(false);
    expect(run).toHaveBeenCalledTimes(1);
    stop();
  });

  it("does not catch up on a flicker shorter than a tick", () => {
    const run = vi.fn();
    const stop = gatedInterval(run, 2000);
    vi.advanceTimersByTime(2000);
    expect(run).toHaveBeenCalledTimes(1);
    setHidden(true);
    vi.advanceTimersByTime(500);
    setHidden(false);
    expect(run).toHaveBeenCalledTimes(1);
    stop();
  });

  it("does not double-poll on the tick right after a catch-up", () => {
    const run = vi.fn();
    const stop = gatedInterval(run, 2000);
    setHidden(true);
    vi.advanceTimersByTime(1500);
    vi.advanceTimersByTime(2000);
    setHidden(false);
    const afterShow = run.mock.calls.length;
    vi.advanceTimersByTime(500);
    expect(run.mock.calls.length).toBe(afterShow);
    stop();
  });

  it("stops ticking and listening once cleaned up", () => {
    const run = vi.fn();
    const stop = gatedInterval(run, 2000);
    stop();
    vi.advanceTimersByTime(60_000);
    setHidden(true);
    setHidden(false);
    expect(run).not.toHaveBeenCalled();
  });
});
