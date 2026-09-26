/**
 * The trash reaching for a worktree that is being carried towards it.
 *
 * While a row is dragged, the trash's drop zone is one shape: the section's box,
 * plus a bump on its top edge under the pointer. Far away the shape is exactly
 * the box the overlay always was. Inside [`REACH_RADIUS`] two things happen,
 * faster the closer the pointer gets: the whole zone grows upward, and the bump
 * rises out of its top edge, until the tip meets the pointer — so the last
 * stretch of the gesture reads as the trash taking the worktree rather than the
 * worktree being aimed at a rectangle.
 *
 * The bump points rather than just rising. Its base follows the pointer only
 * part of the way along the edge ([`BASE_FOLLOW`]) and its tip leans the rest,
 * aiming along the line from the base to the pointer: a pointer out to one side
 * gets a bump bent over towards it, one straight above gets a straight one. The
 * lean skews the bump — the flank facing the pointer steepens, the other one
 * stretches — rather than bending it over, so the edge stays one height per x
 * and what [`reachContains`] tests is still exactly what is drawn.
 *
 * The tip leans as far as the box's side, into the corner. A flank never gets
 * steeper than [`MIN_FLANK`] allows, so one that would run past the side is cut
 * off by it instead: the side wall rises to meet it, and there is no strip of
 * plain edge left between the bump and the corner.
 *
 * The shape is the target, not a picture of one. Everything drawn is somewhere
 * a release trashes the worktree ([`reachContains`]), so the edge keeps
 * [`REACH_MARGIN`] from the pointer in both directions: short of a pointer it
 * has not taken yet, and above one it has. The first is why a worktree picked
 * up right next to the trash does not find itself already inside a zone that
 * leapt over it; the second is why the zone lets go of its bump as the pointer
 * sinks in without ever pulling the target out from under it.
 *
 * That makes the reach depend on where it has been, not only on where the
 * pointer is — the same pointer position is outside the zone for a worktree
 * just picked up there and inside it for one carried in from above. The caller
 * threads the previous [`Reach`] back in; everything else is still a function
 * of the numbers passed.
 *
 * Pure geometry, in whatever coordinate space the caller measures in, so the
 * feel can be tested without a layout engine.
 */

