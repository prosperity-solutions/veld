import { describe, expect, it } from "vitest";

import {
  REACH_BUMP,
  REACH_LIFT,
  REACH_MARGIN,
  REACH_RADIUS,
  type Reach,
  reachContains,
  reachPath,
  trashReach,
} from "./trashReach";

/** A wide-rail trash: the dock's content width, a folded section's height. */
const box = { left: 0, top: 100, right: 220, bottom: 130 };
const FULL = REACH_LIFT + REACH_BUMP;

/** How far above the section's top the tip of a reach sits. */
const tip = (r: Reach) => r.lift + r.h;

/**
 * Carries a pointer through `ys` at `x`, one pixel per move the way a drag
 * reports it, threading each reach into the next. Returns the reach at every
 * step, and whether the pointer was on the zone there.
 */
function carry(x: number, ys: number[]) {
  let prev: Reach | null = null;
  return ys.map((y) => {
    const reach = trashReach(box, x, y, prev);
    prev = reach;
    return { y, gap: box.top - y, reach, over: reachContains(box, reach, x, y) };
  });
}

/** Every whole-pixel y from `from` to `to`, inclusive, in the order given. */
const path = (from: number, to: number) => {
  const step = from <= to ? 1 : -1;
  return Array.from({ length: Math.abs(to - from) + 1 }, (_, i) => from + i * step);
};

/** A pointer carried straight down from outside the radius to below the box. */
const descent = (x: number) =>
  carry(x, path(box.top - REACH_RADIUS - 20, box.bottom));

