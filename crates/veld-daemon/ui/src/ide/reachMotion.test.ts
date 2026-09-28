import { describe, expect, it } from "vitest";

import {
  aimReach,
  arrive,
  goneZone,
  LET_GO,
  leave,
  type Presence,
  presenceGone,
  presenceLook,
  presenceMoving,
  type ReachMotion,
  reachSettled,
  restingMotion,
  shownReach,
  stepPresence,
  stepReach,
} from "./reachMotion";
import {
  REACH_BUMP,
  REACH_LIFT,
  REACH_MARGIN,
  REACH_RADIUS,
  reachContains,
  trashReach,
} from "./trashReach";

const box = { left: 0, top: 100, right: 220, bottom: 130 };
const FRAME = 1 / 60;

/** Aims at (`x`, `y`) and lets `frames` frames pass with the pointer there. */
function hold(m: ReachMotion, x: number, y: number, frames = 1) {
  let next = aimReach(m, box, x, y);
  for (let i = 0; i < frames; i += 1) next = stepReach(next, box, x, y, FRAME);
  return next;
}

/** Carries the pointer through `ys` at `x`, a pixel a frame. */
function carry(m: ReachMotion, x: number, ys: number[]) {
  const seen: ReachMotion[] = [];
  for (const y of ys) {
    m = hold(m, x, y);
    seen.push(m);
  }
  return seen;
}

const path = (from: number, to: number) => {
  const step = from <= to ? 1 : -1;
  return Array.from({ length: Math.abs(to - from) + 1 }, (_, i) => from + i * step);
};

const tip = (m: ReachMotion) => m.lift + m.h;

/** A zone carried in from afar until it has the pointer, at full reach. */
function reached() {
  const steps = carry(restingMotion(), 110, path(box.top - REACH_RADIUS - 20, box.top - 50));
  return hold(steps.at(-1)!, 110, box.top - 50, 60);
}

describe("reaching", () => {
  it("eases towards the wanted shape instead of jumping to it", () => {
    const y = box.top - 60;
    const want = trashReach(box, 110, y, null);
    const frames = [];
    let m = aimReach(restingMotion(), box, 110, y);
    for (let i = 0; i < 30; i += 1) {
      m = stepReach(m, box, 110, y, FRAME);
      frames.push(m.h);
    }
    expect(frames[0]).toBeGreaterThan(0);
    expect(frames[0]).toBeLessThan(want.h / 4);
    // Out of rest slowly, then faster: eased in, not a linear ramp.
    expect(frames[1]! - frames[0]!).toBeGreaterThan(frames[0]!);
    for (let i = 1; i < frames.length; i += 1) {
      expect(frames[i]).toBeGreaterThanOrEqual(frames[i - 1]!);
      expect(frames[i]).toBeLessThanOrEqual(want.h + 1e-9);
    }
    expect(frames.at(-1)).toBeCloseTo(want.h, 1);
  });

  it("arrives and stops", () => {
    const m = hold(restingMotion(), 110, box.top - 60, 120);
    expect(reachSettled(m)).toBe(true);
    expect(m.h).toBe(trashReach(box, 110, box.top - 60, null).h);
  });

  it("never reaches past a worktree picked up beside it", () => {
    const y = box.top - 30;
    let m = aimReach(restingMotion(), box, 110, y);
    for (let i = 0; i < 120; i += 1) {
      m = stepReach(m, box, 110, y, FRAME);
      expect(tip(m)).toBeLessThanOrEqual(30 - REACH_MARGIN + 1e-9);
      expect(m.over).toBe(false);
    }
  });

  it("takes a pointer it catches up with, without a move", () => {
    // A quick dive: the wanted shape is already around the pointer, the drawn
    // one is still growing into it.
    let m = restingMotion();
    for (const y of path(box.top - REACH_RADIUS, box.top - 40)) m = aimReach(m, box, 110, y);
    expect(m.over).toBe(false);
    m = hold(m, 110, box.top - 40, 60);
    expect(m.over).toBe(true);
  });

  it("never loses a pointer moving straight down once it has it", () => {
    for (const x of [0, 30, 110, 190, 220]) {
      let had = false;
      for (const m of carry(restingMotion(), x, path(box.top - REACH_RADIUS - 20, box.bottom))) {
        if (had) expect(m.over).toBe(true);
        had ||= m.over;
      }
      expect(had).toBe(true);
    }
  });

  it("swings its tip round after a pointer moving sideways, and keeps it", () => {
    let m = reached();
    const y = box.top - 50;
    const seen = path(110, 40).map((x) => (m = hold(m, x, y)));
    expect(seen.every((s) => s.base === null)).toBe(true);
    // Trailing the pointer on the way, rather than stuck to it.
    expect(seen.some((s) => s.tx > (s.target?.tx ?? 0) + 1)).toBe(true);
    m = hold(m, 40, y, 60);
    expect(m.cx + m.tx).toBeCloseTo(40, 0);
    expect(m.over).toBe(true);
  });

  it("does not let go of a pointer that twitches out over the top", () => {
    const start = reached();
    const edge = box.top - tip(start);
    let m = carry(start, 110, path(box.top - 50, edge - REACH_MARGIN + 2)).at(-1)!;
    expect(m.over).toBe(false);
    expect(m.base).toBeNull();
    m = carry(m, 110, path(edge - REACH_MARGIN + 2, box.top - 50)).at(-1)!;
    expect(m.over).toBe(true);
  });
});

