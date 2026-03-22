import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { UIMode, Thread, VeldPopoverElement } from "./types";
import { mkEl, submitOnModEnter } from "./helpers";
import { PREFIX, ICONS, API, SUBMIT_HINT } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { closeActivePopover, positionPopover } from "./popover";
import { deps } from "../shared/registry";

/**
 * Acquire a screen capture stream, showing a disclaimer modal the first time.
 * Resolves when the stream is available in `getState().captureStream`.
 */
export function acquireCaptureStream(): Promise<void> {
  if (getState().captureStream) return Promise.resolve();

  return Promise.resolve().then(() => {
    // preferCurrentTab and displaySurface are Chromium-only extensions
    const opts: VeldDisplayMediaStreamOptions = {
      video: { displaySurface: "browser" },
      preferCurrentTab: true,
    };
    return navigator.mediaDevices.getDisplayMedia(opts).then((stream) => {
      dispatch({ type: "SET_CAPTURE_STREAM", stream });
      // If the user stops sharing via browser UI, clean up.
      stream.getVideoTracks()[0].addEventListener("ended", () => {
        dispatch({ type: "SET_CAPTURE_STREAM", stream: null });
        if (getState().activeMode === "screenshot") {
          // Late-bind to avoid circular import with modes.ts
          import("./modes").then((m) => m.setMode(null));
        }
      });
    });
  });
}

/** Stop the active capture stream and release all tracks. */
export function stopCaptureStream(): void {
  const stream = getState().captureStream;
  if (stream) {
    stream.getTracks().forEach((t) => {
      t.stop();
    });
    dispatch({ type: "SET_CAPTURE_STREAM", stream: null });
  }
}

/**
 * Capture a screenshot of the selected viewport region.
 */
export function captureScreenshot(
  viewX: number,
  viewY: number,
  viewW: number,
  viewH: number,
): void {
  // Hide veld UI so the screenshot is clean.
  const _sel =
    "[class^='" + PREFIX + "'], [class*=' " + PREFIX + "']";
  const veldEls = Array.from(document.querySelectorAll(_sel)).concat(
    Array.from(refs.shadow.querySelectorAll(_sel)),
  );
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

  // Exit screenshot mode (removes backdrop) but keep the stream alive.
  const stream = getState().captureStream;
  dispatch({ type: "SET_CAPTURE_STREAM", stream: null }); // prevent setMode(null) from stopping it
  deps().setMode(null);
  dispatch({ type: "SET_CAPTURE_STREAM", stream }); // restore for reuse

  if (!stream) {
    restoreVeldUI(hiddenEls);
    showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
    return;
  }

  const track = stream.getVideoTracks()[0];

  function grabCleanFrame(): void {
    const grabber = new ImageCapture(track);
    grabber
      .grabFrame()
      .then((bitmap: ImageBitmap) => {
        restoreVeldUI(hiddenEls);
        cropAndShowEditor(bitmap, viewX, viewY, viewW, viewH);
      })
      .catch(() => {
        restoreVeldUI(hiddenEls);
        showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
      });
  }

  // Wait for the UI to fully repaint before capturing: two rAF cycles
  // to flush styles + composite, plus a small timeout as safety margin
  // for slower compositors.
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      setTimeout(grabCleanFrame, 50);
    });
  });
}

/** Restore visibility of veld UI elements hidden for a clean screenshot. */
export function restoreVeldUI(
  hiddenEls: { el: HTMLElement; prev: string }[],
): void {
  hiddenEls.forEach((item) => {
    item.el.style.visibility = item.prev;
  });
  refs.hostEl.style.visibility = "";
}

/** Crop the captured bitmap to the selected region and show the editor. */
export function cropAndShowEditor(
  bitmap: ImageBitmap,
  viewX: number,
  viewY: number,
  viewW: number,
  viewH: number,
): void {
  // The captured bitmap may be at native resolution (dpr-scaled).
  const scaleX = bitmap.width / window.innerWidth;
  const scaleY = bitmap.height / window.innerHeight;

  const canvas = document.createElement("canvas");
  canvas.width = Math.round(viewW * scaleX);
  canvas.height = Math.round(viewH * scaleY);
  const ctx = canvas.getContext("2d")!;

  // Crop: draw the full bitmap offset so only the selected area is visible.
  ctx.drawImage(
    bitmap,
    Math.round(viewX * scaleX),
    Math.round(viewY * scaleY),
    canvas.width,
    canvas.height,
    0,
    0,
    canvas.width,
    canvas.height,
  );
  bitmap.close();

  canvas.toBlob((pngBlob) => {
    if (!pngBlob) {
      showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
      return;
    }
    uploadAndShowEditor(pngBlob, viewX, viewY, viewW, viewH);
  }, "image/png");
}

