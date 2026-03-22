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
  const straightRatio = totalLen / (directDist || 1);
  if (directDist > 20 && straightRatio < 1.2) {
    // Check for arrowhead: last 15% of points deviate from main line direction
    const cutoff = Math.floor(points.length * 0.85);
    if (cutoff > 2 && points.length - cutoff >= 2) {
      const mainEnd = points[cutoff];
      const headLen = pathLength(points.slice(cutoff));
      const headDirect = dist(mainEnd, last);
      if (headLen > 5 && headDirect / (headLen || 1) < 0.8) {
        return { type: "arrow", start: first, end: mainEnd, headTip: last };
      }
    }
    return { type: "line", start: first, end: last };
  }

  // --- Circle ---
  let cx = 0,
    cy = 0;
  for (let i = 0; i < points.length; i++) {
    cx += points[i].x;
    cy += points[i].y;
  }
  cx /= points.length;
  cy /= points.length;

  const dists: number[] = [];
  for (let j = 0; j < points.length; j++) {
    dists.push(dist(points[j], { x: cx, y: cy, pressure: 0 }));
  }
  const meanR =
    dists.reduce(function (a, b) {
      return a + b;
    }, 0) / dists.length;

  if (meanR > 10) {
    let variance = 0;
    for (let k = 0; k < dists.length; k++) {
      const d = dists[k] - meanR;
      variance += d * d;
    }
    const stdDev = Math.sqrt(variance / dists.length);
    const closedEnough = directDist < meanR * 0.5;

    if (stdDev / meanR < 0.25 && closedEnough) {
      return { type: "circle", cx, cy, radius: meanR };
    }
  }

  // --- Rectangle ---
  if (directDist < totalLen * 0.15 && points.length > 10) {
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (let m = 0; m < points.length; m++) {
      if (points[m].x < minX) minX = points[m].x;
      if (points[m].y < minY) minY = points[m].y;
      if (points[m].x > maxX) maxX = points[m].x;
      if (points[m].y > maxY) maxY = points[m].y;
    }
    const bw = maxX - minX;
    const bh = maxY - minY;
    if (bw > 20 && bh > 20) {
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
      if (edgeCount / points.length > 0.7) {
        return { type: "rect", x: minX, y: minY, w: bw, h: bh };
      }
    }
  }

  return null;
}
