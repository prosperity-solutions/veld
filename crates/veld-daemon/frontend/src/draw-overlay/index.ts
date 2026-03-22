// ---------------------------------------------------------------------------
// Veld Draw Overlay — Smart canvas annotation engine
// Lazy-loaded by the feedback overlay when draw mode is activated.
// Exposes window.__veld_draw with activate() and compositeOnto().
// ---------------------------------------------------------------------------

import type {
  Point,
  DrawTool,
  BBox,
  StrokeEntry,
  StrokeDraw,
  PinEntry,
  BlurEntry,
  SpotlightEntry,
  DrawActivateOptions,
} from "./types";
import { mkEl, mkBtn, PREFIX } from "../shared/dom";
import { dist, pathLength, computeBBox } from "./geometry";
import { buildSnapshotCanvas } from "./color";
import { recognizeShape } from "./shapes";
import { createPixelatedRegion } from "./blur";
import { compositeOnto } from "./composite";

const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform);
const ERASER_OP = "destination-out";
const BASE_WIDTH = 5;
const WIDTHS = [
  { id: "thin", size: 2, dotSize: 4 },
  { id: "medium", size: 5, dotSize: 8 },
  { id: "thick", size: 12, dotSize: 14 },
];
const COLORS = [
  { id: "red",   label: "Red",   style: "#ef4444" },
  { id: "green", label: "Green", style: "#C4F56A" },
  { id: "white", label: "White", style: "#ffffff" },
  { id: "black", label: "Black", style: "#000000" },
];

// SVG icons
const ICON_SHAPES =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/></svg>';
const ICON_UNDO =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="1 4 1 10 7 10"/><path d="M3.51 15a9 9 0 105.64-11.36L1 10"/></svg>';
const ICON_REDO =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 11-5.64-11.36L23 10"/></svg>';
const ICON_ERASER =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 20H7L3 16c-.8-.8-.8-2 0-2.8L14.8 1.4c.8-.8 2-.8 2.8 0l5 5c.8.8.8 2 0 2.8L11 20"/><path d="M6 12l6 6"/></svg>';
const ICON_CHECK =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>';
const ICON_SPOTLIGHT =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="5"/><path d="M12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42"/></svg>';
const ICON_BLUR =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/></svg>';
const ICON_NUMBER =
  '<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="2"/><text x="12" y="16.5" text-anchor="middle" fill="currentColor" font-size="14" font-weight="bold" font-family="sans-serif">1</text></svg>';

