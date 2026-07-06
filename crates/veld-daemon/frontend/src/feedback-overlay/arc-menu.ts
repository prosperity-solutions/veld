// ---------------------------------------------------------------------------
// Arc Menu — floating radial toolbar with metaball "goo" physics.
//
// A draggable bubble that expands into a partial radial arc of tool items.
// Items render as metaball blobs (SVG goo filter) so they melt out of / back
// into the bubble organically. Supports macOS-dock-style fisheye magnification,
// momentum physics with edge bounce, per-edge viewport collision, goo-bridged
// tooltips, and data-driven submenu transitions.
//
// Layering (all inside `container`, positioned relative to the bubble center):
//   ┌ goo-layer  (filter: url(#goo)) — solid circles only; the filter merges
//   │            neighbouring circles into liquid necks. Holds the goo bubble,
//   │            per-item goo blobs, and goo tooltip pills. No text/icons here.
//   ┌ glow-layer — per-item accent glows (click flash) + the bubble glow.
//   └ icon-layer — crisp, unfiltered: the tool-button icon overlays, tooltip
//                  text, the solid bubble cover (masks goo), and the bubble hit
//                  target. Icons sit BELOW the cover so items visually emerge
//                  from behind the bubble.
//
// This engine is deliberately framework-agnostic and owns no application state:
// the host passes in an item model (each item carries its own persistent icon
// element), and the engine calls back on select / move / open-change. The Veld
// feedback overlay reuses its existing tool-button elements as icon overlays so
// active-state classes, badges, and tooltips keep working unchanged.
// ---------------------------------------------------------------------------

const SVG_NS = "http://www.w3.org/2000/svg";

/** A single menu entry. `el` is a persistent, caller-owned icon overlay. */
export interface ArcItem {
  id: string;
  /** Persistent icon element positioned/scaled by the engine each frame. */
  el: HTMLElement;
  /** Human label shown in the goo tooltip. */
  label: string;
  /** Optional keyboard hint chips, e.g. ["⌘", "⇧", "F"]. */
  kbd?: string[];
  /** Child menu — turns this item into a submenu opener. */
  sub?: ArcItem[];
  /** Leaf action. Ignored when `sub` is present. */
  onSelect?: () => void;
  /** Keep the menu open after selecting (in-place toggles: theme, shortcuts). */
  stayOpen?: boolean;
  /** Live active-state read — tints the item's goo blob with the accent color. */
  isActive?: () => boolean;
  /** Live visibility read — hidden items are omitted from the arc. */
  isVisible?: () => boolean;
}

/** All physics / geometry knobs. Every value is tunable. */
export interface ArcConfig {
  BUBBLE_SZ: number; // base bubble diameter (px)
  ARC_R: number; // bubble-center → item-center distance
  ITEM_SZ: number; // base item diameter
  FISH_RANGE: number; // angular range (rad) of fisheye influence
  FISH_MAG: number; // max fisheye magnification (1 = none)
  OPEN_MS: number; // open animation duration
  CLOSE_MS: number; // close animation duration (retraction is lerp-driven)
  DRAG_TH: number; // px before a press becomes a drag
  SPAN_PI: number; // radians of arc span contributed per item
  MIN_SPAN: number; // minimum total arc span (rad)
  MAX_SPAN: number; // maximum total arc span (rad)
  TT_GAP: number; // gap from item edge to tooltip pill
  TT_PAD: number; // internal horizontal padding of the tooltip pill
  TT_LERP: number; // tooltip scale animation speed
  LERP: number; // main position/scale lerp speed
  SUB_LERP: number; // retraction lerp during open/close/submenu transitions
  FRICTION: number; // velocity decay per frame (0.9 heavy … 0.99 icy)
  BOUNCE: number; // wall restitution (0 dead … 1 perfect)
  MIN_VEL: number; // speed below which momentum stops
  SNAP_VEL: number; // release speed needed to enter float mode
  VP_PAD: number; // viewport edge padding
  BADGE_SZ: number; // badge diameter (kept for host badge sizing parity)
  gooBlur: number; // metaball blur radius (feGaussianBlur stdDeviation)
  gooAlpha: number; // metaball alpha multiplier (feColorMatrix)
  gooOffset: number; // metaball alpha offset (feColorMatrix)
}

export const DEFAULT_ARC_CONFIG: ArcConfig = {
  BUBBLE_SZ: 40,
  ARC_R: 64,
  ITEM_SZ: 32,
  FISH_RANGE: 0.4,
  FISH_MAG: 1.65,
  OPEN_MS: 600,
  CLOSE_MS: 280,
  DRAG_TH: 4,
  SPAN_PI: 0.48,
  MIN_SPAN: Math.PI * 0.4,
  MAX_SPAN: Math.PI * 1.72,
  TT_GAP: 2,
  TT_PAD: 8,
  TT_LERP: 0.15,
  LERP: 0.1,
  SUB_LERP: 0.1,
  FRICTION: 0.95,
  BOUNCE: 0.6,
  MIN_VEL: 15,
  SNAP_VEL: 60,
  VP_PAD: 4,
  BADGE_SZ: 14,
  gooBlur: 6,
  gooAlpha: 44,
  gooOffset: -14,
};

export interface ArcCallbacks {
  /** Bubble moved. `committed` is true on drag/float end (persist then). */
  onMove?: (x: number, y: number, committed: boolean) => void;
  /** Menu opened or closed. */
  onOpenChange?: (open: boolean) => void;
  /** Drag state changed (suppress tooltips, etc.). */
  onDragChange?: (dragging: boolean) => void;
  /** Should an outside click auto-close the menu? Defaults to always true. */
  shouldCloseOnOutsideClick?: () => boolean;
}

