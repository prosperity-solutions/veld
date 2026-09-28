/**
 * The trash's drop zone in time: how the drawn zone gets to the shape
 * [`trashReach`] asks for, when it gives up on a worktree, and how the zone
 * arrives when a drag starts and goes when it ends.
 *
 * `trashReach` says where the zone *wants* to be for a pointer; this is where
 * it *is*. The two differ because a shape that jumps with every pointer move
 * reads as a readout, not as something reaching — so the drawn zone follows the
 * wanted one on a spring:
 *
 * - **Reaching** is critically damped: it eases out of rest and into the
 *   wanted shape without overshooting, so it never springs past the margin
 *   `trashReach` keeps short of a pointer it has not taken.
 * - **Letting go** is underdamped: the zone springs back to rest, overshoots
 *   into a shallow dent, and wobbles out.
 *
 * It lets go when the pointer shows it is aiming somewhere else — it leaves the
 * zone upwards, back into the list behind the bump, rising more than
 * [`REACH_MARGIN`] above its tip (less is a hand that twitched; off a flank
 * below the tip is the bump's lean trailing a sideways move), or it backs away more than
 * [`LET_GO`] from the closest it came. Having let go, it measures its reach
 * from where it let go instead of from [`REACH_RADIUS`]: nothing there, and
 * growing again the closer the pointer comes back, until it is fully out again
 * by the time the pointer is at the trash. So a pointer that lingers where it
 * left does not have the bump rising straight back under it, and one that turns
 * back towards the trash is met the whole way in.
 *
 * The drawn zone is still the target — [`ReachMotion.over`] is measured against
 * it — and a zone that has the pointer never pulls away from under it: a frame
 * that would uncover the pointer is not taken. Only the pointer leaves.
 *
 * Pure: time comes in as `dt`, so the feel can be tested without a clock.
 */

import {
  REACH_MARGIN,
  REACH_RADIUS,
  type Reach,
  type ReachBox,
  reachContains,
  trashReach,
} from "./trashReach";

/** How far, in px, the pointer may back away from the closest it came before
 *  the zone gives up on it. */
export const LET_GO = 24;
/** Angular frequency of the reach, in rad/s: critically damped, so about 95%
 *  there after 4.7 / this seconds. */
export const REACH_EASE = 22;
/** The let-go springs, as [angular frequency in rad/s, damping ratio]. The bump
 *  is quicker than the lift, so the edge wobbles rather than bobbing as one. */
export const LIFT_WOBBLE: readonly [number, number] = [15, 0.38];
export const BUMP_WOBBLE: readonly [number, number] = [20, 0.38];

/** Below this distance and speed a spring counts as arrived. */
const SETTLE = 0.05;
/** The longest step integrated at once, in seconds — a spring stepped over a
 *  long frame gap would otherwise overshoot on numbers alone. */
const MAX_STEP = 1 / 30;
const SUBSTEPS_PER_SECOND = 240;

export interface ReachMotion {
  /** The shape the zone is heading for, as `trashReach` gives it — with less
   *  of its nearness after a let-go. Threaded move to move. */
  target: Reach | null;
  lift: number;
  liftV: number;
  h: number;
  hV: number;
  cx: number;
  tx: number;
  txV: number;
  /** After a let-go, the farthest the pointer has been since: the distance the
   *  reach is measured from until the pointer is taken or leaves
   *  [`REACH_RADIUS`]. `null` otherwise. */
  base: number | null;
  /** Whether the springs are the let-go's wobbly ones — from the let-go until
   *  the zone starts reaching again. */
  wobble: boolean;
  /** The pointer's closest distance to the box since it last came into
   *  [`REACH_RADIUS`] or the zone last let go. */
  nearest: number;
  /** Whether the pointer is on the zone as drawn. */
  over: boolean;
  /** Whether the zone has had the pointer since it was last clear of it — on
   *  it, or off it by no more than the margin. */
  held: boolean;
}

export function restingMotion(): ReachMotion {
  return {
    target: null,
    lift: 0,
    liftV: 0,
    h: 0,
    hV: 0,
    cx: 0,
    tx: 0,
    txV: 0,
    base: null,
    wobble: false,
    nearest: Number.POSITIVE_INFINITY,
    over: false,
    held: false,
  };
}

/** The shape to draw and hit-test. The dent a let-go overshoots into is kept
 *  within the box, however short scrolling has made it. */
