import type { Point, RecognizedShape } from "./types";
import { dist, pathLength } from "./geometry";

/**
 * Analyze a completed freehand stroke and attempt to recognize a clean shape.
 * Returns null if the stroke doesn't match any known shape pattern.
 */
export function recognizeShape(points: Point[]): RecognizedShape | null {
  if (points.length < 5) return null;

  const totalLen = pathLength(points);
  const first = points[0];
  const last = points[points.length - 1];
  const directDist = dist(first, last);
  if (totalLen < 20) return null;

  // --- Straight line / arrow ---
  const straightRatio = totalLen / (directDist || 1);

  if (directDist > 20 && straightRatio < 1.5) {
    // Try arrow detection: find the point where the stroke deviates from straightness
    // Walk backward from the end to find where the "flick" starts
    const arrowResult = detectArrow(points, first, last, totalLen);
    if (arrowResult) return arrowResult;

    // Pure straight line (stricter)
    if (straightRatio < 1.15) {
      return { type: "line", start: first, end: last };
    }
  }

  // --- Closed shape detection ---
  const closedEnough = directDist < totalLen * 0.25;
  if (!closedEnough || points.length < 8) return null;

  // Bounding box
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (let i = 0; i < points.length; i++) {
    if (points[i].x < minX) minX = points[i].x;
    if (points[i].y < minY) minY = points[i].y;
    if (points[i].x > maxX) maxX = points[i].x;
    if (points[i].y > maxY) maxY = points[i].y;
  }
  const bw = maxX - minX;
  const bh = maxY - minY;
  if (bw < 20 || bh < 20) return null;

  // Use corner detection to distinguish rectangle from circle.
  // Rectangles have ~4 sharp direction changes. Circles have ~0.
  const cornerCount = countCorners(points);
  const aspectRatio = Math.max(bw, bh) / Math.min(bw, bh);

  // --- Rectangle ---
  // 3-5 corners (4 expected, allow some tolerance) AND points follow the edges
  if (cornerCount >= 3 && cornerCount <= 6) {
    return { type: "rect", x: minX, y: minY, w: bw, h: bh };
  }

  // --- Circle ---
  // Few corners, roughly equidistant from center
  if (cornerCount <= 2) {
    const cx = (minX + maxX) / 2;
    const cy = (minY + maxY) / 2;
    const meanR = (bw + bh) / 4; // approximate radius
    // Verify points are actually circular (not just a random closed scribble)
    let maxDev = 0;
    for (let j = 0; j < points.length; j++) {
      const d = Math.abs(dist(points[j], { x: cx, y: cy, pressure: 0 }) - meanR);
      if (d > maxDev) maxDev = d;
    }
    // Max deviation should be small relative to radius
    if (maxDev < meanR * 0.4 && aspectRatio < 1.8) {
      return { type: "circle", cx, cy, radius: meanR };
    }
  }

  // --- Fallback: elongated closed shape → rectangle
  if (aspectRatio > 1.8) {
    return { type: "rect", x: minX, y: minY, w: bw, h: bh };
  }

  return null;
}

/**
 * Detect an arrowhead at the end of a mostly-straight stroke.
 * Walks backward from the end to find where the flick diverges.
 */
function detectArrow(
  points: Point[], first: Point, last: Point, totalLen: number,
): RecognizedShape | null {
  if (points.length < 10) return null;

  // Direction of the main stroke (first → last)
  const mainAngle = Math.atan2(last.y - first.y, last.x - first.x);

  // Walk backward from the end looking for where direction diverges
  let flickStart = points.length - 1;
  for (let i = points.length - 2; i > Math.floor(points.length * 0.6); i--) {
    const segAngle = Math.atan2(
      points[i + 1].y - points[i].y,
      points[i + 1].x - points[i].x,
    );
    let angleDiff = Math.abs(segAngle - mainAngle);
    if (angleDiff > Math.PI) angleDiff = 2 * Math.PI - angleDiff;
    if (angleDiff > 0.5) { // ~30 degrees divergence
      flickStart = i;
      break;
    }
  }

  // Need at least 3 flick points and the flick can't be too long
  const flickLen = points.length - flickStart;
  if (flickLen < 3 || flickLen > points.length * 0.35) return null;

  // The shaft (before flick) should be fairly straight
  const shaftPoints = points.slice(0, flickStart + 1);
  const shaftLen = pathLength(shaftPoints);
  const shaftDirect = dist(shaftPoints[0], shaftPoints[shaftPoints.length - 1]);
  const shaftRatio = shaftLen / (shaftDirect || 1);
  if (shaftRatio > 1.2) return null; // shaft too curvy

  const end = points[flickStart];
  return { type: "arrow", start: first, end, headTip: last };
}

/**
 * Count sharp direction changes (corners) in a path.
 * Used to distinguish rectangles (~4 corners) from circles (~0 corners).
 */
function countCorners(points: Point[]): number {
  if (points.length < 5) return 0;

  // Sample every few points to smooth out noise
  const step = Math.max(1, Math.floor(points.length / 40));
  const angles: number[] = [];

  for (let i = step; i < points.length - step; i += step) {
    const prev = points[Math.max(0, i - step)];
    const curr = points[i];
    const next = points[Math.min(points.length - 1, i + step)];

    const a1 = Math.atan2(curr.y - prev.y, curr.x - prev.x);
    const a2 = Math.atan2(next.y - curr.y, next.x - curr.x);
    let diff = Math.abs(a2 - a1);
    if (diff > Math.PI) diff = 2 * Math.PI - diff;
    angles.push(diff);
  }

  // A corner is a point where the angle change is significant (> ~45 degrees)
  let corners = 0;
  const threshold = Math.PI / 4; // 45 degrees
  for (let i = 0; i < angles.length; i++) {
    if (angles[i] > threshold) {
      corners++;
      // Skip nearby angles to avoid double-counting one corner
      while (i + 1 < angles.length && angles[i + 1] > threshold * 0.5) i++;
    }
  }

  return corners;
}