export interface ArcMenuOptions {
  /** Fixed, zero-size container translated to the bubble center. */
  container: HTMLElement;
  /** Tree scope hosting the goo <filter> (the shadow root). */
  scope: ShadowRoot;
  /** Persistent bubble hit target (drag + click). */
  bubble: HTMLElement;
  /** Icon wrapper inside the bubble (swapped between logo / close / back). */
  bubbleIcon: HTMLElement;
  /** Root menu provider — re-invoked on reflow so visibility stays live. */
  items: () => ArcItem[];
  /** Bubble icon SVGs for each state. */
  icons: { logo: string; close: string; back: string };
  /** Optional goo tooltip shown on bubble hover while the menu is closed. */
  bubbleTooltip?: { label: string; kbd?: string[] };
  /** CSS class prefix (e.g. "veld-feedback-"). */
  prefix: string;
  config?: Partial<ArcConfig>;
  callbacks?: ArcCallbacks;
}

export interface ArcMenuHandle {
  open(): void;
  close(): void;
  toggle(): void;
  back(): void;
  isOpen(): boolean;
  depth(): number;
  /** Re-evaluate visible items and relayout if open. */
  reflow(): void;
  setPosition(x: number, y: number, animate?: boolean): void;
  getPosition(): { x: number; y: number };
  /** Nudge inward so the current state fits the viewport. */
  moveIntoView(animate?: boolean): void;
  /** Clamp to the closed-state viewport bounds and persist. */
  clampToViewport(): void;
  destroy(): void;
  /** Expose config for host-side sizing parity (read-only intent). */
  readonly config: ArcConfig;
}

// ── math ────────────────────────────────────────────────────────────────
const lerp = (a: number, b: number, t: number): number => a + (b - a) * t;
const clamp = (v: number, lo: number, hi: number): number =>
  Math.max(lo, Math.min(hi, v));
const smoothstep = (t: number): number => t * t * (3 - 2 * t);
const easeOutCubic = (t: number): number => 1 - Math.pow(1 - t, 3);
const easeOutBack = (t: number): number => {
  const c = 1.7;
  const c3 = c + 1;
  return 1 + c3 * Math.pow(t - 1, 3) + c * Math.pow(t - 1, 2);
};
/** Signed shortest angular difference from a to b, in (-π, π]. */
const angDiff = (a: number, b: number): number =>
  (((b - a) % (Math.PI * 2)) + Math.PI * 3) % (Math.PI * 2) - Math.PI;

interface ItemViz {
  sc: number; // current scale
  ag: number; // current angle (rad)
  r: number; // current radius from bubble center
}

