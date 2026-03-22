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

  // --- Straight line / arrow ---
  // Arrow check is more lenient (ratio < 1.4) because the flick adds length
  const straightRatio = totalLen / (directDist || 1);

  if (directDist > 20 && straightRatio < 1.4) {
    // Check for arrowhead: last 15-20% of points deviate from main line direction
    const cutoff = Math.floor(points.length * 0.8);
    if (cutoff > 2 && points.length - cutoff >= 2) {
      const mainEnd = points[cutoff];
      const mainDist = dist(first, mainEnd);
      const mainRatio = pathLength(points.slice(0, cutoff + 1)) / (mainDist || 1);
      const headLen = pathLength(points.slice(cutoff));
      const headDirect = dist(mainEnd, last);
      // The main shaft should be relatively straight, and the head should deviate
      if (mainRatio < 1.15 && headLen > 5 && headDirect / (headLen || 1) < 0.85) {
        return { type: "arrow", start: first, end: mainEnd, headTip: last };
      }
    }
    // Pure straight line (stricter ratio)
    if (straightRatio < 1.15) {
      return { type: "line", start: first, end: last };
    }
  }

  // --- Closed shape detection (circle or rectangle) ---
  // Both require the path to be roughly closed
  const closedEnough = directDist < totalLen * 0.2;
  if (!closedEnough || points.length < 8) return null;

  // Compute centroid and bounding box
  let cx = 0, cy = 0;
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (let i = 0; i < points.length; i++) {
    cx += points[i].x;
    cy += points[i].y;
    if (points[i].x < minX) minX = points[i].x;
    if (points[i].y < minY) minY = points[i].y;
    if (points[i].x > maxX) maxX = points[i].x;
    if (points[i].y > maxY) maxY = points[i].y;
  }
  cx /= points.length;
  cy /= points.length;
  const bw = maxX - minX;
  const bh = maxY - minY;
  if (bw < 20 || bh < 20) return null;

  // Aspect ratio — used to distinguish circle from rectangle
  const aspectRatio = Math.max(bw, bh) / Math.min(bw, bh);

  // --- Circle ---
  // Check if points are roughly equidistant from centroid
  const dists: number[] = [];
  for (let j = 0; j < points.length; j++) {
    dists.push(dist(points[j], { x: cx, y: cy, pressure: 0 }));
  }
  const meanR = dists.reduce((a, b) => a + b, 0) / dists.length;

  if (meanR > 10) {
    let variance = 0;
    for (let k = 0; k < dists.length; k++) {
      const d = dists[k] - meanR;
      variance += d * d;
    }
    const stdDev = Math.sqrt(variance / dists.length);
    const cv = stdDev / meanR; // coefficient of variation

    // Circle: low CV AND roughly square aspect ratio
    if (cv < 0.2 && aspectRatio < 1.5) {
      return { type: "circle", cx, cy, radius: meanR };
    }
  }

  // --- Rectangle ---
  // Check if points cluster near the bounding box edges
  let edgeCount = 0;
  const edgeThresh = Math.min(bw, bh) * 0.2;
  for (let n = 0; n < points.length; n++) {
    const px = points[n].x;
    const py = points[n].y;
    const nearEdge =
      Math.abs(px - minX) < edgeThresh ||
      Math.abs(px - maxX) < edgeThresh ||
      Math.abs(py - minY) < edgeThresh ||
      Math.abs(py - maxY) < edgeThresh;
    if (nearEdge) edgeCount++;
  }
  // Rectangle: most points near edges, OR elongated aspect ratio with moderate edge proximity
  if (edgeCount / points.length > 0.6 || (aspectRatio > 1.5 && edgeCount / points.length > 0.4)) {
    return { type: "rect", x: minX, y: minY, w: bw, h: bh };
  }

  return null;
}