function activate(
  canvas: HTMLCanvasElement,
  opts?: DrawActivateOptions,
): () => void {
  const options = opts || {};
  const ctx = canvas.getContext("2d")!;
  const dpr = window.devicePixelRatio || 1;

  let displayWidth: number;
  let displayHeight: number;
  if (!options.inline) {
    const rect = canvas.getBoundingClientRect();
    displayWidth = rect.width;
    displayHeight = rect.height;
    canvas.width = Math.round(rect.width * dpr);
    canvas.height = Math.round(rect.height * dpr);
    ctx.scale(dpr, dpr);
  } else {
    displayWidth = canvas.width;
    displayHeight = canvas.height;
  }

  let snapCanvas: HTMLCanvasElement | null = null;
  if (options.pageSnapshot) {
    snapCanvas = buildSnapshotCanvas(
      options.pageSnapshot,
      options.inline ? canvas.width : Math.round(displayWidth),
      options.inline ? canvas.height : Math.round(displayHeight),
    );
  } else if (options.baseImage) {
    snapCanvas = buildSnapshotCanvas(options.baseImage);
  }

  let hasPressureDevice = false;
  let activeWidthIdx = 1;
  let activeColorIdx = 0; // red

  const strokes: StrokeEntry[] = [];
  let undoneStrokes: StrokeEntry[] = [];
  let currentStroke: StrokeDraw | null = null;
  let baseWidth = BASE_WIDTH;
  let toolMode: DrawTool = "draw";
  let shapeSnap = false; // shape recognition toggle
  let pinCounter = 0;
  let pendingPin: PinEntry | null = null; // placed but arrow not locked
  let pendingPinAngle = 0;
  let drawing = false;
  let rafPending = false;

  function redraw(): void {
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.restore();

    const spotlights: SpotlightEntry[] = [];
    const allStrokes: StrokeEntry[] = currentStroke
      ? (strokes as StrokeEntry[]).concat([currentStroke])
      : strokes;
    for (let i = 0; i < allStrokes.length; i++) {
      if ((allStrokes[i] as SpotlightEntry).type === "spotlight") {
        spotlights.push(allStrokes[i] as SpotlightEntry);
      } else {
        renderStroke(ctx, allStrokes[i]);
      }
    }
    if (spotlights.length > 0) renderSpotlights(ctx, spotlights);

    // Pin previews in number mode
    if (toolMode === "number") {
      if (pendingPin) {
        // Pending pin: semi-transparent, arrow follows cursor
        ctx.save();
        ctx.globalAlpha = 0.45;
        renderPin(ctx, { ...pendingPin, angle: pendingPinAngle });
        ctx.restore();
      } else if (cursorPos && !drawing) {
        // Ghost preview: show next pin number under cursor
        ctx.save();
        ctx.globalAlpha = 0.35;
        renderPin(ctx, {
          type: "pin",
          x: cursorPos.x, y: cursorPos.y,
          number: pinCounter + 1,
          color: COLORS[activeColorIdx].style,
          angle: 0,
        });
        ctx.restore();
      }
    }
  }

  function renderStroke(c: CanvasRenderingContext2D, s: StrokeEntry): void {
    if ((s as PinEntry).type === "pin") { renderPin(c, s as PinEntry); return; }
    if ((s as BlurEntry).type === "blur") { renderBlur(c, s as BlurEntry); return; }
    const stroke = s as StrokeDraw;
    if (stroke.shape) { renderShape(c, stroke); return; }
    if (!stroke.points || stroke.points.length < 2) return;

    c.save();
    c.lineCap = "round";
    c.lineJoin = "round";
    c.globalCompositeOperation = (stroke.compositeOp || "source-over") as GlobalCompositeOperation;

    if (stroke.hasPressure) {
      c.strokeStyle = stroke.color;
      for (let j = 1; j < stroke.points.length; j++) {
        const p0 = stroke.points[j - 1], p1 = stroke.points[j];
        const pressure = (p0.pressure + p1.pressure) / 2;
        c.lineWidth = stroke.baseWidth * (0.3 + pressure * 1.4);
        c.beginPath(); c.moveTo(p0.x, p0.y); c.lineTo(p1.x, p1.y); c.stroke();
      }
    } else {
      c.strokeStyle = stroke.color;
      c.lineWidth = stroke.baseWidth || stroke.width || BASE_WIDTH;
      c.beginPath();
      c.moveTo(stroke.points[0].x, stroke.points[0].y);
      for (let k = 1; k < stroke.points.length; k++) c.lineTo(stroke.points[k].x, stroke.points[k].y);
      c.stroke();
    }
    c.restore();
  }

  function renderShape(c: CanvasRenderingContext2D, s: StrokeDraw): void {
    c.save();
    c.strokeStyle = s.color;
    c.lineWidth = s.baseWidth || BASE_WIDTH;
    c.lineCap = "round"; c.lineJoin = "round";
    c.globalCompositeOperation = (s.compositeOp || "source-over") as GlobalCompositeOperation;
    const sh = s.shape!;
    if (sh.type === "line") {
      c.beginPath(); c.moveTo(sh.start.x, sh.start.y); c.lineTo(sh.end.x, sh.end.y); c.stroke();
    } else if (sh.type === "arrow") {
      c.beginPath(); c.moveTo(sh.start.x, sh.start.y); c.lineTo(sh.end.x, sh.end.y); c.stroke();
      const angle = Math.atan2(sh.end.y - sh.start.y, sh.end.x - sh.start.x);
      const headLen = Math.min(20, dist(sh.start, sh.end) * 0.3);
      c.beginPath();
      c.moveTo(sh.end.x, sh.end.y);
      c.lineTo(sh.end.x - headLen * Math.cos(angle - 0.45), sh.end.y - headLen * Math.sin(angle - 0.45));
      c.moveTo(sh.end.x, sh.end.y);
      c.lineTo(sh.end.x - headLen * Math.cos(angle + 0.45), sh.end.y - headLen * Math.sin(angle + 0.45));
      c.stroke();
    } else if (sh.type === "circle") {
      c.beginPath(); c.arc(sh.cx, sh.cy, sh.radius, 0, Math.PI * 2); c.stroke();
    } else if (sh.type === "rect") {
      c.strokeRect(sh.x, sh.y, sh.w, sh.h);
    }
    c.restore();
  }

  function renderSpotlights(c: CanvasRenderingContext2D, spotlights: SpotlightEntry[]): void {
    c.save();
    c.globalCompositeOperation = "source-over";
    c.fillStyle = "rgba(0, 0, 0, 0.65)";
    if (options.inline) { c.fillRect(0, 0, canvas.width, canvas.height); }
    else { c.save(); c.setTransform(1, 0, 0, 1, 0, 0); c.fillRect(0, 0, canvas.width, canvas.height); c.restore(); }
    c.globalCompositeOperation = "destination-out";
    c.fillStyle = "rgba(0, 0, 0, 1)";
    for (let i = 0; i < spotlights.length; i++) {
      const s = spotlights[i];
      c.beginPath();
      if (s.shape) {
        if (s.shape.type === "circle") c.arc(s.shape.cx, s.shape.cy, s.shape.radius, 0, Math.PI * 2);
        else if (s.shape.type === "rect") c.rect(s.shape.x, s.shape.y, s.shape.w, s.shape.h);
        else traceStrokePath(c, s.points);
      } else if (s.points && s.points.length > 0) traceStrokePath(c, s.points);
      c.fill();
    }
    c.restore();
  }

  function traceStrokePath(c: CanvasRenderingContext2D, points: Point[]): void {
    if (!points || points.length < 2) return;
    c.moveTo(points[0].x, points[0].y);
    for (let i = 1; i < points.length; i++) c.lineTo(points[i].x, points[i].y);
    c.closePath();
  }

  function renderPin(c: CanvasRenderingContext2D, s: PinEntry): void {
    const r = 16;
    const pointerLen = 8;
    const pointerWidth = 12;
    const numStr = String(s.number);
    const angle = s.angle || 0;
    c.save();
    c.globalCompositeOperation = "source-over";

    const lum = 0.299 * parseInt(s.color.slice(1, 3), 16) +
                0.587 * parseInt(s.color.slice(3, 5), 16) +
                0.114 * parseInt(s.color.slice(5, 7), 16);
    const textColor = lum > 128 ? "#000000" : "#ffffff";
    const outlineColor = lum > 128 ? "rgba(0,0,0,0.25)" : "rgba(255,255,255,0.35)";

    // Draw combined shape: circle + pointer arrow as one filled path (no shadow)
    // Pin path helper
    const gapAngle = Math.atan2(pointerWidth / 2, r);
    const tipX = s.x + Math.cos(angle) * (r + pointerLen);
    const tipY = s.y + Math.sin(angle) * (r + pointerLen);

    function tracePath(): void {
      c.beginPath();
      c.arc(s.x, s.y, r, angle + gapAngle, angle + Math.PI * 2 - gapAngle);
      c.lineTo(s.x + Math.cos(angle + gapAngle) * r, s.y + Math.sin(angle + gapAngle) * r);
      c.lineTo(tipX, tipY);
      c.lineTo(s.x + Math.cos(angle - gapAngle) * r, s.y + Math.sin(angle - gapAngle) * r);
      c.closePath();
    }

    // Shadow: offset dark copy behind
    c.fillStyle = "rgba(0,0,0,0.15)";
    c.save();
    c.translate(0, 2);
    tracePath();
    c.fill();
    c.restore();

    // Main shape
    c.fillStyle = s.color;
    tracePath();
    c.fill();

    // Thin outline
    c.strokeStyle = outlineColor;
    c.lineWidth = 1.5;
    c.stroke();

    // Number — big, bold, crisp
    c.font = "900 16px -apple-system, BlinkMacSystemFont, sans-serif";
    c.textAlign = "center";
    c.textBaseline = "middle";
    c.fillStyle = textColor;
    c.fillText(numStr, s.x, s.y);

    c.restore();
  }

  function renderBlur(c: CanvasRenderingContext2D, s: BlurEntry): void {
    if (!s.pixelCanvas) return;
    c.save(); c.globalCompositeOperation = "source-over";
    c.drawImage(s.pixelCanvas, s.bbox.x, s.bbox.y, s.bbox.w, s.bbox.h);
    c.restore();
  }

  function scheduleRedraw(): void {
    if (rafPending) return;
    rafPending = true;
    requestAnimationFrame(function () { rafPending = false; redraw(); });
  }

  // ---- Pointer events -------------------------------------------------------

  function getPos(e: PointerEvent): Point {
    const r = canvas.getBoundingClientRect();
    let pressure = e.pressure != null ? e.pressure : 0.5;
    if (pressure === 0 && e.pointerType !== "mouse") pressure = 0.5;
    if (options.inline) {
      const scaleX = canvas.width / r.width, scaleY = canvas.height / r.height;
      return { x: (e.clientX - r.left) * scaleX, y: (e.clientY - r.top) * scaleY, pressure };
    }
    return { x: e.clientX - r.left, y: e.clientY - r.top, pressure };
  }

  function onPointerDown(e: PointerEvent): void {
    if (e.button !== 0) return;
    if (e.target !== canvas) return; // don't capture clicks on toolbar elements
    drawing = true;
    canvas.setPointerCapture(e.pointerId);
    const pos = getPos(e);

    // Number mode: first click places pin, second click locks arrow direction
    if (toolMode === "number") {
      if (pendingPin) {
        // Second click — lock the angle and commit
        pendingPin.angle = pendingPinAngle;
        strokes.push(pendingPin);
        pendingPin = null;
        undoneStrokes = [];
        scheduleRedraw();
        updateToolbarState();
      } else {
        // First click — create pending pin
        pinCounter++;
        pendingPin = {
          type: "pin", x: pos.x, y: pos.y,
          number: pinCounter,
          color: COLORS[activeColorIdx].style,
          angle: 0,
        };
        pendingPinAngle = 0;
        scheduleRedraw();
      }
      drawing = false;
      return;
    }

    const isPressure = e.pointerType !== "mouse" && e.pressure > 0 && e.pressure < 1;
    if (isPressure && !hasPressureDevice) { hasPressureDevice = true; hideThicknessButtons(); }
    const color = toolMode === "eraser" ? "#000000" : COLORS[activeColorIdx].style;
    const strokeWidth = hasPressureDevice ? baseWidth : WIDTHS[activeWidthIdx].size;
    currentStroke = {
      points: [pos], color,
      baseWidth: toolMode === "eraser" ? strokeWidth * 3 : strokeWidth,
      compositeOp: toolMode === "eraser" ? ERASER_OP : "source-over",
      hasPressure: hasPressureDevice, toolMode,
    };
    scheduleRedraw();
  }

  // Track cursor for pin preview ghost
  let cursorPos: Point | null = null;

  function onPointerMove(e: PointerEvent): void {
    if (toolMode === "number") {
      cursorPos = getPos(e);
      // Track arrow angle for pending pin
      if (pendingPin) {
        pendingPinAngle = Math.atan2(cursorPos.y - pendingPin.y, cursorPos.x - pendingPin.x);
      }
      scheduleRedraw();
      return;
    }
    if (!drawing || !currentStroke) return;
    currentStroke.points.push(getPos(e));
    scheduleRedraw();
  }

  function onPointerUp(): void {
    if (!drawing) return;
    drawing = false;
    if (!currentStroke) return;
    const pts = currentStroke.points;
    const totalPathLen = pathLength(pts);

    // Number mode is handled in onPointerDown, shouldn't reach here
    if (toolMode === "number") { currentStroke = null; return; }
    if (pts.length === 1) pts.push({ x: pts[0].x + 0.5, y: pts[0].y + 0.5, pressure: pts[0].pressure });

    if (toolMode === "blur") {
      const bbox = computeBBox(pts);
      if (bbox.w > 5 && bbox.h > 5) {
        let snapBbox: BBox = bbox;
        if (!options.inline && snapCanvas) {
          const dW = canvas.width / dpr, dH = canvas.height / dpr;
          snapBbox = { x: bbox.x * (snapCanvas.width / dW), y: bbox.y * (snapCanvas.height / dH),
                       w: bbox.w * (snapCanvas.width / dW), h: bbox.h * (snapCanvas.height / dH) };
        }
        const pixelCanvas = createPixelatedRegion(snapCanvas, snapBbox);
        if (pixelCanvas) strokes.push({ type: "blur", bbox, pixelCanvas });
      }
      currentStroke = null; undoneStrokes = []; scheduleRedraw(); updateToolbarState(); return;
    }

    if (toolMode === "spotlight") {
      strokes.push({ type: "spotlight", points: pts, shape: shapeSnap ? recognizeShape(pts) : null });
      currentStroke = null; undoneStrokes = []; scheduleRedraw(); updateToolbarState(); return;
    }

    if (shapeSnap && toolMode === "draw" && totalPathLen > 20) {
      const shape = recognizeShape(pts);
      if (shape) currentStroke.shape = shape;
    }
    strokes.push(currentStroke);
    currentStroke = null; undoneStrokes = []; scheduleRedraw(); updateToolbarState();
  }

  canvas.addEventListener("pointerdown", onPointerDown);
  canvas.addEventListener("pointermove", onPointerMove);
  canvas.addEventListener("pointerup", onPointerUp);
  canvas.addEventListener("pointercancel", onPointerUp);
  const onCtx = (e: Event) => e.preventDefault();
  canvas.addEventListener("contextmenu", onCtx);
  const onWhl = (e: WheelEvent) => { e.preventDefault(); baseWidth = Math.max(1, Math.min(30, baseWidth + (e.deltaY > 0 ? -1 : 1))); };
  canvas.addEventListener("wheel", onWhl, { passive: false });

  // ---- Undo / Redo ----------------------------------------------------------

  function undo(): void {
    // Cancel pending pin placement if active
    if (pendingPin) {
      pinCounter = Math.max(0, pinCounter - 1);
      pendingPin = null;
      scheduleRedraw(); updateToolbarState();
      return;
    }
    if (!strokes.length) return;
    const removed = strokes.pop()!;
    undoneStrokes.push(removed);
    if ((removed as PinEntry).type === "pin") pinCounter = Math.max(0, pinCounter - 1);
    scheduleRedraw(); updateToolbarState();
  }

  function redo(): void {
    if (!undoneStrokes.length) return;
    const restored = undoneStrokes.pop()!;
    strokes.push(restored);
    if ((restored as PinEntry).type === "pin") pinCounter = (restored as PinEntry).number;
    scheduleRedraw(); updateToolbarState();
  }

  function onKeyDown(e: KeyboardEvent): void {
    if (drawing) return; // don't trigger shortcuts while dragging
    const mod = IS_MAC ? e.metaKey : e.ctrlKey;
    if (mod && e.key === "z" && !e.shiftKey) { e.preventDefault(); undo(); return; }
    if (mod && e.key === "z" && e.shiftKey) { e.preventDefault(); redo(); return; }
    if (mod || e.altKey) return;

    const k = e.key.toLowerCase();
    // Colors: 1-4 or first letter
    if (k >= "1" && k <= String(COLORS.length)) { activeColorIdx = parseInt(k) - 1; updateToolbarState(); return; }
    // Tool modes
    if (k === "d" || k === "p") { toolMode = "draw"; updateToolbarState(); return; } // draw/pen
    if (k === "e") { toolMode = toolMode === "eraser" ? "draw" : "eraser"; updateToolbarState(); return; }
    if (k === "s") { toolMode = toolMode === "spotlight" ? "draw" : "spotlight"; updateToolbarState(); return; }
    if (k === "x") { toolMode = toolMode === "blur" ? "draw" : "blur"; updateToolbarState(); return; }
    if (k === "n") { toolMode = toolMode === "number" ? "draw" : "number"; updateToolbarState(); return; }
    if (k === "o") { shapeSnap = !shapeSnap; updateToolbarState(); return; }
    // Thickness: [ and ]
    if (k === "[") { activeWidthIdx = Math.max(0, activeWidthIdx - 1); updateToolbarState(); return; }
    if (k === "]") { activeWidthIdx = Math.min(WIDTHS.length - 1, activeWidthIdx + 1); updateToolbarState(); return; }
    // Collapse: Tab
    if (e.key === "Tab") { e.preventDefault(); collapseBtn.click(); return; }
  }
  document.addEventListener("keydown", onKeyDown, true);

  // ---- Toolbar --------------------------------------------------------------

  // ---- Tooltip helper --------------------------------------------------------

  const tooltipEl = mkEl("div", "tooltip");
  const tooltipRoot = options.mountTarget || document.body;
  tooltipRoot.appendChild(tooltipEl);

  function showTip(anchor: HTMLElement, text: string): void {
    tooltipEl.textContent = text;
    tooltipEl.style.display = "block";
    const r = anchor.getBoundingClientRect();
    const tw = tooltipEl.offsetWidth;
    const th = tooltipEl.offsetHeight;
    let top = r.top + window.scrollY - th - 8;
    if (top < window.scrollY + 4) top = r.bottom + window.scrollY + 8;
    let left = r.left + window.scrollX + r.width / 2 - tw / 2;
    left = Math.max(4, Math.min(window.innerWidth - tw - 4, left));
    tooltipEl.style.top = top + "px";
    tooltipEl.style.left = left + "px";
  }

  function hideTip(): void { tooltipEl.style.display = "none"; }

  function tip(el: HTMLElement, text: string): void {
    el.addEventListener("mouseenter", () => showTip(el, text));
    el.addEventListener("mouseleave", hideTip);
    el.addEventListener("mousedown", hideTip);
  }

  // ---- Toolbar --------------------------------------------------------------

  const toolbar = mkEl("div", "draw-toolbar");
  let toolbarCollapsed = false;

  // Collapse/expand handle — small grip bar
  const collapseBtn = mkEl("button", "draw-collapse-btn") as HTMLButtonElement;
  collapseBtn.type = "button";
  collapseBtn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"><path d="M15 18l-6-6 6-6"/></svg>';
  tip(collapseBtn, "Collapse  Tab");
  toolbar.appendChild(collapseBtn);
  toolbar.appendChild(mkEl("span", "draw-sep"));

  // Collapsible tools container
  const toolsWrap = mkEl("div", "draw-tools-wrap");

  const colorBtns: HTMLElement[] = [];
  const widthBtns: HTMLElement[] = [];

  // Color buttons
  COLORS.forEach(function (c, i) {
    const btn = mkEl("button", "draw-color") as HTMLButtonElement;
    btn.type = "button";
    btn.style.background = c.style;
    if (c.id === "white") btn.style.borderColor = "#aaa";
    if (c.id === "black") btn.style.borderColor = "#555";
    btn.addEventListener("click", function () { activeColorIdx = i; updateToolbarState(); });
    tip(btn, c.label + "  " + (i + 1));
    colorBtns.push(btn);
    toolsWrap.appendChild(btn);
  });

  toolsWrap.appendChild(mkEl("span", "draw-sep"));

  WIDTHS.forEach(function (w, i) {
    const btn = mkEl("button", "draw-thick") as HTMLButtonElement;
    btn.title = w.id.charAt(0).toUpperCase() + w.id.slice(1); btn.type = "button";
    const dot = mkEl("span", "draw-thick-dot");
    dot.style.width = w.dotSize + "px"; dot.style.height = w.dotSize + "px";
    btn.appendChild(dot);
    btn.addEventListener("click", function () { activeWidthIdx = i; updateToolbarState(); });
    tip(btn, w.id.charAt(0).toUpperCase() + w.id.slice(1));
    widthBtns.push(btn); toolsWrap.appendChild(btn);
  });
  const thickSep = mkEl("span", "draw-sep");
  toolsWrap.appendChild(thickSep);

  function hideThicknessButtons(): void {
    widthBtns.forEach(btn => { btn.style.display = "none"; });
    if (thickSep) thickSep.style.display = "none";
  }

  // Shape snap toggle
  const shapeBtn = mkBtn("draw-tool-btn", ICON_SHAPES);
  shapeBtn.addEventListener("click", () => { shapeSnap = !shapeSnap; updateToolbarState(); });
  tip(shapeBtn, "Shape snap  O");
  toolsWrap.appendChild(shapeBtn);

  const spotlightBtn = mkBtn("draw-tool-btn", ICON_SPOTLIGHT);
  spotlightBtn.addEventListener("click", () => { toolMode = toolMode === "spotlight" ? "draw" : "spotlight"; updateToolbarState(); });
  tip(spotlightBtn, "Spotlight  S");
  toolsWrap.appendChild(spotlightBtn);

  const blurBtn = mkBtn("draw-tool-btn", ICON_BLUR);
  blurBtn.addEventListener("click", () => { toolMode = toolMode === "blur" ? "draw" : "blur"; updateToolbarState(); });
  tip(blurBtn, "Blur / Redact  X");
  toolsWrap.appendChild(blurBtn);

  const eraserBtn = mkBtn("draw-tool-btn", ICON_ERASER);
  eraserBtn.addEventListener("click", () => { toolMode = toolMode === "eraser" ? "draw" : "eraser"; updateToolbarState(); });
  tip(eraserBtn, "Eraser  E");
  toolsWrap.appendChild(eraserBtn);

  const numberBtn = mkBtn("draw-tool-btn", ICON_NUMBER);
  numberBtn.addEventListener("click", () => { toolMode = toolMode === "number" ? "draw" : "number"; updateToolbarState(); });
  tip(numberBtn, "Numbered pins  N");
  toolsWrap.appendChild(numberBtn);

  toolsWrap.appendChild(mkEl("span", "draw-sep"));

  const undoBtn = mkBtn("draw-tool-btn", ICON_UNDO);
  undoBtn.addEventListener("click", undo);
  tip(undoBtn, "Undo (" + (IS_MAC ? "\u2318" : "Ctrl") + "+Z)");
  toolsWrap.appendChild(undoBtn);

  const redoBtn = mkBtn("draw-tool-btn", ICON_REDO);
  redoBtn.addEventListener("click", redo);
  tip(redoBtn, "Redo (" + (IS_MAC ? "\u2318" : "Ctrl") + "+\u21E7+Z)");
  toolsWrap.appendChild(redoBtn);

  toolsWrap.appendChild(mkEl("span", "draw-sep"));

  // Insert tools wrap between collapse btn and done btn
  toolbar.appendChild(toolsWrap);

  const doneBtn = mkBtn("draw-done-btn", ICON_CHECK + " Done");
  doneBtn.addEventListener("click", () => { if (options.onDone) options.onDone(strokes.length > 0); });
  tip(doneBtn, "Finish drawing");
  toolbar.appendChild(doneBtn);

  // Collapse toggle
  collapseBtn.addEventListener("click", () => {
    toolbarCollapsed = !toolbarCollapsed;
    toolsWrap.style.display = toolbarCollapsed ? "none" : "";
    collapseBtn.classList.toggle(PREFIX + "draw-collapse-collapsed", toolbarCollapsed);
  });

  function updateToolbarState(): void {
    colorBtns.forEach((btn, i) => { btn.classList.toggle(PREFIX + "draw-color-active", i === activeColorIdx); });
    widthBtns.forEach((btn, i) => { btn.classList.toggle(PREFIX + "draw-thick-active", i === activeWidthIdx); });
    shapeBtn.classList.toggle(PREFIX + "draw-tool-btn-active", shapeSnap);
    spotlightBtn.classList.toggle(PREFIX + "draw-tool-btn-active", toolMode === "spotlight");
    blurBtn.classList.toggle(PREFIX + "draw-tool-btn-active", toolMode === "blur");
    eraserBtn.classList.toggle(PREFIX + "draw-tool-btn-active", toolMode === "eraser");
    numberBtn.classList.toggle(PREFIX + "draw-tool-btn-active", toolMode === "number");
    canvas.style.cursor = toolMode === "number" ? "pointer" : "crosshair";
    if (toolMode !== "number") { cursorPos = null; scheduleRedraw(); }
    (undoBtn as HTMLButtonElement).disabled = strokes.length === 0;
    (redoBtn as HTMLButtonElement).disabled = undoneStrokes.length === 0;
  }

  if (options.mountTarget) options.mountTarget.appendChild(toolbar);
  else document.body.appendChild(toolbar);
  updateToolbarState();

  return function cleanup(): void {
    canvas.removeEventListener("pointerdown", onPointerDown);
    canvas.removeEventListener("pointermove", onPointerMove);
    canvas.removeEventListener("pointerup", onPointerUp);
    canvas.removeEventListener("pointercancel", onPointerUp);
    canvas.removeEventListener("contextmenu", onCtx);
    canvas.removeEventListener("wheel", onWhl);
    document.removeEventListener("keydown", onKeyDown, true);
    if (toolbar.parentNode) toolbar.parentNode.removeChild(toolbar);
    if (tooltipEl.parentNode) tooltipEl.parentNode.removeChild(tooltipEl);
  };
}

window.__veld_draw = { activate, compositeOnto };