export function createArcMenu(opts: ArcMenuOptions): ArcMenuHandle {
  const C: ArcConfig = { ...DEFAULT_ARC_CONFIG, ...(opts.config || {}) };
  const cb = opts.callbacks || {};
  const P = opts.prefix;
  const cls = (name: string) => P + name;

  // ── layers ──────────────────────────────────────────────────────────
  const container = opts.container;
  const gooLayer = document.createElement("div");
  gooLayer.className = cls("goo-layer");
  const glowLayer = document.createElement("div");
  glowLayer.className = cls("glow-layer");
  const iconLayer = document.createElement("div");
  iconLayer.className = cls("icon-layer");

  const gooBubble = document.createElement("div");
  gooBubble.className = cls("goo-blob") + " " + cls("goo-bubble");
  gooLayer.appendChild(gooBubble);

  const bubbleGlow = document.createElement("div");
  bubbleGlow.className = cls("bubble-glow");
  const bubbleCover = document.createElement("div");
  bubbleCover.className = cls("bubble-cover");

  // The caller-owned bubble hit target + icon live in the crisp icon layer,
  // above the cover.
  iconLayer.appendChild(bubbleGlow);
  iconLayer.appendChild(bubbleCover);
  iconLayer.appendChild(opts.bubble);

  container.appendChild(gooLayer);
  container.appendChild(glowLayer);
  container.appendChild(iconLayer);

  ensureGooFilter(opts.scope, P, C);

  // ── bubble tooltip (persistent goo pill, shown on hover while closed) ──
  let bubbleTT: HTMLElement | null = null;
  let bubbleTTText: HTMLElement | null = null;
  if (opts.bubbleTooltip) {
    const t = document.createElement("div");
    t.className = cls("tt-text");
    let html = `<span class="${cls("tt-label")}">${escapeHtml(opts.bubbleTooltip.label)}</span>`;
    const kbd = opts.bubbleTooltip.kbd;
    if (kbd && kbd.length) {
      html += `<span class="${cls("tt-kbd")}">` +
        kbd.map((k) => `<kbd>${escapeHtml(k)}</kbd>`).join("") + "</span>";
    }
    t.innerHTML = html;
    iconLayer.insertBefore(t, bubbleGlow);
    bubbleTTText = t;

    const pill = document.createElement("div");
    pill.className = cls("goo-tooltip");
    const w = t.offsetWidth + Math.max(C.TT_PAD, 12) * 2;
    pill.style.width = w + "px";
    pill.dataset.w = String(w);
    gooLayer.appendChild(pill);
    bubbleTT = pill;
  }

  // ── state ───────────────────────────────────────────────────────────
  const S = {
    x: 0,
    y: 0,
    vx: 0,
    vy: 0,
    dragging: false,
    wasDrag: false,
    pointerDown: false,
    dragOff: { x: 0, y: 0 },
    dragStart: { x: 0, y: 0 },
    open: false,
    openT: 0,
    opening: false,
    closing: false,
    retracting: false,
    animStart: 0,
    floating: false,
    hoveringBubble: false,
    bubbleScale: 1,
    bubbleTgtScale: 1,
    bubbleTTScale: 0,
    mouseAng: null as number | null,
    mouseInZone: false,
    // current level
    menu: [] as ArcItem[],
    stack: [] as ArcItem[][],
    // pending level during a retraction transition
    pending: null as ArcItem[] | null,
    pendingStack: null as ArcItem[][] | null,
    // per-item visualization + ephemeral decoration
    cur: [] as ItemViz[],
    tgt: [] as ItemViz[],
    ttScales: [] as number[],
    closeClear: true,
  };

  let gooBlobs: HTMLElement[] = [];
  let gooTTs: HTMLElement[] = [];
  let ttTexts: HTMLElement[] = [];
  let glows: HTMLElement[] = [];
  let hovIdx = -1;

  const wiredItems = new WeakSet<ArcItem>();

  // ── arc geometry ──────────────────────────────────────────────────────
  // The arc always faces the viewport center, so dragging the bubble live-
  // reorients the arc.
  function arcCfg(n: number) {
    const cx = window.innerWidth / 2;
    const cy = window.innerHeight / 2;
    const center = Math.atan2(cy - S.y, cx - S.x);
    const span = clamp(n * C.SPAN_PI, C.MIN_SPAN, C.MAX_SPAN);
    return { center, start: center - span / 2, span };
  }
  function baseAngles(start: number, span: number, n: number): number[] {
    const out: number[] = [];
    for (let i = 0; i < n; i++) out.push(start + ((i + 0.5) / n) * span);
    return out;
  }

  // ── fisheye ────────────────────────────────────────────────────────────
  // Items near the cursor inflate; the arc span is redistributed
  // proportionally so total width stays constant (macOS dock on a curve).
  function fisheye(
    base: number[],
    mouseAng: number | null,
    start: number,
    span: number,
  ): { sc: number[]; ag: number[] } {
    const n = base.length;
    if (mouseAng === null || !S.mouseInZone || !n) {
      return { sc: base.map(() => 1), ag: [...base] };
    }
    const sc = base.map((a) => {
      const d = Math.abs(angDiff(a, mouseAng));
      return 1 + (C.FISH_MAG - 1) * smoothstep(clamp(1 - d / C.FISH_RANGE, 0, 1));
    });
    const total = sc.reduce((a, b) => a + b, 0);
    const unit = span / total;
    let c = start;
    const ag = sc.map((s) => {
      const mid = c + (s * unit) / 2;
      c += s * unit;
      return mid;
    });
    return { sc, ag };
  }

  // ── viewport bounds (per-edge collision) ────────────────────────────────
  // Extents are computed from actual item positions, so the bubble can hug an
  // edge the arc doesn't face. Tooltips + fisheye are intentionally excluded.
  function bounds() {
    const pad = C.VP_PAD;
    const br = (C.BUBBLE_SZ / 2) * (S.bubbleScale || 1);
    let extL = br;
    let extR = br;
    let extT = br;
    let extB = br;
    if (S.open && !S.closing && S.cur.length > 0) {
      for (let i = 0; i < S.cur.length; i++) {
        const c = S.cur[i];
        if (!c || c.r < 1) continue;
        const ir = (C.ITEM_SZ * (c.sc || 1)) / 2;
        const ix = Math.cos(c.ag) * c.r;
        const iy = Math.sin(c.ag) * c.r;
        extL = Math.max(extL, -ix + ir);
        extR = Math.max(extR, ix + ir);
        extT = Math.max(extT, -iy + ir);
        extB = Math.max(extB, iy + ir);
      }
    }
    return {
      minX: extL + pad,
      maxX: window.innerWidth - extR - pad,
      minY: extT + pad,
      maxY: window.innerHeight - extB - pad,
    };
  }

  // ── build / destroy items ───────────────────────────────────────────────
  function visible(menu: ArcItem[]): ArcItem[] {
    return menu.filter((it) => (it.isVisible ? it.isVisible() : true));
  }

  function buildItems(menu: ArcItem[]): void {
    destroyItems();
    S.menu = menu;
    const n = menu.length;
    const arc = arcCfg(n);
    const ba = baseAngles(arc.start, arc.span, n);
    // Comfortable horizontal padding inside the tooltip pill.
    const ttPad = Math.max(C.TT_PAD, 12);

    menu.forEach((item, i) => {
      wireItem(item);

      // Goo blob — solid circle in the filtered layer.
      const blob = document.createElement("div");
      blob.className = cls("goo-blob");
      blob.style.width = C.ITEM_SZ + "px";
      blob.style.height = C.ITEM_SZ + "px";
      gooLayer.appendChild(blob);
      gooBlobs.push(blob);

      // Accent glow — click flash, unfiltered.
      const glow = document.createElement("div");
      glow.className = cls("accent-glow");
      glowLayer.appendChild(glow);
      glows.push(glow);

      // Icon overlay — the caller's persistent tool button, inserted BELOW the
      // bubble cover so items emerge from behind the bubble.
      item.el.classList.add(cls("tool-btn"));
      iconLayer.insertBefore(item.el, bubbleGlow);

      // Tooltip text (crisp). Built first so we can size the pill from its
      // real rendered width — kbd chips are wider than a plain string, so
      // measuring the actual element keeps text off the pill edges.
      const tt = document.createElement("div");
      tt.className = cls("tt-text");
      let html = `<span class="${cls("tt-label")}">${escapeHtml(item.label)}</span>`;
      if (item.sub) html += `<span class="${cls("tt-arrow")}">›</span>`;
      if (item.kbd && item.kbd.length) {
        html += `<span class="${cls("tt-kbd")}">`;
        html += item.kbd.map((k) => `<kbd>${escapeHtml(k)}</kbd>`).join("");
        html += "</span>";
      }
      tt.innerHTML = html;
      iconLayer.insertBefore(tt, bubbleGlow);
      ttTexts.push(tt);

      // Goo tooltip pill (filtered) — merges with the blob at small scale.
      const ttW = tt.offsetWidth + ttPad * 2;
      const ttBlob = document.createElement("div");
      ttBlob.className = cls("goo-tooltip");
      ttBlob.style.width = ttW + "px";
      ttBlob.dataset.w = String(ttW);
      gooLayer.appendChild(ttBlob);
      gooTTs.push(ttBlob);
    });

    S.cur = menu.map((_, i) => ({ sc: 0.01, ag: ba[i], r: 0 }));
    S.tgt = menu.map((_, i) => ({ sc: 1, ag: ba[i], r: C.ARC_R }));
    S.ttScales = menu.map(() => 0);
  }

  function destroyItems(): void {
    gooBlobs.forEach((e) => e.remove());
    gooTTs.forEach((e) => e.remove());
    ttTexts.forEach((e) => e.remove());
    glows.forEach((e) => e.remove());
    // Detach (but keep) the caller-owned icon overlays.
    S.menu.forEach((item) => {
      item.el.classList.remove(cls("tool-btn"));
      if (item.el.parentNode) item.el.parentNode.removeChild(item.el);
      item.el.style.transform = "";
      item.el.style.opacity = "";
    });
    gooBlobs = [];
    gooTTs = [];
    ttTexts = [];
    glows = [];
    S.cur = [];
    S.tgt = [];
    S.ttScales = [];
    hovIdx = -1;
  }

  // ── per-frame positioning ────────────────────────────────────────────────
  function posItems(progress: number): void {
    const n = S.menu.length;
    for (let i = 0; i < n; i++) {
      const blob = gooBlobs[i];
      const item = S.menu[i];
      const glow = glows[i];
      const ttBlob = gooTTs[i];
      const tt = ttTexts[i];
      const c = S.cur[i];
      if (!blob || !item || !c) continue;

      const itemT = clamp((progress * (n + 2) - i) / 2, 0, 1);
      const appear = easeOutBack(clamp(itemT, 0, 1));

      const r = c.r * appear;
      const x = Math.cos(c.ag) * r;
      const y = Math.sin(c.ag) * r;
      const sz = C.ITEM_SZ * c.sc;

      // Goo blob — always fully opaque and surface-colored so neighbouring
      // blobs merge with no colour discontinuity at the necks.
      blob.style.width = sz + "px";
      blob.style.height = sz + "px";
      blob.style.transform = `translate(calc(${x}px - 50%),calc(${y}px - 50%))`;

      // Icon overlay — distance-based fade in, fisheye scale.
      const active = item.isActive ? item.isActive() : false;
      const separation = r / (C.ARC_R || 1);
      const iconFade = clamp((separation - 0.5) * 3, 0, 1);
      item.el.style.transform = `translate(calc(${x}px - 50%),calc(${y}px - 50%)) scale(${c.sc})`;
      item.el.style.opacity = String(iconFade);
      item.el.style.pointerEvents = iconFade > 0.6 ? "auto" : "none";

      // Active state reads as a soft accent halo behind the icon (no hard
      // colour edge on the blob), fading in/out via CSS.
      if (glow) {
        glow.style.transform = `translate(calc(${x}px - 50%),calc(${y}px - 50%))`;
        glow.style.opacity = active ? String(iconFade * 0.95) : "0";
      }

      // Tooltip — horizontal pill scaling out from its attachment edge,
      // clamped to the viewport.
      if (ttBlob && tt) {
        const ttW = parseFloat(ttBlob.dataset.w || "60");
        const ttH = 26;
        const outR = C.ARC_R + (C.ITEM_SZ * c.sc) / 2 + C.TT_GAP;
        const ttX = Math.cos(c.ag) * outR * appear;
        const ttY = Math.sin(c.ag) * outR * appear;
        const ts = S.ttScales[i] || 0;
        const ox = (1 - Math.cos(c.ag)) / 2;
        const oy = (1 - Math.sin(c.ag)) / 2;
        let pL = ttX - ox * ttW;
        let pT = ttY - oy * ttH;

        const vpPad = 6;
        const vpL = S.x + pL;
        const vpT = S.y + pT;
        const vpR = vpL + ttW;
        const vpB = vpT + ttH;
        if (vpL < vpPad) pL += vpPad - vpL;
        if (vpR > window.innerWidth - vpPad) pL -= vpR - (window.innerWidth - vpPad);
        if (vpT < vpPad) pT += vpPad - vpT;
        if (vpB > window.innerHeight - vpPad) pT -= vpB - (window.innerHeight - vpPad);

        ttBlob.style.width = ttW + "px";
        ttBlob.style.height = ttH + "px";
        ttBlob.style.left = pL + "px";
        ttBlob.style.top = pT + "px";
        ttBlob.style.transformOrigin = `${ox * 100}% ${oy * 100}%`;
        ttBlob.style.transform = `scale(${ts})`;
        ttBlob.style.opacity = ts > 0.01 ? "1" : "0";

        tt.style.left = pL + ttW * 0.5 + "px";
        tt.style.top = pT + ttH * 0.5 + "px";
        tt.style.opacity = ts > 0.5 ? String(clamp((ts - 0.5) * 2, 0, 1)) : "0";
      }
    }
  }

  function flashBubbleGlow(): void {
    bubbleGlow.style.transition = "none";
    bubbleGlow.style.opacity = "1";
    requestAnimationFrame(() => {
      bubbleGlow.style.transition = "opacity .6s ease-out";
      bubbleGlow.style.opacity = "0";
    });
  }

  // ── bubble icon ─────────────────────────────────────────────────────────
  let iconTimer: ReturnType<typeof setTimeout> | null = null;
  let currentIconKey = "";
  function updateBubbleIcon(): void {
    const key = S.stack.length > 0 ? "back" : S.open ? "close" : "logo";
    if (key === currentIconKey) return;
    currentIconKey = key;
    const html =
      key === "back" ? opts.icons.back : key === "close" ? opts.icons.close : opts.icons.logo;
    if (iconTimer) clearTimeout(iconTimer);
    opts.bubbleIcon.style.opacity = "0";
    iconTimer = setTimeout(() => {
      opts.bubbleIcon.innerHTML = html;
      opts.bubbleIcon.style.opacity = "1";
    }, 110);
  }

  // ── open / close / submenu ────────────────────────────────────────────────
  function openMenu(): void {
    if (closeTimer) { clearTimeout(closeTimer); closeTimer = null; }
    if (S.open && !S.closing) return;
    S.open = true;
    S.opening = true;
    S.closing = false;
    S.openT = 0;
    S.animStart = performance.now();
    flashBubbleGlow();
    buildItems(visible(opts.items()));
    updateBubbleIcon();
    cb.onOpenChange?.(true);
    ensureLoop();
  }

  function closeMenu(clear = true): void {
    if (closeTimer) { clearTimeout(closeTimer); closeTimer = null; }
    // Re-entry guard: ignore if already closing (avoids double onOpenChange
    // and restarted timing).
    if (!S.open || S.closing) return;
    S.closing = true;
    S.opening = false;
    S.retracting = false;
    S.closeClear = clear;
    S.animStart = performance.now();
    hideAllTooltips();
    updateBubbleIcon();
    cb.onOpenChange?.(false);
    ensureLoop();
  }

  // Submenu transitions are data-driven: stash the pending level, lerp items
  // back to the bubble center, and only rebuild once the goo has absorbed them
  // (maxR < 4). No setTimeout races.
  function enterSub(item: ArcItem): void {
    if (S.retracting || !item.sub) return;
    hideAllTooltips();
    S.pending = visible(item.sub);
    S.pendingStack = [...S.stack, S.menu];
    S.retracting = true;
    S.opening = false;
    updateBubbleIcon();
    ensureLoop();
  }
  function goBack(): void {
    if (!S.stack.length || S.retracting) return;
    hideAllTooltips();
    S.pending = S.stack[S.stack.length - 1];
    S.pendingStack = S.stack.slice(0, -1);
    S.retracting = true;
    S.opening = false;
    // Icon returns to logo/close only after rebuild; keep back arrow meanwhile.
    ensureLoop();
  }

  function clickItem(i: number): void {
    if (S.retracting || S.closing) return;
    const item = S.menu[i];
    if (!item) return;
    hideAllTooltips();
    hovIdx = -1;
    if (item.sub) {
      enterSub(item);
      return;
    }
    flashBubbleGlow();
    item.onSelect?.();
    if (!item.stayOpen) {
      // Track the timer so a fast close/re-open can't collapse the new menu.
      if (closeTimer) clearTimeout(closeTimer);
      closeTimer = setTimeout(() => {
        closeTimer = null;
        if (!destroyed) closeMenu();
      }, C.CLOSE_MS);
    }
  }

  // ── tooltips ──────────────────────────────────────────────────────────────
  function hideAllTooltips(): void {
    hovIdx = -1;
    for (let i = 0; i < S.ttScales.length; i++) S.ttScales[i] = 0;
  }

  // ── main loop (gated: runs only while something is animating) ───────────────
  let loopId: number | null = null;
  let closeTimer: ReturnType<typeof setTimeout> | null = null;
  let destroyed = false;
  function needsFrame(): boolean {
    return (
      S.open ||
      S.opening ||
      S.closing ||
      S.retracting ||
      S.floating ||
      S.hoveringBubble ||
      S.bubbleTTScale > 0.01 ||
      Math.abs(S.bubbleScale - S.bubbleTgtScale) > 0.001
    );
  }
  function ensureLoop(): void {
    if (destroyed) return;
    if (loopId == null) {
      S.animStart = S.animStart || performance.now();
      loopId = requestAnimationFrame(tick);
    }
  }

  function tick(): void {
    loopId = null;
    const now = performance.now();
    const dt = now - S.animStart;
    const n = S.cur.length;

    // Bubble scale: hover target + a gentle breathing while engaged.
    S.bubbleScale = lerp(S.bubbleScale, S.bubbleTgtScale, 0.15);
    const engaged = S.open || S.hoveringBubble;
    const breath = S.pointerDown || !engaged ? 0 : Math.sin(now / 2800 * Math.PI) * 0.015;
    applyBubbleScale(S.bubbleScale + breath);

    // Bubble goo tooltip: shown on hover while the menu is closed and idle.
    if (bubbleTT) {
      const showBTT =
        S.hoveringBubble && !S.open && !S.opening && !S.closing &&
        !S.retracting && !S.pointerDown;
      S.bubbleTTScale = lerp(S.bubbleTTScale, showBTT ? 1 : 0, C.TT_LERP);
      if (S.bubbleTTScale < 0.005) S.bubbleTTScale = 0;
      positionBubbleTT();
    }

    if (S.opening) {
      S.openT = clamp(dt / C.OPEN_MS, 0, 1);
      if (S.openT >= 1) {
        S.opening = false;
        S.openT = 1;
      }
    }

    if (S.closing) {
      let maxR = 0;
      for (let i = 0; i < n; i++) {
        if (!S.cur[i]) continue;
        S.cur[i].r = lerp(S.cur[i].r, 0, C.SUB_LERP);
        S.cur[i].sc = lerp(S.cur[i].sc, 0.3, C.SUB_LERP);
        if (S.cur[i].r > maxR) maxR = S.cur[i].r;
      }
      posItems(1);
      if (maxR < 4) {
        S.open = false;
        S.closing = false;
        S.openT = 0;
        destroyItems();
        if (S.closeClear) {
          S.stack = [];
        }
        updateBubbleIcon();
      }
      if (needsFrame()) loopId = requestAnimationFrame(tick);
      return;
    }

    if (S.retracting) {
      let maxR = 0;
      for (let i = 0; i < n; i++) {
        if (!S.cur[i]) continue;
        S.cur[i].r = lerp(S.cur[i].r, 0, C.SUB_LERP);
        S.cur[i].sc = lerp(S.cur[i].sc, 0.3, C.SUB_LERP);
        if (S.cur[i].r > maxR) maxR = S.cur[i].r;
      }
      posItems(1);
      if (maxR < 4 && S.pending) {
        S.retracting = false;
        S.stack = S.pendingStack || [];
        buildItems(S.pending);
        S.pending = null;
        S.pendingStack = null;
        updateBubbleIcon();
        S.opening = true;
        S.openT = 0;
        S.animStart = performance.now();
      }
      loopId = requestAnimationFrame(tick);
      return;
    }

    if (!S.open) {
      // Only bubble hover/scale animation remains.
      if (needsFrame()) loopId = requestAnimationFrame(tick);
      return;
    }

    const arc = arcCfg(n);
    const ba = baseAngles(arc.start, arc.span, n);
    const { sc, ag } = fisheye(ba, S.mouseAng, arc.start, arc.span);

    for (let i = 0; i < n; i++) {
      if (!S.tgt[i] || !S.cur[i]) continue;
      S.tgt[i].sc = sc[i];
      S.tgt[i].ag = ag[i];
      S.tgt[i].r = C.ARC_R;
      S.cur[i].sc = lerp(S.cur[i].sc, S.tgt[i].sc, C.LERP);
      S.cur[i].ag = S.cur[i].ag + angDiff(S.cur[i].ag, S.tgt[i].ag) * C.LERP;
      S.cur[i].r = lerp(S.cur[i].r, S.tgt[i].r, C.LERP);
      const ttTgt = hovIdx === i ? 1 : 0;
      S.ttScales[i] = lerp(S.ttScales[i] || 0, ttTgt, C.TT_LERP);
      if (S.ttScales[i] < 0.005) S.ttScales[i] = 0;
    }

    posItems(easeOutCubic(S.openT));

    // Live bounds correction: gently push the bubble inward while open so the
    // arc never clips off-screen.
    if (!S.dragging && !S.floating) {
      const b = bounds();
      let nx = S.x;
      let ny = S.y;
      let moved = false;
      if (nx < b.minX) { nx = b.minX; moved = true; }
      if (nx > b.maxX) { nx = b.maxX; moved = true; }
      if (ny < b.minY) { ny = b.minY; moved = true; }
      if (ny > b.maxY) { ny = b.maxY; moved = true; }
      if (moved) {
        S.x = lerp(S.x, nx, 0.18);
        S.y = lerp(S.y, ny, 0.18);
        applyContainer();
        cb.onMove?.(S.x, S.y, false);
      }
    }

    loopId = requestAnimationFrame(tick);
  }

  // ── DOM writes for bubble + container ───────────────────────────────────────
  function applyBubbleScale(scale: number): void {
    const sz = C.BUBBLE_SZ * scale;
    gooBubble.style.width = sz + "px";
    gooBubble.style.height = sz + "px";
    bubbleCover.style.width = sz + "px";
    bubbleCover.style.height = sz + "px";
    opts.bubble.style.width = sz + "px";
    opts.bubble.style.height = sz + "px";
  }
  function applyContainer(): void {
    container.style.left = S.x + "px";
    container.style.top = S.y + "px";
  }

  // Position the bubble's goo tooltip so it buds off the bubble toward the
  // viewport center (open space), mirroring the item tooltips.
  function positionBubbleTT(): void {
    if (!bubbleTT || !bubbleTTText) return;
    const ts = S.bubbleTTScale;
    if (ts <= 0) {
      bubbleTT.style.opacity = "0";
      bubbleTTText.style.opacity = "0";
      return;
    }
    const ttW = parseFloat(bubbleTT.dataset.w || "60");
    const ttH = 26;
    const br = (C.BUBBLE_SZ / 2) * (S.bubbleScale || 1);
    const ang = Math.atan2(
      window.innerHeight / 2 - S.y,
      window.innerWidth / 2 - S.x,
    );
    const anchorR = br + C.TT_GAP;
    const ttX = Math.cos(ang) * anchorR;
    const ttY = Math.sin(ang) * anchorR;
    const ox = (1 - Math.cos(ang)) / 2;
    const oy = (1 - Math.sin(ang)) / 2;
    let pL = ttX - ox * ttW;
    let pT = ttY - oy * ttH;

    const vpPad = 6;
    const vpL = S.x + pL;
    const vpT = S.y + pT;
    const vpR = vpL + ttW;
    const vpB = vpT + ttH;
    if (vpL < vpPad) pL += vpPad - vpL;
    if (vpR > window.innerWidth - vpPad) pL -= vpR - (window.innerWidth - vpPad);
    if (vpT < vpPad) pT += vpPad - vpT;
    if (vpB > window.innerHeight - vpPad) pT -= vpB - (window.innerHeight - vpPad);

    bubbleTT.style.width = ttW + "px";
    bubbleTT.style.height = ttH + "px";
    bubbleTT.style.left = pL + "px";
    bubbleTT.style.top = pT + "px";
    bubbleTT.style.transformOrigin = `${ox * 100}% ${oy * 100}%`;
    bubbleTT.style.transform = `scale(${ts})`;
    bubbleTT.style.opacity = ts > 0.01 ? "1" : "0";
    bubbleTTText.style.left = pL + ttW * 0.5 + "px";
    bubbleTTText.style.top = pT + ttH * 0.5 + "px";
    bubbleTTText.style.opacity = ts > 0.5 ? String(clamp((ts - 0.5) * 2, 0, 1)) : "0";
  }

  // ── mouse zone (fisheye activation) ───────────────────────────────────────
  function updateZone(cx: number, cy: number): void {
    if (!S.open || S.closing) {
      S.mouseInZone = false;
      return;
    }
    const dx = cx - S.x;
    const dy = cy - S.y;
    const dist = Math.sqrt(dx * dx + dy * dy);
    const ang = Math.atan2(dy, dx);
    const arc = arcCfg(S.menu.length);
    S.mouseInZone =
      dist > C.ARC_R - 35 &&
      dist < C.ARC_R + 35 &&
      Math.abs(angDiff(ang, arc.center)) < arc.span / 2 + 0.35;
    S.mouseAng = ang;
  }

  // ── momentum physics ──────────────────────────────────────────────────────
  const velHist: { x: number; y: number; t: number }[] = [];
  const VH_SIZE = 6;
  let physFrame: number | null = null;
  let lastPhysT = 0;

  function startFloat(): void {
    if (velHist.length < 2) {
      S.vx = 0;
      S.vy = 0;
      return;
    }
    const first = velHist[0];
    const last = velHist[velHist.length - 1];
    const dt = (last.t - first.t) / 1000;
    if (dt <= 0) {
      S.vx = 0;
      S.vy = 0;
      return;
    }
    S.vx = (last.x - first.x) / dt;
    S.vy = (last.y - first.y) / dt;
    const speed = Math.hypot(S.vx, S.vy);
    if (speed < C.SNAP_VEL) {
      S.vx = 0;
      S.vy = 0;
      S.floating = false;
      cb.onMove?.(S.x, S.y, true);
      return;
    }
    const maxV = 2500;
    if (speed > maxV) {
      S.vx = (S.vx / speed) * maxV;
      S.vy = (S.vy / speed) * maxV;
    }
    S.floating = true;
    lastPhysT = performance.now();
    if (physFrame != null) cancelAnimationFrame(physFrame);
    physFrame = requestAnimationFrame(physicsTick);
  }

  function physicsTick(): void {
    const now = performance.now();
    const dt = Math.min((now - lastPhysT) / 1000, 0.05);
    lastPhysT = now;
    const f = Math.pow(C.FRICTION, dt * 60);
    S.vx *= f;
    S.vy *= f;
    S.x += S.vx * dt;
    S.y += S.vy * dt;
    const b = bounds();
    if (S.x < b.minX) { S.x = b.minX; S.vx = -S.vx * C.BOUNCE; }
    if (S.x > b.maxX) { S.x = b.maxX; S.vx = -S.vx * C.BOUNCE; }
    if (S.y < b.minY) { S.y = b.minY; S.vy = -S.vy * C.BOUNCE; }
    if (S.y > b.maxY) { S.y = b.maxY; S.vy = -S.vy * C.BOUNCE; }
    applyContainer();
    cb.onMove?.(S.x, S.y, false);
    const speed = Math.hypot(S.vx, S.vy);
    if (speed > C.MIN_VEL) {
      physFrame = requestAnimationFrame(physicsTick);
    } else {
      S.vx = 0;
      S.vy = 0;
      S.floating = false;
      physFrame = null;
      cb.onMove?.(S.x, S.y, true);
    }
  }

  // ── pointer wiring ─────────────────────────────────────────────────────────
  function onBubblePointerDown(e: PointerEvent): void {
    e.preventDefault();
    if (S.floating) {
      S.floating = false;
      S.vx = 0;
      S.vy = 0;
      if (physFrame != null) cancelAnimationFrame(physFrame);
      physFrame = null;
    }
    S.pointerDown = true;
    S.dragging = false;
    S.wasDrag = false;
    S.dragStart.x = e.clientX;
    S.dragStart.y = e.clientY;
    S.dragOff.x = e.clientX - S.x;
    S.dragOff.y = e.clientY - S.y;
    try {
      opts.bubble.setPointerCapture(e.pointerId);
    } catch (_) { /* ignore */ }
    S.bubbleTgtScale = 1.12;
    velHist.length = 0;
    velHist.push({ x: e.clientX, y: e.clientY, t: performance.now() });
    ensureLoop();
  }

  function onWindowPointerMove(e: PointerEvent): void {
    if (S.pointerDown) {
      const dx = e.clientX - S.dragStart.x;
      const dy = e.clientY - S.dragStart.y;
      if (!S.wasDrag && Math.hypot(dx, dy) > C.DRAG_TH) {
        S.wasDrag = true;
        S.dragging = true;
        opts.bubble.classList.add(cls("dragging"));
        cb.onDragChange?.(true);
      }
      if (S.dragging) {
        const b = bounds();
        S.x = clamp(e.clientX - S.dragOff.x, b.minX, b.maxX);
        S.y = clamp(e.clientY - S.dragOff.y, b.minY, b.maxY);
        applyContainer();
        cb.onMove?.(S.x, S.y, false);
        velHist.push({ x: e.clientX, y: e.clientY, t: performance.now() });
        if (velHist.length > VH_SIZE) velHist.shift();
      }
    }
    updateZone(e.clientX, e.clientY);
    if (!S.pointerDown) {
      const dx = e.clientX - S.x;
      const dy = e.clientY - S.y;
      const onBubble = Math.hypot(dx, dy) < C.BUBBLE_SZ / 2 + 4;
      if (onBubble && !S.hoveringBubble) {
        S.hoveringBubble = true;
        S.bubbleTgtScale = 1.1;
        ensureLoop();
      } else if (!onBubble && S.hoveringBubble) {
        S.hoveringBubble = false;
        S.bubbleTgtScale = 1;
        ensureLoop();
      }
    }
  }

  function onWindowPointerUp(): void {
    if (!S.pointerDown) return;
    S.pointerDown = false;
    S.dragging = false;
    opts.bubble.classList.remove(cls("dragging"));
    S.bubbleTgtScale = S.hoveringBubble ? 1.1 : 1;
    if (!S.wasDrag) {
      flashBubbleGlow();
      if (S.open && !S.closing && !S.retracting) {
        if (S.stack.length > 0) goBack();
        else closeMenu();
      } else if (!S.open && !S.opening) {
        openMenu();
      }
    } else {
      startFloat();
    }
    cb.onDragChange?.(false);
  }

  function onOutsidePointerDown(e: PointerEvent): void {
    if (!S.open || S.closing) return;
    if (cb.shouldCloseOnOutsideClick && !cb.shouldCloseOnOutsideClick()) return;
    const path = e.composedPath();
    if (path.indexOf(container) !== -1) return;
    const dx = e.clientX - S.x;
    const dy = e.clientY - S.y;
    if (Math.hypot(dx, dy) > C.ARC_R + 55) closeMenu();
  }

  function wireItem(item: ArcItem): void {
    if (wiredItems.has(item)) return;
    wiredItems.add(item);
    item.el.addEventListener("pointerenter", () => {
      const idx = S.menu.indexOf(item);
      if (idx >= 0) hovIdx = idx;
    });
    item.el.addEventListener("pointerleave", () => {
      hovIdx = -1;
    });
    item.el.addEventListener("pointerdown", (e) => e.stopPropagation());
    item.el.addEventListener("click", (e) => {
      e.stopPropagation();
      const idx = S.menu.indexOf(item);
      if (idx >= 0) clickItem(idx);
    });
  }

  // ── event registration ──────────────────────────────────────────────────
  opts.bubble.addEventListener("pointerdown", onBubblePointerDown as EventListener);
  window.addEventListener("pointermove", onWindowPointerMove as EventListener);
  window.addEventListener("pointerup", onWindowPointerUp as EventListener);
  window.addEventListener("pointerdown", onOutsidePointerDown as EventListener);

  // ── init bubble geometry ───────────────────────────────────────────────
  opts.bubbleIcon.innerHTML = opts.icons.logo;
  currentIconKey = "logo";
  applyBubbleScale(1);

  // ── public handle ─────────────────────────────────────────────────────
  return {
    config: C,
    open: openMenu,
    close: () => closeMenu(),
    toggle: () => {
      if (S.open && !S.closing) closeMenu();
      else openMenu();
    },
    back: goBack,
    isOpen: () => S.open && !S.closing,
    depth: () => S.stack.length,
    reflow: () => {
      if (!S.open || S.closing || S.retracting) return;
      // Only the root level has visibility-conditional items (the listening
      // dot). Recompute it and rebuild only when the visible set actually
      // changed, so routine polls don't re-trigger the open animation.
      const level = S.stack.length ? S.menu : visible(opts.items());
      const sameSet =
        level.length === S.menu.length &&
        level.every((it, i) => it === S.menu[i]);
      if (sameSet) return;
      buildItems(level);
      S.opening = true;
      S.openT = 0;
      S.animStart = performance.now();
      ensureLoop();
    },
    setPosition: (x, y, animate = false) => {
      // Only left/top may animate here; the base CSS transition (opacity +
      // transform) is left intact so the hide/scale animation still works.
      container.style.transition = animate ? "left .2s ease, top .2s ease" : "";
      S.x = x;
      S.y = y;
      applyContainer();
      if (animate) setTimeout(() => (container.style.transition = ""), 220);
    },
    getPosition: () => ({ x: S.x, y: S.y }),
    moveIntoView: (animate = false) => {
      const b = bounds();
      const nx = clamp(S.x, b.minX, b.maxX);
      const ny = clamp(S.y, b.minY, b.maxY);
      if (nx !== S.x || ny !== S.y) {
        container.style.transition = animate ? "left .2s ease, top .2s ease" : "";
        S.x = nx;
        S.y = ny;
        applyContainer();
        cb.onMove?.(S.x, S.y, true);
        if (animate) setTimeout(() => (container.style.transition = ""), 220);
      }
    },
    clampToViewport: () => {
      const b = bounds();
      const nx = clamp(S.x, b.minX, b.maxX);
      const ny = clamp(S.y, b.minY, b.maxY);
      if (nx !== S.x || ny !== S.y) {
        S.x = nx;
        S.y = ny;
        applyContainer();
        cb.onMove?.(S.x, S.y, true);
      }
    },
    destroy: () => {
      destroyed = true;
      if (loopId != null) cancelAnimationFrame(loopId);
      if (physFrame != null) cancelAnimationFrame(physFrame);
      if (iconTimer) clearTimeout(iconTimer);
      if (closeTimer) { clearTimeout(closeTimer); closeTimer = null; }
      opts.bubble.removeEventListener("pointerdown", onBubblePointerDown as EventListener);
      window.removeEventListener("pointermove", onWindowPointerMove as EventListener);
      window.removeEventListener("pointerup", onWindowPointerUp as EventListener);
      window.removeEventListener("pointerdown", onOutsidePointerDown as EventListener);
      if (bubbleTT) bubbleTT.remove();
      if (bubbleTTText) bubbleTTText.remove();
      destroyItems();
    },
  };
}