describe("trashReach", () => {
  it("is at rest outside the radius", () => {
    for (const step of descent(110).filter((s) => s.gap >= REACH_RADIUS)) {
      expect(step.reach.lift).toBe(0);
      expect(step.reach.h).toBe(0);
    }
  });

  it("grows the whole zone and the bump as the pointer closes in", () => {
    const steps = descent(110);
    const far = steps.find((s) => s.gap === 130)!.reach;
    const mid = steps.find((s) => s.gap === 90)!.reach;
    expect(far.lift).toBeGreaterThan(0);
    expect(far.h).toBeGreaterThan(0);
    expect(mid.lift).toBeGreaterThan(far.lift);
    expect(mid.h).toBeGreaterThan(far.h);
  });

  it("never grows past its maxima", () => {
    for (const { reach } of descent(110)) {
      expect(reach.lift).toBeLessThanOrEqual(REACH_LIFT);
      expect(reach.h).toBeLessThanOrEqual(REACH_BUMP);
    }
  });

  it("reaches full size for a worktree carried in from afar", () => {
    const steps = descent(110);
    expect(tip(steps.find((s) => s.gap === FULL - REACH_MARGIN)!.reach)).toBeCloseTo(FULL);
  });

  it("stops short of a worktree picked up right beside it", () => {
    for (const gap of [REACH_MARGIN + 1, 20, 30, 45]) {
      const r = trashReach(box, 110, box.top - gap, null);
      expect(tip(r)).toBeCloseTo(gap - REACH_MARGIN);
      expect(reachContains(box, r, 110, box.top - gap)).toBe(false);
    }
    expect(tip(trashReach(box, 110, box.top - REACH_MARGIN, null))).toBe(0);
  });

  it("lets a worktree picked up beside it move down into it", () => {
    const steps = carry(110, path(box.top - 30, box.top));
    expect(steps[0]!.over).toBe(false);
    expect(steps.at(-1)!.over).toBe(true);
  });

  it("holds at full size until the pointer is a margin inside", () => {
    for (const s of descent(110).filter(
      (s) => s.gap <= FULL && s.gap >= FULL - REACH_MARGIN,
    )) {
      expect(tip(s.reach)).toBeCloseTo(FULL);
    }
  });

  it("draws back a margin above the pointer once it is further in", () => {
    for (const s of descent(110).filter(
      (s) => s.gap < FULL - REACH_MARGIN && s.gap >= -REACH_MARGIN,
    )) {
      expect(tip(s.reach)).toBeCloseTo(s.gap + REACH_MARGIN);
    }
  });

  it("is flat once the pointer is a margin inside the box", () => {
    for (const s of descent(110).filter((s) => s.gap <= -REACH_MARGIN)) {
      expect(tip(s.reach)).toBe(0);
    }
  });

  it("is steady for a pointer that stops", () => {
    const [, held] = carry(110, [box.top - 30, box.top - 30]);
    const [first] = carry(110, [box.top - 30]);
    expect(held!.reach).toEqual(first!.reach);
    const inside = descent(110).find((s) => s.gap === FULL - 5)!;
    expect(trashReach(box, 110, inside.y, inside.reach)).toEqual(inside.reach);
  });

  it("rests for a pointer beside the rail", () => {
    const r = carry(box.right + REACH_RADIUS, path(box.top - 40, box.top + 10));
    expect(tip(r.at(-1)!.reach)).toBe(0);
  });

  it("moves its base part of the way along, clamped clear of the corners", () => {
    expect(trashReach(box, 110, 80, null).cx).toBe(110);
    expect(trashReach(box, 90, 80, null).cx).toBe(100);
    // 8px corner + 72px half-width.
    expect(trashReach(box, 0, 80, null).cx).toBe(80);
    expect(trashReach(box, 400, 80, null).cx).toBe(140);
  });

  it("stands upright for a pointer straight above the middle", () => {
    expect(trashReach(box, 110, box.top - 60, null).tx).toBe(0);
  });

  it("leans its tip towards a pointer off to one side", () => {
    const left = trashReach(box, 30, box.top - 60, null);
    const right = trashReach(box, 190, box.top - 60, null);
    expect(left.cx + left.tx).toBeLessThan(left.cx);
    expect(right.cx + right.tx).toBeGreaterThan(right.cx);
  });

  it("leans more the nearer the pointer, aiming along the line to it", () => {
    const leans = [150, 110, 80].map((gap) => trashReach(box, 30, box.top - gap, null).tx);
    expect(leans[1]).toBeLessThan(leans[0]!);
    expect(leans[2]).toBeLessThan(leans[1]!);
  });

  it("puts its tip right under a close pointer", () => {
    for (const x of [40, 70, 150, 180]) {
      const r = trashReach(box, x, box.top - 40, null);
      expect(r.cx + r.tx).toBeCloseTo(x);
    }
  });

  it("leans all the way into the corner for a pointer at or past the side", () => {
    for (const x of [-200, box.left]) {
      const r = trashReach(box, x, box.top - 30, null);
      expect(r.cx + r.tx).toBe(box.left);
    }
    for (const x of [box.right, box.right + 200]) {
      const r = trashReach(box, x, box.top - 30, null);
      expect(r.cx + r.tx).toBe(box.right);
    }
  });

  it("reaches less for a pointer off to the side", () => {
    const side = carry(box.right + 40, path(box.top - 200, box.top - 60)).at(-1)!;
    const centre = carry(110, path(box.top - 200, box.top - 60)).at(-1)!;
    expect(tip(side.reach)).toBeLessThan(tip(centre.reach));
  });
});

describe("reachContains", () => {
  it("never loses a pointer moving straight down once it has it", () => {
    for (const x of [0, 30, 110, 190, 220]) {
      let had = false;
      for (const { over } of descent(x)) {
        if (had) expect(over).toBe(true);
        had ||= over;
      }
      expect(had).toBe(true);
    }
  });

  it("takes a pointer carried in before it reaches the box", () => {
    const entered = descent(110).find((s) => s.over)!;
    expect(entered.gap).toBeGreaterThan(FULL - REACH_MARGIN);
  });

  it("covers the grown strip beside the bump", () => {
    const reach = { lift: REACH_LIFT, cx: 110, tx: 0, h: REACH_BUMP };
    expect(reachContains(box, reach, 10, box.top - REACH_LIFT + 1)).toBe(true);
    expect(reachContains(box, reach, 10, box.top - REACH_LIFT - 1)).toBe(false);
  });

  it("is only the box at rest", () => {
    const rest = { lift: 0, cx: 110, tx: 0, h: 0 };
    expect(reachContains(box, rest, 110, box.top - 1)).toBe(false);
    expect(reachContains(box, rest, 110, box.top + 1)).toBe(true);
    expect(reachContains(box, rest, box.right + 1, box.top + 1)).toBe(false);
    expect(reachContains(box, rest, 110, box.bottom + 1)).toBe(false);
  });

  it("is nothing for a box scrolled out of view", () => {
    const gone = { left: 0, top: 50, right: 220, bottom: 50 };
    expect(reachContains(gone, { lift: 16, cx: 110, tx: 0, h: 40 }, 110, 40)).toBe(false);
  });
});

