import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import {
  acquireCaptureStream,
  stopCaptureStream,
  uploadAndShowEditor,
  restoreVeldUI,
} from "./screenshot";
import { deps } from "../shared/registry";

/** Load the draw.js script if not already loaded. */
export function ensureDrawScript(): Promise<void> {
  if (getState().drawLoaded && window.__veld_draw) return Promise.resolve();
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
 */
export function setupGlobalDrawCanvas(): void {
  const canvas = document.createElement("canvas");
  canvas.className = PREFIX + "draw-canvas";
  document.body.appendChild(canvas);
  dispatch({ type: "SET_DRAW_CANVAS", canvas });

  // Disable page scroll while drawing
  dispatch({ type: "SET_PREV_OVERFLOW", overflow: document.body.style.overflow });
  document.body.style.overflow = "hidden";

  // Activate immediately — no snapshot needed upfront.
  // Blur tool will acquire a snapshot lazily via acquireSnapshot callback.
  {
    const canvas = getState().drawCanvas;
    if (!canvas) return;

    // Lazy snapshot acquisition for blur tool
    const acquireSnapshot = async (): Promise<ImageBitmap | null> => {
      try {
        await acquireCaptureStream();
        const stream = getState().captureStream;
        const track = stream && stream.getVideoTracks()[0];
        if (!track || typeof ImageCapture === "undefined") return null;
        const ic = new ImageCapture(track);
        const bitmap = await ic.grabFrame();
        stopCaptureStream();
        return bitmap;
      } catch {
        return null;
      }
    };

    const cleanup = window.__veld_draw!.activate(canvas, {
      pageSnapshot: null,
      acquireSnapshot,
      mountTarget: refs.shadow,
      onDone: (hasStrokes: boolean) => {
        if (hasStrokes) {
          const drawCanvas = getState().drawCanvas!;

          // 1. Teardown draw toolbar (but keep canvas for compositing later)
          const drawCleanup = getState().drawCleanup;
          dispatch({ type: "SET_DRAW_CLEANUP", cleanup: null });
          if (drawCleanup) drawCleanup();

          // 2. Remove draw canvas from DOM
          if (drawCanvas && drawCanvas.parentNode) {
            drawCanvas.parentNode.removeChild(drawCanvas);
          }
          dispatch({ type: "SET_DRAW_CANVAS", canvas: null });
          const savedOverflow = getState().prevOverflow;
          if (savedOverflow !== null) {
            document.body.style.overflow = savedOverflow;
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

          // 4. Acquire capture stream (if not already present), grab clean frame, composite
          const doCapture = (): void => {
            const stream = getState().captureStream;
            const captureTrack = stream && stream.getVideoTracks()[0];
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
                  uploadAndShowEditor(blob, 0, 0, window.innerWidth, window.innerHeight);
              }, "image/png");
            }
          }; // end doCapture

          // Acquire stream if needed, then capture
          if (getState().captureStream) {
            doCapture();
          } else {
            acquireCaptureStream().then(() => {
              // Wait for repaint after dialog closes
              requestAnimationFrame(() => doCapture());
            }).catch(() => {
              // User denied — just send the drawing without page background
              restoreVeldUI(hiddenEls);
              drawCanvas.toBlob((blob) => {
                if (blob) uploadAndShowEditor(blob, 0, 0, window.innerWidth, window.innerHeight);
              }, "image/png");
            });
          }
        } else {
          // Inline teardown — same pattern as save path, without capture/composite.
          // The save path (above) does this inline and works. The previous code here
          // delegated to deps().setMode(null) which failed silently for unknown reasons.
          const drawCleanup = getState().drawCleanup;
          dispatch({ type: "SET_DRAW_CLEANUP", cleanup: null });
          if (drawCleanup) drawCleanup();

          const drawCanvas2 = getState().drawCanvas;
          if (drawCanvas2 && drawCanvas2.parentNode) {
            drawCanvas2.parentNode.removeChild(drawCanvas2);
          }
          dispatch({ type: "SET_DRAW_CANVAS", canvas: null });

          const savedOverflow2 = getState().prevOverflow;
          if (savedOverflow2 !== null) {
            document.body.style.overflow = savedOverflow2;
            dispatch({ type: "SET_PREV_OVERFLOW", overflow: null });
          }

          dispatch({ type: "SET_MODE", mode: null });
          refs.toolBtnDraw.classList.remove(PREFIX + "tool-active");
          stopCaptureStream();
        }
      },
    });
    dispatch({ type: "SET_DRAW_CLEANUP", cleanup });
  } // end activate block
}

/** Tear down the draw canvas and restore page scroll. */
export function teardownGlobalDrawCanvas(): void {
  const cleanup = getState().drawCleanup;
  if (cleanup) {
    try { cleanup(); } catch (err) { console.error("[veld] draw cleanup failed:", err); }
    dispatch({ type: "SET_DRAW_CLEANUP", cleanup: null });
  }
  const drawCanvas = getState().drawCanvas;
  if (drawCanvas && drawCanvas.parentNode) {
    drawCanvas.parentNode.removeChild(drawCanvas);
  }
  dispatch({ type: "SET_DRAW_CANVAS", canvas: null });
  const prevOverflow = getState().prevOverflow;
  if (prevOverflow !== null) {
    document.body.style.overflow = prevOverflow;
    dispatch({ type: "SET_PREV_OVERFLOW", overflow: null });
  }
}
