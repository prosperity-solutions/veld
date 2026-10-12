import { describe, expect, it } from "vitest";
import { HIDDEN_POLL_MS, POLL_MS, pollDue, pollOnShow } from "./pollCadence";

describe("pollDue", () => {
  it("polls on every tick while visible", () => {
    expect(pollDue(false, 0)).toBe(true);
    expect(pollDue(false, POLL_MS)).toBe(true);
  });

  it("skips ticks while hidden until a hidden interval has passed", () => {
    expect(pollDue(true, POLL_MS)).toBe(false);
    expect(pollDue(true, HIDDEN_POLL_MS - 1)).toBe(false);
    expect(pollDue(true, HIDDEN_POLL_MS)).toBe(true);
  });

  it("is much slower hidden than visible — the point of #400", () => {
    expect(HIDDEN_POLL_MS).toBeGreaterThanOrEqual(10 * POLL_MS);
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
