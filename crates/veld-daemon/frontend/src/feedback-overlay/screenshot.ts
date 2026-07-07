import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { Thread, VeldPopoverElement } from "./types";
import { mkEl, submitOnModEnter } from "./helpers";
import { PREFIX, API, SUBMIT_HINT } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { closeActivePopover, positionPopover } from "./popover";
import { deps } from "../shared/registry";

// The single captured frame the user selects over. Held here (not in the store)
// because it is a transient ImageBitmap tied to one screenshot flow.
let frozenBitmap: ImageBitmap | null = null;

/**
 * Acquire a screen-capture stream. Chromium's `preferCurrentTab` /
 * `displaySurface` hints bias the picker toward the current tab.
 */
export function acquireCaptureStream(): Promise<void> {
  if (getState().captureStream) return Promise.resolve();

  const md = navigator.mediaDevices;
  if (!md || typeof md.getDisplayMedia !== "function") {
    return Promise.reject(new Error("screen capture unavailable"));
  }

  const opts: VeldDisplayMediaStreamOptions = {
    video: { displaySurface: "browser" },
    preferCurrentTab: true,
  };
  return md.getDisplayMedia(opts).then((stream) => {
    dispatch({ type: "SET_CAPTURE_STREAM", stream });
  });
}

/** Stop the active capture stream and release all tracks. */
export function stopCaptureStream(): void {
  const stream = getState().captureStream;
  if (stream) {
    stream.getTracks().forEach((t) => t.stop());
    dispatch({ type: "SET_CAPTURE_STREAM", stream: null });
  }
}

/**
 * Freeze-first capture. Called on screenshot-mode entry:
 *   1. acquire the display stream (permission picker)
 *   2. grab ONE frame, then stop the stream immediately (the share banner
 *      only flashes for that instant)
 *   3. paint the frozen frame as the selection surface
 *
 * The user then drags a rectangle over the frozen frame; the crop is taken
 * from the same pixels they selected, so the browser's share banner can't
 * shift the layout between selecting and capturing.
 */
export function beginScreenshotCapture(): void {
  // ImageCapture is Chromium-only. Firefox/Safari support getDisplayMedia but
  // not ImageCapture, so detect up front rather than surfacing a misleading
  // "capture denied" when the constructor throws.
  if (typeof ImageCapture === "undefined") {
    failCapture("Screen capture isn't supported in this browser");
    return;
  }
  acquireCaptureStream()
    .then(() => {
      const stream = getState().captureStream;
      if (!stream) {
        failCapture("Screen capture unavailable");
        return;
      }
      const grabber = new ImageCapture(stream.getVideoTracks()[0]);
      // A freshly-acquired capture track's first frame is often blank/black.
      // Let the compositor produce a couple of frames before grabbing.
      afterWarmup(() => {
        grabber
          .grabFrame()
          .then((bitmap: ImageBitmap) => {
            stopCaptureStream(); // banner disappears immediately
            if (getState().activeMode !== "screenshot") {
              bitmap.close(); // user bailed while the picker was up
              return;
            }
            if (frozenBitmap) frozenBitmap.close(); // guard rapid re-entry
            frozenBitmap = bitmap;
            showFrozenFrame(bitmap);
          })
          .catch(() => failCapture("Screen capture failed"));
      });
    })
    .catch(() => failCapture("Screen capture denied"));
}

/**
 * Give the freshly-acquired capture track a moment to produce a real (non-blank)
 * frame before grabbing. Uses setTimeout, not requestAnimationFrame — rAF is
 * paused in backgrounded tabs, which would leave the capture stream (and the OS
 * "sharing" indicator) live with no grab until the tab was refocused.
 */
function afterWarmup(fn: () => void): void {
  setTimeout(fn, 120);
}

/** Paint the frozen frame onto the backdrop and enable the selection cursor. */
function showFrozenFrame(bitmap: ImageBitmap): void {
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  canvas.getContext("2d")!.drawImage(bitmap, 0, 0);

  // background-size 100% 100% maps the frame 1:1 onto the fixed, viewport-sized
  // backdrop, so a rectangle drawn in viewport coordinates maps straight back
  // onto the bitmap.
  refs.overlay.style.backgroundImage = `url(${canvas.toDataURL("image/png")})`;
  refs.overlay.style.backgroundSize = "100% 100%";
  refs.overlay.classList.add(PREFIX + "overlay-active");
  refs.overlay.classList.add(PREFIX + "overlay-crosshair");
  toast("Drag to capture a region");
}

