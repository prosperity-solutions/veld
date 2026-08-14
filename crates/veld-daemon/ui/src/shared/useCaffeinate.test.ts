import { describe, expect, it } from "vitest";

import type { CaffeinateState } from "../api";
import { announcesEnding, attributesToShares, formatRemaining } from "./useCaffeinate";

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
 * Which deadline the countdown may be attributed to.
 *
 * Pinned because the `"both"` case shipped wrong: the sharing panel appended
 * "when the share expires" to a number that was the *manual* hold's, since
 * `remaining_secs` is the later of the two deadlines. Two consumers each wrote
 * the condition out and one of them got it wrong, which is why the rule is one
 * exported function now and why this asserts the exclusion rather than the
 * happy path alone.
 */
describe("attributesToShares", () => {
  const state = (
    reason: CaffeinateState["reason"],
    sharing_bound_by_share: boolean,
  ) => ({ reason, sharing_bound_by_share }) as CaffeinateState;

  it("attributes the countdown to sharing when sharing is the only reason", () => {
    expect(attributesToShares(state("sharing", true))).toBe(true);
  });

  it("never attributes it under `both`, where the number is the later deadline", () => {
    // The defect this exists to prevent. `remaining_secs` is
    // `max(manual, sharing)`, so a manual 4h hold taken during a 2h share shows
    // the manual number — and blaming the share for it is the exact
    // mis-attribution the whole field was added to remove.
    expect(attributesToShares(state("both", true))).toBe(false);
  });

  it("says nothing for a hold the share is not bounding", () => {
    expect(attributesToShares(state("sharing", false))).toBe(false);
    expect(attributesToShares(state("manual", true))).toBe(false);
    expect(attributesToShares(state("none", true))).toBe(false);
  });

  it("treats an older daemon's silence as no attribution", () => {
    // The field is absent on a daemon that predates it; the nullish fallback
    // must read as "do not attribute" rather than throwing or claiming.
    expect(attributesToShares({ reason: "sharing" } as CaffeinateState)).toBe(false);
    expect(attributesToShares(null)).toBe(false);
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
