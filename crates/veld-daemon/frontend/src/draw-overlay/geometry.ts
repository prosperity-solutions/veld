import type { Point, BBox } from "./types";

export function dist(a: Point, b: Point): number {
  const dx = a.x - b.x;
  const dy = a.y - b.y;
  return Math.sqrt(dx * dx + dy * dy);
}

export function pathLength(points: Point[]): number {
  let len = 0;
  for (let i = 1; i < points.length; i++) {
    len += dist(points[i - 1], points[i]);
  }
  return len;
}

export function computeBBox(points: Point[]): BBox {
  let minX = Infinity,
    minY = Infinity,
    maxX = -Infinity,
    maxY = -Infinity;
  for (let i = 0; i < points.length; i++) {
    if (points[i].x < minX) minX = points[i].x;
    if (points[i].y < minY) minY = points[i].y;
    if (points[i].x > maxX) maxX = points[i].x;
    if (points[i].y > maxY) maxY = points[i].y;
  }
  return { x: minX, y: minY, w: maxX - minX, h: maxY - minY };
}