/** Reset the frozen-frame backdrop and release the bitmap. */
export function clearFrozenFrame(): void {
  refs.overlay.style.backgroundImage = "";
  refs.overlay.style.backgroundSize = "";
  if (frozenBitmap) {
    frozenBitmap.close();
    frozenBitmap = null;
  }
}

function failCapture(message: string): void {
  clearFrozenFrame();
  stopCaptureStream();
  if (getState().activeMode === "screenshot") deps().setMode(null);
  toast(message, true);
}

/**
 * Crop the frozen frame to the selected region and open the editor. Called from
 * the backdrop mouseup once the user finishes the selection rectangle.
 */
export function captureScreenshot(
  viewX: number,
  viewY: number,
  viewW: number,
  viewH: number,
): void {
  // Detach the bitmap before setMode(null) so teardown doesn't close it.
  const bitmap = frozenBitmap;
  frozenBitmap = null;
  deps().setMode(null); // teardown clears the backdrop + resets the cursor

  if (!bitmap) {
    showScreenshotThreadEditor(null, null);
    return;
  }
  cropAndShowEditor(bitmap, viewX, viewY, viewW, viewH);
}

/** Crop the captured bitmap to the selected region and show the editor. */
export function cropAndShowEditor(
  bitmap: ImageBitmap,
  viewX: number,
  viewY: number,
  viewW: number,
  viewH: number,
): void {
  // The bitmap is at native (dpr-scaled) resolution; the selection is in CSS px.
  const scaleX = bitmap.width / window.innerWidth;
  const scaleY = bitmap.height / window.innerHeight;

  const canvas = document.createElement("canvas");
  canvas.width = Math.round(viewW * scaleX);
  canvas.height = Math.round(viewH * scaleY);
  const ctx = canvas.getContext("2d")!;

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
      showScreenshotThreadEditor(null, null);
      return;
    }
    uploadAndShowEditor(pngBlob);
  }, "image/png");
}

/** Upload a screenshot blob to the API, then show the thread editor. */
export function uploadAndShowEditor(pngBlob: Blob): void {
  const screenshotId =
    "ss_" + Date.now() + "_" + Math.random().toString(36).slice(2, 8);

  fetch(API + "/screenshots/" + screenshotId, {
    method: "POST",
    headers: { "Content-Type": "image/png" },
    body: pngBlob,
  })
    .then((res) => {
      if (!res.ok) throw new Error("Upload failed: " + res.status);
      showScreenshotThreadEditor(pngBlob, screenshotId);
    })
    .catch((err) => {
      toast("Screenshot upload failed: " + err.message, true);
      // Still show the editor, just without the stored screenshot.
      showScreenshotThreadEditor(null, null);
    });
}

/** Show the screenshot thread editor popover with an optional preview. */
export function showScreenshotThreadEditor(
  pngBlob: Blob | null,
  screenshotId: string | null,
): void {
  closeActivePopover();

  const pop = mkEl("div", "popover popover-screenshot") as VeldPopoverElement;
  pop._veldType = "screenshot";

  let previewUrl: string | null = null;
  if (pngBlob) {
    previewUrl = URL.createObjectURL(pngBlob);
    const previewContainer = mkEl("div", "screenshot-preview");
    const previewImg = document.createElement("img");
    previewImg.src = previewUrl;
    previewImg.className = PREFIX + "screenshot-img";
    previewContainer.appendChild(previewImg);
    pop.appendChild(previewContainer);
  }

  // Revoke the object URL when the popover is closed by any means.
  pop._veldCleanup = (): void => {
    if (previewUrl) {
      URL.revokeObjectURL(previewUrl);
      previewUrl = null;
    }
  };

  const header = mkEl(
    "div",
    "popover-header",
    "Screenshot — " + window.location.pathname,
  );
  pop.appendChild(header);

  const body = mkEl("div", "popover-body");
  const ta = mkEl("textarea", "textarea") as HTMLTextAreaElement;
  ta.placeholder = "Describe what you see…";
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
    const payload = {
      scope: {
        type: "page",
        page_url: window.location.pathname,
      },
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

  // Highlight the screenshot toolbar button while the editor is open.
  refs.toolBtnScreenshot.classList.add(PREFIX + "tool-active");

  refs.shadow.appendChild(pop);
  dispatch({ type: "SET_POPOVER", popover: pop });

  // Position in the center of the viewport.
  const centerRect = {
    x: window.scrollX + window.innerWidth / 2 - 160,
    y: window.scrollY + window.innerHeight / 3,
    width: 320,
    height: 0,
  };
  positionPopover(pop, centerRect);
  ta.focus();
}