export function shownReach(m: ReachMotion, box: ReachBox): Reach {
  const dent = -Math.max(0, box.bottom - box.top) / 4;
  return { lift: Math.max(m.lift, dent), cx: m.cx, tx: m.tx, h: Math.max(m.h, dent) };
}

const clamp01 = (v: number) => Math.min(Math.max(v, 0), 1);
const smoothstep = (t: number) => {
  const c = clamp01(t);
  return c * c * (3 - 2 * c);
};

/** The pointer's distance to the box: 0 inside it, the same measure
 *  `trashReach` eases its nearness over. */
function distance(box: ReachBox, x: number, y: number): number {
  const dx = Math.max(box.left - x, 0, x - box.right);
  const dy = Math.max(box.top - y, 0, y - box.bottom);
  return Math.hypot(dx, dy);
}

/**
 * The motion for a pointer that moved to (`x`, `y`): what the zone now wants,
 * whether it lets go, and whether the pointer is on it. Measuring the same
 * point twice changes nothing.
 */
export function aimReach(
  m: ReachMotion,
  box: ReachBox,
  x: number,
  y: number,
): ReachMotion {
  const d = distance(box, x, y);
  let { base, nearest, wobble } = m;
  const shown = shownReach(m, box);
  // The base moves with the pointer at once, unsprung, so the shape drawn after
  // this move is the one the pointer is on or not. A base that would slide out
  // from under a pointer it has stays where it was until the next move.
  const moved = trashReach(box, x, y, m.target).cx;
  const cx = reachContains(box, shown, x, y) &&
    !reachContains(box, shownReach({ ...m, cx: moved }, box), x, y)
    ? m.cx
    : moved;
  const over = reachContains(box, shownReach({ ...m, cx }, box), x, y);
  // Still with the zone: level with it, give or take the margin — anywhere up
  // to its tip, not only under its edge. A bump swinging round after a pointer
  // that moved sideways can trail it off a flank; that is the zone lagging, not
  // the pointer leaving.
  const near =
    x >= box.left &&
    x <= box.right &&
    y <= box.bottom &&
    y >= box.top - Math.max(shown.lift + shown.h, shown.lift, 0) - REACH_MARGIN;
  let held = over || (m.held && near);
  if (d > REACH_RADIUS) {
    // Out of reach: the next approach is a fresh one.
    base = null;
    nearest = d;
  } else {
    if (held) base = null;
    nearest = Math.min(nearest, d);
    // Out through the top edge, not off the side of the rail, which is only
    // the pointer overshooting a zone it is still on.
    const leftUpward = m.held && !near && x >= box.left && x <= box.right && y < box.top;
    if (leftUpward || (!over && d - nearest > LET_GO)) {
      base = d;
      nearest = d;
      held = false;
      wobble = true;
    }
  }
  if (base !== null) base = Math.max(base, d);
  // Scaling the nearness, not the result: the reach a pointer has already been
  // given is still the reach it keeps, so it holds rather than shrinking as the
  // pointer closes the last of the gap.
  const k = base === null || base <= 0 ? 1 : smoothstep((base - d) / base);
  const target = trashReach(box, x, y, m.target, k);
  // Reaching again: the pointer has come back far enough that the zone is
  // heading above where it is, rather than wobbling out around rest.
  if (wobble && target.lift + target.h > Math.max(m.lift + m.h, 0) + 0.5) wobble = false;
  return { ...m, target, cx, base, nearest, wobble, over, held };
}

/** One damped spring advanced by `dt` seconds, semi-implicitly: stable at any
 *  stiffness a frame rate can throw at it. */
function spring(
  x: number,
  v: number,
  goal: number,
  omega: number,
  zeta: number,
  dt: number,
): [number, number] {
  const n = Math.max(1, Math.ceil(dt * SUBSTEPS_PER_SECOND));
  const s = dt / n;
  for (let i = 0; i < n; i += 1) {
    v += (-omega * omega * (x - goal) - 2 * zeta * omega * v) * s;
    x += v * s;
  }
  if (Math.abs(x - goal) < SETTLE && Math.abs(v) < SETTLE) return [goal, 0];
  return [x, v];
}

const stepOf = (dt: number) => Math.min(Math.max(dt, 0), MAX_STEP);

