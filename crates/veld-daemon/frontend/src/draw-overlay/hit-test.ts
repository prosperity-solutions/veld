import type { StrokeEntry, StrokeDraw, PinEntry, BlurEntry, SpotlightEntry, Point } from "./types";
import { dist, computeBBox } from "./geometry";

const PIN_RADIUS = 16;

/**
 * Hit-test strokes at a given point. Returns the index of the
 * top-most (last) stroke that contains the point, or null.
 */
export function hitTest(
  strokes: readonly StrokeEntry[],
  point: Point,
  threshold: number,
): number | null {
  // Check in reverse order — top-most stroke wins
  for (let i = strokes.length - 1; i >= 0; i--) {
    if (hitTestEntry(strokes[i], point, threshold)) {
      return i;
    }
  }
  return null;
}

function hitTestEntry(entry: StrokeEntry, point: Point, threshold: number): boolean {
  if ((entry as PinEntry).type === "pin") {
    return hitTestPin(entry as PinEntry, point);
  }
  if ((entry as BlurEntry).type === "blur") {
    return hitTestBlur(entry as BlurEntry, point);
  }
  if ((entry as SpotlightEntry).type === "spotlight") {
    return hitTestSpotlight(entry as SpotlightEntry, point, threshold);
  }
  // StrokeDraw (freehand or recognized shape)
  return hitTestStroke(entry as StrokeDraw, point, threshold);
}

function hitTestPin(pin: PinEntry, point: Point): boolean {
  const dx = point.x - pin.x;
  const dy = point.y - pin.y;
  return Math.sqrt(dx * dx + dy * dy) <= PIN_RADIUS + 4; // small extra margin
}

function hitTestBlur(blur: BlurEntry, point: Point): boolean {
  const { x, y, w, h } = blur.bbox;
  return point.x >= x && point.x <= x + w && point.y >= y && point.y <= y + h;
}

function hitTestSpotlight(spotlight: SpotlightEntry, point: Point, threshold: number): boolean {
  if (!spotlight.points || spotlight.points.length < 2) return false;
  const bbox = computeBBox(spotlight.points);
  // Check if inside bounding box (with threshold)
  return (
    point.x >= bbox.x - threshold &&
    point.x <= bbox.x + bbox.w + threshold &&
    point.y >= bbox.y - threshold &&
    point.y <= bbox.y + bbox.h + threshold
  );
}

function hitTestStroke(stroke: StrokeDraw, point: Point, threshold: number): boolean {
  if (!stroke.points || stroke.points.length === 0) return false;

  // Single point — check distance
  if (stroke.points.length === 1) {
    return dist(point, stroke.points[0]) <= threshold;
  }

  // Check distance to each segment
  const halfWidth = (stroke.baseWidth || 5) / 2;
  const hitDist = threshold + halfWidth;

  for (let i = 1; i < stroke.points.length; i++) {
    if (distToSegment(point, stroke.points[i - 1], stroke.points[i]) <= hitDist) {
      return true;
    }
  }
  return false;
}

/** Distance from a point to a line segment. */
function distToSegment(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lenSq = dx * dx + dy * dy;

  if (lenSq === 0) return dist(p, a); // Degenerate segment

  // Project p onto the line, clamped to [0, 1]
  let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / lenSq;
  t = Math.max(0, Math.min(1, t));

  const projX = a.x + t * dx;
  const projY = a.y + t * dy;
  const ex = p.x - projX;
  const ey = p.y - projY;
  return Math.sqrt(ex * ex + ey * ey);
}
