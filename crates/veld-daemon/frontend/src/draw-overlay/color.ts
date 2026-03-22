/**
 * Build an offscreen canvas from a snapshot source for pixel sampling.
 */
export function buildSnapshotCanvas(
  source: ImageBitmap | HTMLCanvasElement | HTMLImageElement | null,
  width?: number,
  height?: number,
): HTMLCanvasElement | null {
  if (!source) return null;
  const sc = document.createElement("canvas");
  sc.width =
    width ||
    (source as HTMLCanvasElement).width ||
    (source as HTMLImageElement).naturalWidth ||
    300;
  sc.height =
    height ||
    (source as HTMLCanvasElement).height ||
    (source as HTMLImageElement).naturalHeight ||
    150;
  const sctx = sc.getContext("2d")!;
  try {
    sctx.drawImage(source as CanvasImageSource, 0, 0, sc.width, sc.height);
  } catch {
    return null; // tainted canvas, CORS, etc.
  }
  return sc;
}

/**
 * Sample average luminance at (x, y) from a snapshot canvas.
 * Returns 0-255 or -1 if no snapshot available.
 */
export function sampleLuminance(
  snapCanvas: HTMLCanvasElement | null,
  x: number,
  y: number,
): number {
  if (!snapCanvas) return -1;
  const sx = Math.max(0, Math.min(Math.round(x) - 2, snapCanvas.width - 5));
  const sy = Math.max(0, Math.min(Math.round(y) - 2, snapCanvas.height - 5));
  const sctx = snapCanvas.getContext("2d")!;
  let data: Uint8ClampedArray;
  try {
    data = sctx.getImageData(sx, sy, 5, 5).data;
  } catch {
    return -1;
  }
  let r = 0,
    g = 0,
    b = 0,
    count = 0;
  for (let i = 0; i < data.length; i += 4) {
    r += data[i];
    g += data[i + 1];
    b += data[i + 2];
    count++;
  }
  r /= count;
  g /= count;
  b /= count;
  return 0.299 * r + 0.587 * g + 0.114 * b;
}

/**
 * Pick a contrasting annotation color based on background luminance.
 * Red on light backgrounds, green on dark backgrounds.
 */
export function autoColor(luminance: number): string {
  if (luminance < 0) return "#ef4444"; // fallback: red
  return luminance > 128 ? "#ef4444" : "#C4F56A";
}
