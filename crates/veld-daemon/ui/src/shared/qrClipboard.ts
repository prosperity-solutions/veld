/**
 * Putting a share link on the clipboard as **text and a picture at once**.
 *
 * The point is pasting into a chat: Slack, Teams and iMessage all take an image
 * paste, and a QR someone can hold their phone up to is worth more in that context
 * than a URL nobody can type. A single clipboard entry can carry both flavours, so
 * the receiving app picks — Slack takes the image, a terminal takes the text — and
 * neither choice loses the other.
 *
 * What this cannot promise, stated because it is the obvious complaint: *which*
 * flavour an app uses is the app's decision, not ours. Slack pastes the image and
 * drops the text. If you need both visible in one message, paste twice — the text is
 * still on the clipboard.
 *
 * The PNG is drawn from the module matrix straight onto a canvas rather than by
 * rasterising the SVG through an `Image`, which would need a blob URL round-trip and
 * would taint the canvas in some engines.
 */

import { QR_QUIET_ZONE, VELD_MARK, encodeQr, veldMarkBox } from "./qr";

/** Pixels per module in the copied image. */
const SCALE = 8;

export type CopyResult = "both" | "text";

/**
 * A PNG of `value` as a QR code, or `null` when it does not fit a symbol.
 *
 * Black on white with the quiet zone included — a cropped QR is an unscannable one,
 * and a transparent background renders black-on-black in a dark-themed chat.
 */
async function qrPng(value: string): Promise<Blob | null> {
  const qr = encodeQr(value);
  if (!qr) return null;
  const side = (qr.size + 2 * QR_QUIET_ZONE) * SCALE;
  const canvas = document.createElement("canvas");
  canvas.width = side;
  canvas.height = side;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, side, side);
  ctx.fillStyle = "#000000";
  for (let y = 0; y < qr.size; y++) {
    for (let x = 0; x < qr.size; x++) {
      if (!qr.modules[y][x]) continue;
      // Merge horizontal runs, as the SVG path does: same pixels, far fewer fills.
      let run = 1;
      while (x + run < qr.size && qr.modules[y][x + run]) run++;
      ctx.fillRect(
        (x + QR_QUIET_ZONE) * SCALE,
        (y + QR_QUIET_ZONE) * SCALE,
        run * SCALE,
        SCALE,
      );
      x += run - 1;
    }
  }
  drawVeldMark(ctx, qr.size + 2 * QR_QUIET_ZONE);
  return await new Promise((resolve) =>
    canvas.toBlob((b) => resolve(b), "image/png"),
  );
}

/**
 * The same centre mark the on-screen SVG draws, in canvas coordinates.
 *
 * Shares [`VELD_MARK`] with the SVG rather than reimplementing the geometry: the
 * picture on the clipboard has to be the picture that was on screen, and "roughly the
 * same logo" would mean the two consume different amounts of the error-correction
 * budget — so one could scan while the other did not.
 *
 * `Path2D` takes SVG path data directly, which is what lets the logo's own two paths be
 * used verbatim instead of being retraced with canvas primitives.
 */
function drawVeldMark(ctx: CanvasRenderingContext2D, box: number): void {
  const { side, origin } = veldMarkBox(box);
  const scale = ((side - 2 * VELD_MARK.pad) / VELD_MARK.viewBox) * SCALE;
  const plate = side * SCALE;
  const at = origin * SCALE;
  const radius = plate * 0.22;
  ctx.fillStyle = "#ffffff";
  // `roundRect` is Chromium 99+/Safari 16+; the app's own floor is well past both, and
  // a square plate would still be a correct code, so this is not worth a fallback.
  ctx.beginPath();
  ctx.roundRect(at, at, plate, plate, radius);
  ctx.fill();

  ctx.save();
  ctx.translate(at + VELD_MARK.pad * SCALE, at + VELD_MARK.pad * SCALE);
  ctx.scale(scale, scale);
  ctx.fillStyle = "#000000";
  ctx.fill(new Path2D(VELD_MARK.glyph));
  ctx.fillStyle = VELD_MARK.dotFill;
  ctx.fill(new Path2D(VELD_MARK.dot));
  ctx.restore();
}

/**
 * Copy just the QR image.
 *
 * The counterpart to [`copyLinkWithQr`], for the case where the link is already in
 * the message and what is missing is the picture — pasting the pair into Slack gives
 * you one or the other, since the app picks the flavour, so "image only" is the way to
 * get the image *after* the text.
 *
 * Throws when the image cannot be produced or written, which the caller reports: there
 * is no silent fallback here, because falling back to text would put something other
 * than what the button says on the clipboard.
 */
export async function copyQrImage(link: string): Promise<void> {
  if (typeof ClipboardItem !== "function" || !navigator.clipboard?.write) {
    throw new Error("this browser cannot put an image on the clipboard");
  }
  const png = await qrPng(link);
  if (!png) throw new Error("this link is too long for a QR code");
  await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
}

/**
 * Copy `link` as text plus a QR image, falling back to text alone.
 *
 * The fallback is not defensive padding — it is the normal path in a plain browser
 * tab, where `ClipboardItem` may be missing (Firefox until recently) and where an
 * image write can be refused outright. Losing the picture must not lose the link,
 * which is the part someone actually needs, so the caller is told which happened and
 * says so rather than claiming a copy that did not include what it said it did.
 *
 * Throws only if even the text write fails, which the caller reports as an error.
 */
export async function copyLinkWithQr(link: string): Promise<CopyResult> {
  try {
    if (typeof ClipboardItem === "function" && navigator.clipboard?.write) {
      const png = await qrPng(link);
      if (png) {
        await navigator.clipboard.write([
          new ClipboardItem({
            "text/plain": new Blob([link], { type: "text/plain" }),
            "image/png": png,
          }),
        ]);
        return "both";
      }
    }
  } catch {
    // Fall through to text — see the doc comment.
  }
  await navigator.clipboard.writeText(link);
  return "text";
}