describe("reachPath", () => {
  it("draws the plain rounded box at rest", () => {
    const d = reachPath(box, { lift: 0, cx: 110, tx: 0, h: 0 });
    expect(d.startsWith("M0,108")).toBe(true);
    // The bump's peak sits on the top edge.
    expect(d).toContain("110,100");
    expect(d.endsWith("Z")).toBe(true);
  });

  it("raises the whole top edge by the lift", () => {
    const d = reachPath(box, { lift: 10, cx: 110, tx: 0, h: 0 });
    expect(d.startsWith("M0,98")).toBe(true);
    expect(d).toContain("110,90");
    // The bottom stays put.
    expect(d).toContain(",130");
  });

  it("lifts the peak by the lift plus the bump", () => {
    expect(reachPath(box, { lift: 10, cx: 110, tx: 0, h: 20 })).toContain("110,70");
  });

  it("starts and ends the bump on the lifted edge", () => {
    const d = reachPath(box, { lift: 10, cx: 110, tx: 0, h: 20 });
    expect(d).toContain("L38,90");
    expect(d).toContain("L182,90");
  });

  it("peaks at a leaning tip, with the base where it was", () => {
    const d = reachPath(box, { lift: 10, cx: 110, tx: -40, h: 20 });
    expect(d).toContain("L70,70");
    expect(d).toContain("L38,90");
    expect(d).toContain("L182,90");
  });

  it("is the edge it hit-tests, leaning or not", () => {
    const reach = { lift: 10, cx: 110, tx: -50, h: 40 };
    // The steep flank, between the tip at 60 and the base's end at 38.
    expect(reachContains(box, reach, 50, box.top - 30)).toBe(true);
    expect(reachContains(box, reach, 45, box.top - 45)).toBe(false);
    // The long flank on the far side of the tip.
    expect(reachContains(box, reach, 120, box.top - 30)).toBe(true);
    expect(reachContains(box, reach, 160, box.top - 30)).toBe(false);
  });

  it("raises the side wall to meet a tip leaning into the corner", () => {
    const reach = { lift: 10, cx: 140, tx: 80, h: 40 };
    const d = reachPath(box, reach);
    // The right-hand top corner lands on the side at the tip's height, not on
    // the lifted edge: no ledge between the bump and the corner.
    expect(d).toContain(`${box.right},${box.top - 50 + 8}`);
    expect(reachContains(box, reach, box.right - 1, box.top - 48)).toBe(true);
  });

  it("keeps a flank leaning towards the side a slope, not a cliff", () => {
    // Tip 10px short of the side: the near flank runs out past it.
    const reach = { lift: 10, cx: 140, tx: 70, h: 40 };
    expect(reachContains(box, reach, box.right, box.top - 30)).toBe(true);
    // Cut off partway down the flank, not at the peak: the wall stays below it.
    expect(reachContains(box, reach, box.right, box.top - 48)).toBe(false);
  });

  it("draws nothing for a box scrolled out of view", () => {
    expect(
      reachPath({ left: 0, top: 50, right: 220, bottom: 50 }, { lift: 0, cx: 0, tx: 0, h: 0 }),
    ).toBe("");
  });
});