/** Replays pointer moves `[x, y, frames]` — each move aimed at, then that many
 *  frames stepped with the pointer there — checking every state on the way. */
function replay(moves: [number, number, number][], check: (m: ReachMotion, x: number, y: number) => void) {
  let m = restingMotion();
  for (const [x, y, frames] of moves) {
    m = aimReach(m, box, x, y);
    check(m, x, y);
    for (let f = 0; f < frames; f += 1) {
      m = stepReach(m, box, x, y, FRAME);
      check(m, x, y);
    }
  }
  return m;
}

describe("the drawn zone as the target", () => {
  it("says the pointer is on it exactly when the drawn zone covers it", () => {
    // Settled on a pointer, then a sideways move that slid the base out from
    // under it: found by a random walk, shrunk.
    replay(
      [
        [147, 59, 2],
        [122, 79, 60],
        [118, 81, 2],
        [122, 85, 2],
        [117, 81, 2],
        [113, 78, 2],
        [108, 76, 2],
        [110, 74, 2],
      ],
      (m, x, y) => expect(m.over).toBe(reachContains(box, shownReach(m, box), x, y)),
    );
  });

  it("comes to rest under a pointer that stops, even one it will not uncover", () => {
    // A target that has flattened below a pointer the lagging zone still has:
    // found by a random walk, shrunk.
    let m = replay(
      [
        [134, 67, 2],
        [130, 76, 2],
        [131, 80, 2],
        [129, 81, 2],
        [126, 81, 2],
        [128, 84, 2],
        [129, 88, 2],
        [132, 78, 2],
      ],
      () => {},
    );
    m = hold(m, 132, 78, 120);
    expect(m.over).toBe(true);
    expect(reachSettled(m)).toBe(true);
  });
});

