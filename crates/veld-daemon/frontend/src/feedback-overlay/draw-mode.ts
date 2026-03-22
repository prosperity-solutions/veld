import { refs } from "./refs";
import { store, dispatch } from "./store";
import type { UIMode } from "./types";
import { PREFIX } from "./constants";
import {
  stopCaptureStream,
  uploadAndShowEditor,
  restoreVeldUI,
} from "./screenshot";

// Late-bound to avoid circular import with modes.ts
let setModeFn: (mode: UIMode) => void;
export function setDrawModeDeps(deps: { setMode: (mode: UIMode) => void }): void {
  setModeFn = deps.setMode;
}

/** Load the draw.js script if not already loaded. */
export function ensureDrawScript(): Promise<void> {
  if (store.drawLoaded && window.__veld_draw) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = "/__veld__/feedback/draw.js";
    s.onload = (): void => {
      dispatch({ type: "SET_DRAW_LOADED", loaded: true });
      resolve();
    };
    s.onerror = reject;
    (document.head || document.documentElement).appendChild(s);
  });
}

/**
 * Set up the full-page draw canvas and activate the draw module.
 * Accepts a `setModeFn` callback to break circular dependency with modes.ts.
 */
export function setupGlobalDrawCanvas(): void {
  const canvas = document.createElement("canvas");
  canvas.className = PREFIX + "draw-canvas";
  document.body.appendChild(canvas);
  dispatch({ type: "SET_DRAW_CANVAS", canvas });

  // Disable page scroll while drawing
  dispatch({ type: "SET_PREV_OVERFLOW", overflow: document.body.style.overflow });
  document.body.style.overflow = "hidden";

  // Grab a snapshot for blur/redact, then activate.
  // The capture stream was acquired before this function is called.
  const track =
    store.captureStream && store.captureStream.getVideoTracks()[0];
  const ic =
    track && typeof ImageCapture !== "undefined"
      ? new ImageCapture(track)
      : null;
  const snapshotPromise: Promise<ImageBitmap | null> = ic
    ? (ic.grabFrame() as Promise<ImageBitmap>).catch(() => null)
    : Promise.resolve(null);

  snapshotPromise.then((snapshot) => {
    if (!store.drawCanvas) return; // torn down while waiting
    const cleanup = window.__veld_draw!.activate(store.drawCanvas, {
      pageSnapshot: snapshot,
      mountTarget: refs.shadow,
      onDone: (hasStrokes: boolean) => {
        if (hasStrokes) {
          const drawCanvas = store.drawCanvas!;

          // 1. Teardown draw toolbar (but keep canvas for compositing later)
          const drawCleanup = store.drawCleanup;
          dispatch({ type: "SET_DRAW_CLEANUP", cleanup: null });
          if (drawCleanup) drawCleanup();

          // 2. Remove draw canvas from DOM
          if (drawCanvas && drawCanvas.parentNode) {
            drawCanvas.parentNode.removeChild(drawCanvas);
          }
          dispatch({ type: "SET_DRAW_CANVAS", canvas: null });
          if (store.prevOverflow !== null) {
            document.body.style.overflow = store.prevOverflow;
            dispatch({ type: "SET_PREV_OVERFLOW", overflow: null });
          }
          dispatch({ type: "SET_MODE", mode: null });
          refs.toolBtnDraw.classList.remove(PREFIX + "tool-active");

          // 3. Hide ALL veld UI so the screenshot is clean
          const _sel =
            "[class^='" + PREFIX + "'], [class*=' " + PREFIX + "']";
          const veldEls = Array.from(
            document.querySelectorAll(_sel),
          ).concat(Array.from(refs.shadow.querySelectorAll(_sel)));
          // Also hide the host element itself
          refs.hostEl.style.visibility = "hidden";
          const hiddenEls: { el: HTMLElement; prev: string }[] = [];
          veldEls.forEach((el) => {
            if ((el as HTMLElement).style.display !== "none") {
              hiddenEls.push({
                el: el as HTMLElement,
                prev: (el as HTMLElement).style.visibility,
              });
              (el as HTMLElement).style.visibility = "hidden";
            }
          });

          // 4. Wait for repaint, grab clean frame, restore UI, composite
          const stream = store.captureStream;
          const captureTrack =
            stream && stream.getVideoTracks()[0];
          if (captureTrack && typeof ImageCapture !== "undefined") {
            requestAnimationFrame(() => {
              requestAnimationFrame(() => {
                setTimeout(() => {
                  const grabber = new ImageCapture(captureTrack);
                  grabber
                    .grabFrame()
                    .then((bitmap: ImageBitmap) => {
                      // Restore veld UI
                      restoreVeldUI(hiddenEls);
                      stopCaptureStream();
                      // Composite: page + annotations
                      const outCanvas = document.createElement("canvas");
                      outCanvas.width = bitmap.width;
                      outCanvas.height = bitmap.height;
                      const ctx = outCanvas.getContext("2d")!;
                      ctx.drawImage(bitmap, 0, 0);
                      ctx.drawImage(
                        drawCanvas,
                        0,
                        0,
                        outCanvas.width,
                        outCanvas.height,
                      );
                      bitmap.close();
                      outCanvas.toBlob((blob) => {
                        if (blob) {
                          uploadAndShowEditor(
                            blob,
                            0,
                            0,
                            window.innerWidth,
                            window.innerHeight,
                          );
                        }
                      }, "image/png");
                    })
                    .catch(() => {
                      restoreVeldUI(hiddenEls);
                      stopCaptureStream();
                      drawCanvas.toBlob((blob) => {
                        if (blob)
                          uploadAndShowEditor(
                            blob,
                            0,
                            0,
                            window.innerWidth,
                            window.innerHeight,
                          );
                      }, "image/png");
                    });
                }, 50);
              });
            });
          } else {
            restoreVeldUI(hiddenEls);
            stopCaptureStream();
            drawCanvas.toBlob((blob) => {
              if (blob)
                uploadAndShowEditor(
                  blob,
                  0,
                  0,
                  window.innerWidth,
                  window.innerHeight,
                );
            }, "image/png");
          }
        } else {
          setModeFn(null);
        }
      },
    });
    dispatch({ type: "SET_DRAW_CLEANUP", cleanup });
  }); // end snapshotPromise.then
}

/** Tear down the draw canvas and restore page scroll. */
export function teardownGlobalDrawCanvas(): void {
  if (store.drawCleanup) {
    store.drawCleanup();
    dispatch({ type: "SET_DRAW_CLEANUP", cleanup: null });
  }
  if (store.drawCanvas && store.drawCanvas.parentNode) {
    store.drawCanvas.parentNode.removeChild(store.drawCanvas);
  }
  dispatch({ type: "SET_DRAW_CANVAS", canvas: null });
  if (store.prevOverflow !== null) {
    document.body.style.overflow = store.prevOverflow;
    dispatch({ type: "SET_PREV_OVERFLOW", overflow: null });
  }
}
