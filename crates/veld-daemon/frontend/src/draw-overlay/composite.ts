/**
 * Flatten draw canvas annotations onto a base PNG image.
 * Returns a Promise<Blob> with the composited PNG.
 */
export function compositeOnto(
  baseBlob: Blob,
  drawCanvas: HTMLCanvasElement,
): Promise<Blob> {
  return new Promise(function (resolve, reject) {
    const img = new Image();
    img.onload = function () {
      const out = document.createElement("canvas");
      out.width = img.naturalWidth;
      out.height = img.naturalHeight;
      const c = out.getContext("2d")!;
      c.drawImage(img, 0, 0);
      c.drawImage(drawCanvas, 0, 0, out.width, out.height);
      URL.revokeObjectURL(img.src);
      out.toBlob(function (blob) {
        if (blob) resolve(blob);
        else reject(new Error("Failed to encode composited PNG"));
      }, "image/png");
    };
    img.onerror = function () {
      URL.revokeObjectURL(img.src);
      reject(new Error("Failed to load base image for compositing"));
    };
    img.src = URL.createObjectURL(baseBlob);
  });
}
