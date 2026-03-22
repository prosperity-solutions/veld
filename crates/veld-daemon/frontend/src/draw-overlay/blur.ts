/**
 * Create a pixelated version of a region from a snapshot canvas.
 * Used for blur/redact tool.
 */
export function createPixelatedRegion(
  snapCanvas: HTMLCanvasElement | null,
  bbox: { x: number; y: number; w: number; h: number },
): HTMLCanvasElement | null {
  if (!snapCanvas) return null;
  const sx = Math.max(0, Math.round(bbox.x));
  const sy = Math.max(0, Math.round(bbox.y));
  const sw = Math.min(Math.round(bbox.w), snapCanvas.width - sx);
  const sh = Math.min(Math.round(bbox.h), snapCanvas.height - sy);
  if (sw <= 0 || sh <= 0) return null;

  const scale = 10;
  const smallW = Math.max(1, Math.round(sw / scale));
  const smallH = Math.max(1, Math.round(sh / scale));

  // Draw small
  const small = document.createElement("canvas");
  small.width = smallW;
  small.height = smallH;
  const sctx = small.getContext("2d")!;
  sctx.drawImage(snapCanvas, sx, sy, sw, sh, 0, 0, smallW, smallH);

  // Scale back up with no smoothing — creates blocky pixel effect
  const out = document.createElement("canvas");
  out.width = sw;
  out.height = sh;
  const octx = out.getContext("2d")!;
  octx.imageSmoothingEnabled = false;
  octx.drawImage(small, 0, 0, sw, sh);

  return out;
}