/** The motion `dt` seconds on, for a pointer still at (`x`, `y`). */
export function stepReach(
  m: ReachMotion,
  box: ReachBox,
  x: number,
  y: number,
  dt: number,
): ReachMotion {
  const t = stepOf(dt);
  const goal = m.target ?? { lift: 0, h: 0, tx: 0 };
  const [lw, lz] = m.wobble ? LIFT_WOBBLE : [REACH_EASE, 1];
  const [bw, bz] = m.wobble ? BUMP_WOBBLE : [REACH_EASE, 1];
  const [lift, liftV] = spring(m.lift, m.liftV, goal.lift, lw, lz, t);
  const [h, hV] = spring(m.h, m.hV, goal.h, bw, bz, t);
  // The lean rides the bump's spring: it swings round to a pointer that moved
  // sideways, and whips back upright with the let-go's wobble.
  const [tx, txV] = spring(m.tx, m.txV, goal.tx, bw, bz, t);
  const next = { ...m, lift, liftV, h, hV, tx, txV };
  const over = reachContains(box, shownReach(next, box), x, y);
  // Settling as the pointer sinks in must not uncover it: hold this frame, and
  // let the pointer's next move decide. Held is arrived — the zone stops where
  // it is rather than straining at a goal it will not take.
  if (m.over && !over) {
    const target = m.target && { ...m.target, lift: m.lift, h: m.h, tx: m.tx };
    return { ...m, target, liftV: 0, hV: 0, txV: 0 };
  }
  return { ...next, over, held: m.held || over };
}

/** Whether the zone has arrived, so the caller can stop stepping it. */
export function reachSettled(m: ReachMotion): boolean {
  const goal = m.target ?? { lift: 0, h: 0, tx: 0 };
  return (
    m.lift === goal.lift &&
    m.h === goal.h &&
    m.tx === goal.tx &&
    m.liftV === 0 &&
    m.hV === 0 &&
    m.txV === 0
  );
}

/**
 * The zone arriving at the start of a drag and going at its end: one spring,
 * `p`, from 0 (gone) to 1 (there), that the zone is scaled and faded by.
 *
 * Arriving overshoots, so the zone swells past its box and wobbles into it.
 * Going starts with a kick outward — a last swell — and then collapses into
 * nothing, fading as it does; it is gone the moment it first reaches 0, so the
 * spring's undershoot is never drawn.
 */
export interface Presence {
  p: number;
  v: number;
  leaving: boolean;
}

/** [angular frequency in rad/s, damping ratio] for arriving and for going. */
export const PRESENCE_ARRIVE: readonly [number, number] = [20, 0.42];
export const PRESENCE_LEAVE: readonly [number, number] = [18, 0.5];
/** The outward speed, in `p` per second, a leaving zone starts with. */
export const LEAVE_KICK = 8;
/** How much narrower than its box the zone is at `p` = 0: it grows mostly in
 *  height, so the swell stays inside the rail. */
const PRESENCE_NARROW = 0.3;

export function goneZone(): Presence {
  return { p: 0, v: 0, leaving: false };
}

/** The zone starting to arrive — from wherever a leave in progress had got to. */
export function arrive(z: Presence): Presence {
  return { ...z, leaving: false };
}

export function leave(z: Presence): Presence {
  return { ...z, v: z.v + LEAVE_KICK, leaving: true };
}

export function stepPresence(z: Presence, dt: number): Presence {
  const [w, zeta] = z.leaving ? PRESENCE_LEAVE : PRESENCE_ARRIVE;
  const [p, v] = spring(z.p, z.v, z.leaving ? 0 : 1, w, zeta, stepOf(dt));
  return z.leaving && p <= 0 ? { p: 0, v: 0, leaving: true } : { ...z, p, v };
}

/** Whether a leaving zone has finished going. */
export function presenceGone(z: Presence): boolean {
  return z.leaving && z.p === 0 && z.v === 0;
}

/** Whether the zone is still arriving or going. */
export function presenceMoving(z: Presence): boolean {
  return z.leaving ? !presenceGone(z) : z.p !== 1 || z.v !== 0;
}

/** How to draw the zone at `z`: scaled about its box's centre, and faded. */
export function presenceLook(z: Presence): { sx: number; sy: number; opacity: number } {
  const p = Math.max(z.p, 0);
  return {
    sx: 1 - PRESENCE_NARROW * (1 - p),
    sy: p,
    opacity: clamp01(p / 0.6),
  };
}
