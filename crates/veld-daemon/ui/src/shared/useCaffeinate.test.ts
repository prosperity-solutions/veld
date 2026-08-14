import { describe, expect, it } from "vitest";

import type { CaffeinateState } from "../api";
import { announcesEnding, formatRemaining } from "./useCaffeinate";

describe("formatRemaining", () => {
  it("reads the way a person says it", () => {
    expect(formatRemaining(30)).toBe("under a minute");
    expect(formatRemaining(59)).toBe("under a minute");
    expect(formatRemaining(60)).toBe("1m");
    expect(formatRemaining(90)).toBe("1m");
    expect(formatRemaining(3600)).toBe("1h");
    expect(formatRemaining(3660)).toBe("1h 1m");
    expect(formatRemaining(4 * 3600 + 12 * 60)).toBe("4h 12m");
  });

  it("never renders a bare hour with a zero minute", () => {
    // "2h 0m" is what a naive template produces and nobody says.
    expect(formatRemaining(2 * 3600)).toBe("2h");
  });
});

/**
 * The store's own rules, exercised through the module's private surface.
 *
 * Worth pinning because this file is where two defects have already lived: an
 * ending toast that fired once per mounted component (three, after the cup, the
 * sharing note and the settings dialog all started reading this), and an
 * interval whose cleanup could never run, so the poll outlived every hold.
 * Both are transition logic, which is exactly what a component test would not
 * have caught either.
 */
describe("the shared store's transitions", () => {
  // The real predicate, not a restatement of it: a test that re-implements the
  // rule it is checking passes happily while the implementation drifts away
  // from it.
  const state = (active: boolean, reason: CaffeinateState["reason"]) =>
    ({ active, reason }) as CaffeinateState;
  const announces = (
    prev: { active: boolean; reason: string } | null,
    next: { active: boolean },
    requested: boolean,
  ) =>
    announcesEnding(
      prev ? state(prev.active, prev.reason as CaffeinateState["reason"]) : null,
      state(next.active, "none"),
      requested,
    );

  it("announces a manual hold ending that nobody asked for", () => {
    expect(announces({ active: true, reason: "manual" }, { active: false }, false)).toBe(true);
  });

  it("says nothing when the user switched it off themselves", () => {
    expect(announces({ active: true, reason: "manual" }, { active: false }, true)).toBe(false);
  });

  it("says nothing when an automatic hold ends", () => {
    // This fires every time a share stops — many times a day, for something the
    // user never turned on. It would be the busiest and least informative
    // notification in the app.
    expect(announces({ active: true, reason: "sharing" }, { active: false }, false)).toBe(false);
  });

  it("still announces when a hold the user asked for ends alongside a share", () => {
    // `both` is not `sharing`: the user did press something, and its ending is
    // news in the way an automatic one is not.
    expect(announces({ active: true, reason: "both" }, { active: false }, false)).toBe(true);
  });
});
