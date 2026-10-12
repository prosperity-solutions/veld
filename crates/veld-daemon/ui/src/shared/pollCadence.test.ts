import { describe, expect, it } from "vitest";
import { HIDDEN_POLL_MS, POLL_MS, pollDue, pollOnShow, TICK_SLACK_MS } from "./pollCadence";

describe("pollDue", () => {
  it("polls on every tick while visible, a little early or late", () => {
    expect(pollDue(false, POLL_MS)).toBe(true);
    expect(pollDue(false, POLL_MS - 2)).toBe(true);
    expect(pollDue(false, POLL_MS + 2)).toBe(true);
  });

  it("skips the tick straight after a catch-up poll on show", () => {
    expect(pollDue(false, 0)).toBe(false);
    expect(pollDue(false, TICK_SLACK_MS - 1)).toBe(false);
  });

  it("skips ticks while hidden until a hidden interval has passed", () => {
    expect(pollDue(true, POLL_MS)).toBe(false);
    expect(pollDue(true, HIDDEN_POLL_MS - POLL_MS)).toBe(false);
    expect(pollDue(true, HIDDEN_POLL_MS)).toBe(true);
  });

  it("does not let timer jitter push a hidden poll to the next tick", () => {
    // A tick due at 60s that fires at 59 998ms must poll, or the hidden rate
    // quietly becomes 65s — or 120s under Chromium's minute-aligned throttling.
    expect(pollDue(true, HIDDEN_POLL_MS - 2)).toBe(true);
  });

  it("is much slower hidden than visible — the point of #400", () => {
    expect(HIDDEN_POLL_MS).toBeGreaterThanOrEqual(10 * POLL_MS);
  });

  it("pins the hidden rate the docs quote", () => {
    // `desktop/ARCHITECTURE.md` and the dbNotice reasoning both say "a minute";
    // changing it should be a decision, not a drift.
    const aMinute = 60_000;
    expect(HIDDEN_POLL_MS).toBe(aMinute);
  });
});

describe("pollOnShow", () => {
  it("catches up at once after a long absence", () => {
    expect(pollOnShow(HIDDEN_POLL_MS)).toBe(true);
    expect(pollOnShow(POLL_MS)).toBe(true);
  });

  it("does not double up after a moment's cover", () => {
    expect(pollOnShow(POLL_MS - 1)).toBe(false);
  });
});