export interface ReachBox {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export interface Reach {
  /** How far the whole zone's top edge rises above `top`. */
  lift: number;
  /** Horizontal centre of the bump's base. */
  cx: number;
  /** How far the bump's tip leans from `cx` towards the pointer. 0 is upright. */
  tx: number;
  /** How far the bump rises above the lifted edge. 0 is a flat edge. */
  h: number;
}

/** How far from the trash, in px, a carried worktree starts to be noticed. */
export const REACH_RADIUS = 160;
/** The most the whole zone grows upward, in px. */
export const REACH_LIFT = 16;
/** The tallest the bump gets above the grown zone, in px. */
export const REACH_BUMP = 40;
/** How far, in px, the edge keeps from the pointer: short of it before the
 *  zone has it, above it once the zone does. */
export const REACH_MARGIN = 10;
/** Half the bump's base width, in px, before narrowing to fit the box. */
const REACH_HALF_WIDTH = 72;
/** How much of the pointer's offset from the middle of the edge the bump's base
 *  follows; the tip's lean makes up the rest. */
export const BASE_FOLLOW = 0.5;
/** The narrowest a flank gets, in px, however far the tip leans: a slope, not
 *  a wall. */
export const MIN_FLANK = 24;
/** The zone's corner radius — `.rail-group.drop-in` and the old `.trash-drop`
 *  border use the same 8px. */
export const REACH_CORNER = 8;
/** Straight segments the bump is drawn with: enough that the 1px outline reads
 *  as a curve at the widest base. */
const BUMP_STEPS = 32;

const clamp = (v: number, lo: number, hi: number) =>
  Math.min(Math.max(v, lo), hi);

/** 0 → 1 with zero slope at both ends, so the reach neither pops in at the edge
 *  of the radius nor jerks to a stop at its peak. */
const smoothstep = (t: number) => {
  const c = clamp(t, 0, 1);
  return c * c * (3 - 2 * c);
};

/** The bump's profile at `t` flank-widths from its tip: 1 at the peak, 0 at
 *  and past the base, and flat at both so it leaves the edge without a kink. */
const profile = (t: number) =>
  Math.abs(t) >= 1 ? 0 : (1 + Math.cos(Math.PI * t)) / 2;

/** The bump's height, as a share of its peak, at `x`: each flank is the
 *  profile stretched over its own side, from the base's end to the tip, so
 *  leaning moves the peak and keeps the base — until a flank is down to
 *  [`MIN_FLANK`], when it moves its end out instead, past the box's side if
 *  the tip is that close to it. */
function bumpAt(box: ReachBox, cx: number, tx: number, x: number): number {
  const w = halfWidth(box);
  if (w <= 0) return 0;
  const tip = cx + tx;
  const flank = Math.max(x < tip ? tip - (cx - w) : cx + w - tip, MIN_FLANK);
  return profile((x - tip) / flank);
}

/** The corner the bump has to stay clear of. The box's own height does not
 *  bound it, because the drawn corner belongs to the grown box, which is
 *  taller. */
const bumpCorner = (box: ReachBox) =>
  Math.max(0, Math.min(REACH_CORNER, (box.right - box.left) / 2));

/** Half-width of the bump's base for a box this wide: the default, unless the
 *  box is too narrow to fit it between its rounded corners. */
function halfWidth(box: ReachBox): number {
  return Math.max(
    0,
    Math.min(REACH_HALF_WIDTH, (box.right - box.left) / 2 - bumpCorner(box)),
  );
}

function cornerFor(box: ReachBox): number {
  return Math.max(
    0,
    Math.min(REACH_CORNER, (box.right - box.left) / 2, (box.bottom - box.top) / 2),
  );
}

/** The zone's full height above its box at `x`, for a bump on `cx` leaning
 *  by `tx`. */
function fullAt(box: ReachBox, cx: number, tx: number, x: number): number {
  return REACH_LIFT + REACH_BUMP * bumpAt(box, cx, tx, x);
}

/**
 * How far the zone reaches for a pointer at (`x`, `y`), given the reach it had
 * a move ago (`prev`, `null` for the first move of a drag), and how much of its
 * nearness it is allowed (`scale`, 1 unless it has just let go of this pointer
 * — see `reachMotion.ts`).
 *
 * Only upward. The trash is the bottom of the rail and spans its width, so a
 * worktree arrives from above; to the side of the section is outside the rail,
 * which clips anything drawn there.
 *
 * The bump's lean aims the tip along the line from its base to the pointer, as
 * if it were fully out: all the way under a pointer nearer than that, less the
 * farther above it is. It depends on where the pointer is, not on how far the
 * zone has reached, so the edge's height under the pointer is still one shape
 * times one scale.
 *
 * The reach is that scale, `k`, applied to the zone's growth and the bump alike,
 * and it is the least of three:
 * - **Nearness** — how close the pointer is, easing in from [`REACH_RADIUS`]
 *   to full a margin before the pointer comes within the zone's full height:
 *   exactly where the next rule stops it growing, so it arrives at full size
 *   instead of stalling just short of it.
 * - **Not past the pointer** — the tip may rise to within [`REACH_MARGIN`] of
 *   the pointer's height, and no further, *unless it was already there*: a
 *   reach keeps what it had, so a pointer that moves down into it enters it,
 *   while a zone that has not reached a pointer yet cannot jump over it. The
 *   tip, not the edge under the pointer: a tip leaning as far as it goes and
 *   still short of the pointer would otherwise stand taller than it, beside
 *   it, and a pointer moving sideways would pass through the bump and out of
 *   its top.
 * - **Not away from the pointer** — the edge stays at least [`REACH_MARGIN`]
 *   above it. This is the settling: once the pointer is that far in, the zone
 *   draws back with it, still around it, and is flat by the time the pointer is
 *   that far inside the box.
 */
export function trashReach(
  box: ReachBox,
  x: number,
  y: number,
  prev: Reach | null,
  scale = 1,
): Reach {
  const c = bumpCorner(box);
  const w = halfWidth(box);
  const mid = (box.left + box.right) / 2;
  const cx = clamp(mid + (x - mid) * BASE_FOLLOW, box.left + c + w, box.right - c - w);
  const full = REACH_LIFT + REACH_BUMP;
  const gap = box.top - y;
  const aim = full / Math.max(gap, full);
  const tx = clamp(cx + (x - cx) * aim, box.left, box.right) - cx;
  const dx = Math.max(box.left - x, 0, x - box.right);
  const near =
    scale *
    smoothstep(
      (REACH_RADIUS - Math.hypot(dx, Math.max(gap, 0))) /
        (REACH_RADIUS - full - REACH_MARGIN),
    );
  const under = fullAt(box, cx, tx, x);
  const had = prev ? prev.lift / REACH_LIFT : 0;
  const k = clamp(
    Math.min(
      near,
      Math.max(had, (gap - REACH_MARGIN) / full),
      (gap + REACH_MARGIN) / under,
    ),
    0,
    1,
  );
  return { lift: REACH_LIFT * k, cx, tx, h: REACH_BUMP * k };
}

/** The height of the zone's top edge at `x` — corners aside, which are too
 *  small to matter to a pointer. */
function edgeAt(box: ReachBox, reach: Reach, x: number): number {
  return box.top - reach.lift - reach.h * bumpAt(box, reach.cx, reach.tx, x);
}

/**
 * Whether (`x`, `y`) is inside the zone as [`reachPath`] draws it: the box, the
 * strip it grew by, and the bump. What the caller treats as over the trash, so
 * a release anywhere on the red lands.
 */
export function reachContains(
  box: ReachBox,
  reach: Reach,
  x: number,
  y: number,
): boolean {
  if (box.right <= box.left || box.bottom <= box.top) return false;
  if (x < box.left || x > box.right || y > box.bottom) return false;
  return y >= edgeAt(box, reach, x);
}

const n = (v: number) => Math.round(v * 100) / 100;

/** How far along a quarter circle's tangents its cubic Bézier's handles sit. */
const KAPPA = 0.5523;

/**
 * SVG path data for the zone: `box`, grown upward by the reach's `lift`, with
 * rounded corners and the bump on its top edge. One path shape whatever the
 * reach, so a resting zone and a full reach are the same outline, not two
 * drawings swapped at some threshold. The bump is traced from the same profile
 * [`reachContains`] tests against, so the outline is the target's edge.
 *
 * The top corners are rounded onto the edge wherever it meets the side — the
 * lifted edge at rest, partway up a flank when the tip has leaned into the
 * corner — so the side wall rises to meet the bump rather than stopping short
 * of it.
 */
export function reachPath(box: ReachBox, reach: Reach): string {
  const { left, right, bottom } = box;
  if (right <= left || bottom <= box.top) return "";
  const top = box.top - reach.lift;
  const r = cornerFor({ left, top, right, bottom });
  const w = halfWidth(box);
  const y = (x: number) => edgeAt(box, reach, x);
  // Each flank gets half the steps, so the steep side of a leaning bump is
  // traced as finely as the long one, and the peak is always a traced point.
  // A flank past a corner is left to the corner.
  const tip = reach.cx + reach.tx;
  const from = tip - Math.max(tip - (reach.cx - w), MIN_FLANK);
  const to = tip + Math.max(reach.cx + w - tip, MIN_FLANK);
  const half = BUMP_STEPS / 2;
  const bump: string[] = [];
  for (let i = 0; i <= BUMP_STEPS; i += 1) {
    const x = i <= half ? from + ((tip - from) * i) / half : tip + ((to - tip) * (i - half)) / half;
    if (x > left + r && x < right - r) bump.push(`L${n(x)},${n(y(x))}`);
  }
  const k = r * (1 - KAPPA);
  return [
    `M${n(left)},${n(y(left) + r)}`,
    `C${n(left)},${n(y(left) + k)} ${n(left + k)},${n(y(left + r))} ${n(left + r)},${n(y(left + r))}`,
    ...bump,
    `L${n(right - r)},${n(y(right - r))}`,
    `C${n(right - k)},${n(y(right - r))} ${n(right)},${n(y(right) + k)} ${n(right)},${n(y(right) + r)}`,
    `L${n(right)},${n(bottom - r)}`,
    `A${n(r)},${n(r)} 0 0 1 ${n(right - r)},${n(bottom)}`,
    `L${n(left + r)},${n(bottom)}`,
    `A${n(r)},${n(r)} 0 0 1 ${n(left)},${n(bottom - r)}`,
    "Z",
  ].join(" ");
}