// ── goo filter ──────────────────────────────────────────────────────────────
// Two-step implicit surface: blur creates overlapping alpha fields, then a
// color-matrix alpha threshold produces a sharp cutoff → thin organic necks
// where circles are near, clean edges where they aren't. feComposite(atop)
// re-composites the crisp original so circle edges stay pixel-clean.
function ensureGooFilter(scope: ShadowRoot, prefix: string, C: ArcConfig): void {
  const id = prefix + "goo";
  if (scope.getElementById && scope.getElementById(id)) return;
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("width", "0");
  svg.setAttribute("height", "0");
  svg.style.cssText = "position:absolute;width:0;height:0;overflow:hidden;";
  const defs = document.createElementNS(SVG_NS, "defs");
  const filter = document.createElementNS(SVG_NS, "filter");
  filter.setAttribute("id", id);
  filter.setAttribute("color-interpolation-filters", "sRGB");

  const blur = document.createElementNS(SVG_NS, "feGaussianBlur");
  blur.setAttribute("in", "SourceGraphic");
  blur.setAttribute("stdDeviation", String(C.gooBlur));
  blur.setAttribute("result", "blur");

  const cmat = document.createElementNS(SVG_NS, "feColorMatrix");
  cmat.setAttribute("in", "blur");
  cmat.setAttribute("type", "matrix");
  cmat.setAttribute(
    "values",
    `1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 ${C.gooAlpha} ${C.gooOffset}`,
  );
  cmat.setAttribute("result", "goo");

  const comp = document.createElementNS(SVG_NS, "feComposite");
  comp.setAttribute("in", "SourceGraphic");
  comp.setAttribute("in2", "goo");
  comp.setAttribute("operator", "atop");

  filter.appendChild(blur);
  filter.appendChild(cmat);
  filter.appendChild(comp);
  defs.appendChild(filter);
  svg.appendChild(defs);
  scope.appendChild(svg);
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}