describe("letting go", () => {
  it("lets go of a pointer that leaves upwards, back into the list", () => {
    const start = reached();
    expect(start.over).toBe(true);
    const out = carry(start, 110, path(box.top - 50, box.top - 80));
    expect(out.find((m) => !m.over)!.base).toBeNull();
    expect(out.at(-1)!.base).not.toBeNull();
  });

  it("springs back through rest, dents, and wobbles out", () => {
    let m = carry(reached(), 110, path(box.top - 50, box.top - 80)).at(-1)!;
    const tips: number[] = [];
    for (let i = 0; i < 120; i += 1) {
      m = stepReach(m, box, 110, box.top - 80, FRAME);
      tips.push(tip(m));
    }
    const low = Math.min(...tips);
    expect(low).toBeLessThan(0);
    // A dent, not a cave-in.
    expect(-low).toBeLessThan((REACH_LIFT + REACH_BUMP) / 2);
    // Back above rest after the dent: a wobble, not a thud.
    const lowAt = tips.indexOf(low);
    expect(Math.max(...tips.slice(lowAt))).toBeGreaterThan(0);
    expect(reachSettled(m)).toBe(true);
    expect(tip(m)).toBe(0);
  });

  it("keeps its dent inside the box", () => {
    const m = { ...restingMotion(), lift: -40, h: -40 };
    const shown = shownReach(m, box);
    expect(shown.lift + shown.h).toBeGreaterThanOrEqual(-(box.bottom - box.top) / 2);
  });

  it("lets go of a pointer that backs away before it is taken", () => {
    let m = hold(restingMotion(), 110, box.top - 40, 30);
    expect(m.over).toBe(false);
    const away = carry(m, 110, path(box.top - 40, box.top - 40 - LET_GO - 1));
    m = away.at(-1)!;
    expect(m.base).not.toBeNull();
    m = hold(m, 110, box.top - 40 - LET_GO - 1, 60);
    expect(tip(m)).toBeCloseTo(0, 1);
  });

  it("stays at rest for a pointer lingering where it let go", () => {
    let m = carry(reached(), 110, path(box.top - 50, box.top - 80)).at(-1)!;
    m = hold(m, 110, box.top - 80, 120);
    expect(tip(m)).toBe(0);
    m = hold(m, 150, box.top - 80, 60);
    expect(tip(m)).toBe(0);
  });

  it("reaches again as the pointer comes back, more the closer it gets", () => {
    let m = carry(reached(), 110, path(box.top - 50, box.top - 100)).at(-1)!;
    m = hold(m, 110, box.top - 100, 60);
    const tips: number[] = [];
    const overs: boolean[] = [];
    for (const gap of [90, 75, 60, 45, 30]) {
      m = carry(m, 110, path(box.top - gap - 14, box.top - gap)).at(-1)!;
      m = hold(m, 110, box.top - gap, 60);
      tips.push(tip(m));
      overs.push(m.over);
      // Never past the margin short of a pointer it has not taken: the zone
      // still has to be entered, not leap over it.
      if (!m.over) expect(tip(m)).toBeLessThanOrEqual(gap - REACH_MARGIN + 1e-9);
    }
    // Met the whole way in: the pointer is taken before it reaches the box, the
    // way a fresh approach takes it.
    expect(overs.at(-1)).toBe(true);
    expect(tips[0]).toBeGreaterThan(0);
    // Growing the closer it gets, and holding — not shrinking — once it is as
    // near the pointer as the margin lets it.
    for (let i = 1; i < tips.length; i += 1) {
      expect(tips[i]).toBeGreaterThanOrEqual(tips[i - 1]! - 1e-9);
    }
    expect(tips[2]).toBeGreaterThan(tips[0]!);
    // Less than a fresh approach would reach at the same spot: it is coming
    // back to a pointer that just turned it down.
    expect(tips[1]).toBeLessThan(tip(hold(restingMotion(), 110, box.top - 75, 60)));
  });

  it("eases rather than wobbles on the way back up", () => {
    let m = carry(reached(), 110, path(box.top - 50, box.top - 100)).at(-1)!;
    m = hold(m, 110, box.top - 100, 90);
    m = carry(m, 110, path(box.top - 100, box.top - 60)).at(-1)!;
    let last = tip(m);
    for (let i = 0; i < 90; i += 1) {
      m = stepReach(m, box, 110, box.top - 60, FRAME);
      expect(tip(m)).toBeGreaterThanOrEqual(last - 1e-9);
      last = tip(m);
    }
    expect(last).toBeGreaterThan(0);
  });

  it("takes a pointer that drops into the box after all", () => {
    let m = carry(reached(), 110, path(box.top - 50, box.top - 80)).at(-1)!;
    m = carry(m, 110, path(box.top - 80, box.top + 5)).at(-1)!;
    expect(m.base).toBeNull();
    m = hold(m, 110, box.top + 5, 120);
    expect(m.over).toBe(true);
  });

  it("reaches again for a pointer that went away and came back", () => {
    let m = carry(reached(), 110, path(box.top - 50, box.top - REACH_RADIUS - 10)).at(-1)!;
    expect(m.base).toBeNull();
    m = carry(m, 110, path(box.top - REACH_RADIUS - 10, box.top - 60)).at(-1)!;
    m = hold(m, 110, box.top - 60, 60);
    expect(tip(m)).toBeGreaterThan(0);
  });

  it("does not let go of a pointer overshooting the side of the rail", () => {
    let m = carry(reached(), 110, path(box.top - 50, box.top + 5)).at(-1)!;
    for (const x of path(110, box.right + 6)) m = hold(m, x, box.top + 5);
    expect(m.over).toBe(false);
    expect(m.base).toBeNull();
    expect(hold(m, 200, box.top + 5).over).toBe(true);
  });
});

describe("presence", () => {
  const run = (z: Presence, frames: number) => {
    const ps: number[] = [];
    for (let i = 0; i < frames; i += 1) {
      z = stepPresence(z, FRAME);
      ps.push(z.p);
    }
    return { z, ps };
  };

  it("arrives with a swell past its box and wobbles into it", () => {
    const { z, ps } = run(arrive(goneZone()), 90);
    expect(ps[0]).toBeGreaterThan(0);
    expect(Math.max(...ps)).toBeGreaterThan(1.05);
    expect(Math.max(...ps)).toBeLessThan(1.35);
    expect(presenceMoving(z)).toBe(false);
    expect(presenceLook(z)).toEqual({ sx: 1, sy: 1, opacity: 1 });
  });

  it("fades in quickly", () => {
    const { ps } = run(arrive(goneZone()), 6);
    expect(presenceLook({ p: ps.at(-1)!, v: 0, leaving: false }).opacity).toBe(1);
  });

  it("goes with a last swell, then collapses and fades out", () => {
    const { z, ps } = run(leave({ p: 1, v: 0, leaving: false }), 60);
    expect(Math.max(...ps)).toBeGreaterThan(1.02);
    expect(presenceGone(z)).toBe(true);
    expect(presenceMoving(z)).toBe(false);
    expect(Math.min(...ps)).toBe(0);
    expect(presenceLook(z).opacity).toBe(0);
    // Gone on the first reach of 0: no undershoot is ever drawn.
    const goneAt = ps.indexOf(0);
    expect(ps.slice(goneAt).every((p) => p === 0)).toBe(true);
  });

  it("arrives again from wherever a leave had got to", () => {
    const half = run(leave({ p: 1, v: 0, leaving: false }), 8).z;
    expect(half.p).toBeGreaterThan(0);
    const back = arrive(half);
    expect(back.p).toBe(half.p);
    expect(presenceMoving(run(back, 120).z)).toBe(false);
  });
});