/** Upload a screenshot blob to the API, then show the thread editor. */
export function uploadAndShowEditor(
  pngBlob: Blob,
  viewX: number,
  viewY: number,
  viewW: number,
  viewH: number,
): void {
  const screenshotId =
    "ss_" + Date.now() + "_" + Math.random().toString(36).slice(2, 8);

  fetch(API + "/screenshots/" + screenshotId, {
    method: "POST",
    headers: { "Content-Type": "image/png" },
    body: pngBlob,
  })
    .then((res) => {
      if (!res.ok) throw new Error("Upload failed: " + res.status);
      showScreenshotThreadEditor(
        pngBlob,
        screenshotId,
        viewX,
        viewY,
        viewW,
        viewH,
      );
    })
    .catch((err) => {
      toast("Screenshot upload failed: " + err.message, true);
      // Still show the editor, just without the stored screenshot.
      showScreenshotThreadEditor(null, null, viewX, viewY, viewW, viewH);
    });
}

/** Show the screenshot thread editor popover with optional preview and annotation. */
export function showScreenshotThreadEditor(
  pngBlob: Blob | null,
  screenshotId: string | null,
  viewX: number,
  viewY: number,
  viewW: number,
  viewH: number,
): void {
  closeActivePopover();

  const pop = mkEl("div", "popover popover-screenshot") as VeldPopoverElement;
  pop._veldType = "screenshot";

  // Screenshot preview (if available)
  let previewUrl: string | null = null;
  let annotateDrawCleanup: (() => void) | null = null;
  if (pngBlob) {
    previewUrl = URL.createObjectURL(pngBlob);
    const previewContainer = mkEl("div", "screenshot-preview");
    const previewImg = document.createElement("img");
    previewImg.src = previewUrl;
    previewImg.className = PREFIX + "screenshot-img";
    previewContainer.appendChild(previewImg);

    // Annotate button — overlaid on the preview image
    const annotateBtn = document.createElement("button");
    annotateBtn.className = PREFIX + "annotate-btn";
    annotateBtn.innerHTML = ICONS.draw + " Annotate";
    annotateBtn.type = "button";
    annotateBtn.addEventListener("click", () => {
      deps().ensureDrawScript().then(() => {
            // Create canvas sized to image natural dimensions over the preview
            const drawCanvas = document.createElement("canvas");
            drawCanvas.className = PREFIX + "draw-canvas-inline";
            // Wait for image to load to get natural dimensions
            const setCanvasSize = (): void => {
              drawCanvas.width =
                previewImg.naturalWidth || previewImg.width;
              drawCanvas.height =
                previewImg.naturalHeight || previewImg.height;
            };
            if (previewImg.complete) {
              setCanvasSize();
            } else {
              previewImg.addEventListener("load", setCanvasSize, {
                once: true,
              });
              setCanvasSize(); // fallback
            }
            previewContainer.appendChild(drawCanvas);
            annotateBtn.style.display = "none";

            // Create a "Done" button to replace annotate
            const doneAnnotateBtn = document.createElement("button");
            doneAnnotateBtn.className = PREFIX + "annotate-btn";
            doneAnnotateBtn.innerHTML = ICONS.check + " Done";
            doneAnnotateBtn.type = "button";
            previewContainer.appendChild(doneAnnotateBtn);

            annotateDrawCleanup = window.__veld_draw!.activate(
              drawCanvas,
              {
                inline: true,
                baseImage: previewImg,
                mountTarget: previewContainer,
                onDone: finishAnnotation,
              },
            );

            function finishAnnotation(): void {
              window.__veld_draw!
                .compositeOnto(pngBlob!, drawCanvas)
                .then((newBlob: Blob) => {
                  // Update the preview with composited image
                  pngBlob = newBlob;
                  if (previewUrl) URL.revokeObjectURL(previewUrl);
                  previewUrl = URL.createObjectURL(newBlob);
                  previewImg.src = previewUrl;

                  // Re-upload the composited screenshot
                  if (screenshotId) {
                    fetch(API + "/screenshots/" + screenshotId, {
                      method: "POST",
                      headers: { "Content-Type": "image/png" },
                      body: newBlob,
                    }).catch(() => {
                      toast(
                        "Failed to upload annotated screenshot",
                        true,
                      );
                    });
                  }

                  // Cleanup draw state
                  if (annotateDrawCleanup) {
                    annotateDrawCleanup();
                    annotateDrawCleanup = null;
                  }
                  if (drawCanvas.parentNode)
                    drawCanvas.parentNode.removeChild(drawCanvas);
                  if (doneAnnotateBtn.parentNode)
                    doneAnnotateBtn.parentNode.removeChild(doneAnnotateBtn);
                  annotateBtn.style.display = "";
                })
                .catch((err: Error) => {
                  toast("Annotation failed: " + err.message, true);
                });
            }

            doneAnnotateBtn.addEventListener("click", finishAnnotation);
      })
      .catch(() => {
        toast("Failed to load draw module", true);
      });
    });
    previewContainer.appendChild(annotateBtn);

    pop.appendChild(previewContainer);
  }

  // Ensure the Object URL is revoked when the popover is closed by any means
  pop._veldCleanup = (): void => {
    if (annotateDrawCleanup) {
      annotateDrawCleanup();
      annotateDrawCleanup = null;
    }
    if (previewUrl) {
      URL.revokeObjectURL(previewUrl);
      previewUrl = null;
    }
  };

  const header = mkEl(
    "div",
    "popover-header",
    "Screenshot \u2014 " + window.location.pathname,
  );
  pop.appendChild(header);

  const body = mkEl("div", "popover-body");
  const ta = mkEl("textarea", "textarea") as HTMLTextAreaElement;
  (ta as HTMLTextAreaElement).placeholder = "Describe what you see\u2026";
  body.appendChild(ta);

  const actions = mkEl("div", "popover-actions");
  const cancelBtn = mkEl("button", "btn btn-secondary", "Cancel");
  cancelBtn.addEventListener("click", () => {
    closeActivePopover();
  });
  const sendBtn = mkEl(
    "button",
    "btn btn-primary",
    "Send" + SUBMIT_HINT,
  ) as HTMLButtonElement;
  sendBtn.addEventListener("click", () => {
    const text = ta.value.trim();
    if (!text) {
      ta.focus();
      return;
    }
    if (sendBtn.disabled) return;
    sendBtn.disabled = true;
    const scope = {
      type: "page",
      page_url: window.location.pathname,
    };
    const payload = {
      scope: scope,
      message: text,
      component_trace: null,
      screenshot: screenshotId || null,
      viewport_width: window.innerWidth,
      viewport_height: window.innerHeight,
    };
    api("POST", "/threads", payload)
      .then((raw) => {
        const thread = raw as Thread;
        dispatch({ type: "ADD_THREAD", thread });
        closeActivePopover();
        // addPin and updateBadge are called from the monolith via event flow
        toast("Thread created");
      })
      .catch((err: Error) => {
        sendBtn.disabled = false;
        toast("Failed to create thread: " + err.message, true);
      });
  });
  actions.appendChild(cancelBtn);
  actions.appendChild(sendBtn);
  submitOnModEnter(ta, sendBtn);
  body.appendChild(actions);
  pop.appendChild(body);

  // Highlight screenshot toolbar button while editor is open.
  refs.toolBtnScreenshot.classList.add(PREFIX + "tool-active");

  refs.shadow.appendChild(pop);
  dispatch({ type: "SET_POPOVER", popover: pop });

  // Position in center of viewport.
  const centerRect = {
    x: window.scrollX + window.innerWidth / 2 - 160,
    y: window.scrollY + window.innerHeight / 3,
    width: 320,
    height: 0,
  };
  positionPopover(pop, centerRect);
  ta.focus();
}
