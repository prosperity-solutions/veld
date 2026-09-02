/**
 * Device emulation and page zoom for browser panes — the pure half.
 *
 * A pane is small and a desktop layout is not, which is the whole argument for
 * doing this inside the dock rather than saying "use Chrome": emulating a
 * 1440-wide viewport *scaled down to fit a 600px pane* is the one case a real
 * browser window cannot give you without a second monitor.
 *
 * Everything here is data and arithmetic, so the rules are testable without a
 * DOM and without an Electron binary — the same discipline as `model.ts`. What
 * *applies* an emulation lives in `browserHost.ts` (which owns the live views)
 * and `desktop/src/browserViews.js` (which owns the native ones).
 *
 * ## Why the stored shape is metrics and not a preset id
 *
 * [`PaneEmulation`] carries every number it needs. A tab could instead store
 * `device: "iphone-15-pro"` and have the renderer look the metrics up, but then
 * a layout written by a build whose preset table has since changed restores as
 * something else — silently, and only for the presets that moved. The id is kept
 * alongside for the *label* only, and a restored emulation with an unknown id
 * degrades to "Custom", which is exactly what it now is.
 */

/** The preset id a hand-entered size carries. */
export const CUSTOM_DEVICE = "custom";

/**
 * The id of the resizable viewport — the one you drag rather than pick.
 *
 * A fixed list cannot cover the size your layout actually breaks at, and finding
 * that size is most of what this feature is for: you drag until it breaks and then
 * read the number off the chrome. It starts at whatever the pane can hold, so
 * turning it on changes nothing visually and only adds the handles.
 *
 * Distinct from [`CUSTOM_DEVICE`] because they mean different things to a reader:
 * "Responsive" is a mode you are in, while "Custom" is a size you set on a device.
 * Dragging a *preset* therefore lands on `custom` (it is no longer that class),
 * while dragging the responsive viewport stays responsive.
 */
export const RESPONSIVE_DEVICE = "responsive";

/**
 * The preset the pane bar's quick device switch applies.
 *
 * The switch used to enter [`RESPONSIVE_DEVICE`], which is the one emulation whose
 * arrival looks like nothing happened: it starts at exactly what the pane can hold,
 * so a click that costs a trip to the bar returned a page the same size with drag
 * handles on it. A phone is what "show me this small" means, and it is also the
 * answer to the question the switch is reached for — does this layout survive a
 * phone — rather than a starting point for finding it.
 *
 * Nothing is lost by the change: dragging a preset's edge lands on
 * [`CUSTOM_DEVICE`] (`resizeEmulation`), so the resizable viewport is one drag
 * away, and the device menu still has Responsive as a named item.
 *
 * An id rather than the preset itself, because the preset table is the one thing
 * this must not duplicate — `presetById` is the lookup, and the preset table's own
 * `still contains the row the pane bar's quick switch applies` is the gate that
 * keeps a renamed row from silently removing the switch.
 */
export const QUICK_DEVICE = "phone";

/**
 * Bounds on an emulated viewport.
 *
 * The lower one is below any real device because rotating a 120-wide viewport is
 * a legitimate thing to try; the upper one is a 4K width plus room, because
 * beyond that the emulation is larger than the compositor surface anyone has and
 * `scale` is doing all the work anyway.
 */
export const MIN_DEVICE_PX = 120;
export const MAX_DEVICE_PX = 4096;

/**
 * Cap on a user-agent string.
 *
 * A UA becomes a request header on every navigation the pane makes, so its
 * length and charset are a real constraint rather than a formatting preference —
 * see [`safeUserAgentText`], and `safeUserAgent` in `desktop/src/validate.js`,
 * which is the copy that actually guards the header.
 */
export const MAX_UA_LEN = 512;

/** Ceiling on a screen radius, and what a hand-entered size gets. Custom sizes
 *  are barely rounded on purpose: nobody said that viewport was a phone. */
export const MAX_DEVICE_RADIUS = 64;
export const CUSTOM_RADIUS = 8;

/**
 * Gap between the emulated screen and the pane's edge, in CSS pixels.
 *
 * Not decoration, and it carries three jobs. Without it an emulated viewport
 * scaled to fit reaches every edge of the pane and is indistinguishable from the
 * pane simply *being* that page; it is what gives the screen's rounded corners
 * something to be round against; and it is the only place the resize handles can
 * live, since under Electron the native view covers the screen's own rect and
 * takes every pointer event inside it.
 *
 * That last job is why it is this wide rather than the 14 it started at: a handle
 * a few pixels from the pane's corner sits in the *window's* own resize zone, and
 * a user reaching for it drags the whole app window instead.
 */
export const DEVICE_PADDING = 20;

/**
 * The resize handles' geometry, in the gap.
 *
 * Here rather than as literals at the call site because the gap and the handles are
 * one constraint, and it has already been got wrong once: at a 14px gap the corner
 * handle sat inside the *window's* own resize zone, so reaching for it dragged the
 * whole app. Widening the gap fixed it — and left the relationship undocumented, so
 * the next person to make the gap smaller (it is already a parameter of
 * [`deviceLayout`]) would reopen exactly that bug with nothing to stop them.
 * [`MIN_DEVICE_PADDING`] is that floor, and a test asserts the gap clears it.
 *
 * The *visual* sizes live in `styles.css` (`.device-handle`), which owns hover and
 * cursor states; these are the numbers the pane positions them with, and the two sets
 * are kept in step by hand — which is fine while they are four numbers and a
 * cross-reference, and is why the floor below is deliberately generous.
 */
export const HANDLE_EDGE_GAP = 6;
export const HANDLE_THICKNESS = 6;
export const HANDLE_LENGTH = 44;
export const HANDLE_CORNER_GAP = 4;
export const HANDLE_CORNER_SIZE = 12;

/**
 * How far each handle's *clickable* box extends past its mark (`::before` in
 * `styles.css`), which is what actually reaches the pane's edge — the marks are only
 * what you see. The floor below is computed from these, not from the marks: a floor that
 * measured the visible pixels only understated the reach by the whole bleed, which is
 * how a "documented minimum" ends up documenting the wrong number.
 *
 * The corner's is smaller than the edges' on purpose. It is the one anchored diagonally
 * into the corner, so it is the one whose bleed lands where the OS window-resize grip
 * is, and 12px of mark plus 7px of bleed would not fit the gap at all.
 */
export const HANDLE_HIT_BLEED = 7;
export const HANDLE_CORNER_HIT_BLEED = 3;

/** The narrowest gap the handles still fit inside — marks *and* hit areas — with a pixel
 *  of air. Anything tighter and a handle's clickable box overlaps the pane's edge, which
 *  is the window's edge, and therefore the OS resize grip, whenever the pane is the last
 *  one in the dock. Reaching for the handle would drag the whole app instead. */
export const MIN_DEVICE_PADDING =
  Math.max(
    HANDLE_EDGE_GAP + HANDLE_THICKNESS + HANDLE_HIT_BLEED,
    HANDLE_CORNER_GAP + HANDLE_CORNER_SIZE + HANDLE_CORNER_HIT_BLEED,
  ) + 1;

export interface DevicePreset {
  id: string;
  label: string;
  /** The submenu it appears under. */
  group: "Phones" | "Tablets" | "Screens";
  width: number;
  height: number;
  /**
   * Emulated DPR, or 0 for "whatever the host display has".
   *
   * Integers only: Electron types this parameter `Integer`, so a real device's
   * fractional ratio (a Pixel's 2.625) is not expressible — it is rounded to the
   * nearest whole one here rather than passed and quietly truncated in the
   * shell. It affects `devicePixelRatio` and which `srcset` candidate a page
   * picks, not layout, so the rounding costs nothing a layout review would see.
   */
  deviceScaleFactor: number;
  /**
   * Chromium's `mobile` screen position: viewport meta tag handling, overlay
   * scrollbars, text autosizing. Independent of touch — a page can be laid out
   * as mobile and still receive mouse events.
   */
  mobile: boolean;
  /** Whether picking this preset also turns touch emulation on. */
  touch: boolean;
  /**
   * Corner radius of the device's screen, in the device's own pixels.
   *
   * Part of what makes an emulated pane readable as a *device* rather than as a
   * small window: a phone's screen is visibly round-cornered, a monitor's is
   * nearly square. Scaled with the viewport when the pane shrinks it, so a phone
   * at 40% keeps its proportions instead of looking like a rounded rectangle
   * someone forgot to scale.
   */
  radius: number;
  /**
   * UA template, or `null` to keep the shell's own.
   *
   * `{chrome}` is substituted with the host Chromium's version at the moment the
   * preset is picked ([`resolveUserAgent`]), so an Android UA does not claim a
   * Chrome release that predates the app by two years. The *resolved* string is
   * what gets stored: it is a claim about the emulated device, and freezing it
   * keeps a restored layout emulating what it emulated yesterday.
   *
   * The screen presets carry `null` on purpose. Emulating a 1440-wide laptop is
   * a layout question, and the shell already *is* a desktop browser — sending a
   * second desktop UA would only add a way for the string to be wrong.
   */
  ua: string | null;
}

/**
 * The user agents the mobile classes send.
 *
 * Android Chrome rather than iOS Safari, for both: `{chrome}` keeps the version
 * honest against the engine actually making the request ([`resolveUserAgent`]),
 * which an iOS string cannot do — it would have to carry a hardcoded iOS release
 * that goes stale silently. The load-bearing part of a mobile UA for a dev preview
 * is the `Mobile` token and the platform, not which vendor it names. A test that
 * genuinely needs iOS sniffing wants a real device, not an emulated one.
 *
 * The tablet string drops `Mobile`, because that token is exactly how a server
 * tells a tablet from a phone.
 */
const MOBILE_UA =
  "Mozilla/5.0 (Linux; Android 16; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{chrome} Mobile Safari/537.36";
const TABLET_UA =
  "Mozilla/5.0 (Linux; Android 16; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{chrome} Safari/537.36";

/**
 * The menu: **size classes, not model names.**
 *
 * A list of named handsets is the thing everyone dislikes about the browser
 * devtools version of this — it is long, it is a year out of date the moment it
 * ships, and "iPhone 14 Pro" tells you nothing about the only question you are
 * asking, which is *how wide*. So the classes are named by what they are, and each
 * one carries the metrics a current device of that class actually reports:
 *
 * - phones: 360 is the commonest Android viewport, 402 the current flagship
 *   width, 440 the large/Max end
 * - tablets: the 8-inch, 11-inch and 13-inch classes
 * - screens: a 14-inch laptop at its default scaled resolution, a 24-inch 1080p
 *   monitor, and a 27-inch QHD one
 *
 * Anything between them is a drag away — [`RESPONSIVE_DEVICE`] and the custom
 * size are what a fixed list can never cover, which is also why this one stays
 * short instead of trying.
 *
 * Sizes are CSS pixels as the device reports them, so they are what a page's media
 * queries see. Device pixel ratios are integers because Electron types the
 * parameter that way; a real 2.625 or 3.75 rounds, which changes which `srcset`
 * candidate is picked and nothing about layout.
 */
export const DEVICE_PRESETS: readonly DevicePreset[] = [
  {
    id: "phone-small",
    label: "Small phone",
    group: "Phones",
    width: 360,
    height: 780,
    deviceScaleFactor: 3,
    radius: 36,
    mobile: true,
    touch: true,
    ua: MOBILE_UA,
  },
  {
    id: "phone",
    label: "Phone",
    group: "Phones",
    width: 402,
    height: 874,
    deviceScaleFactor: 3,
    radius: 44,
    mobile: true,
    touch: true,
    ua: MOBILE_UA,
  },
  {
    id: "phone-large",
    label: "Large phone",
    group: "Phones",
    width: 440,
    height: 956,
    deviceScaleFactor: 3,
    radius: 48,
    mobile: true,
    touch: true,
    ua: MOBILE_UA,
  },
  {
    id: "tablet-small",
    label: "Small tablet",
    group: "Tablets",
    width: 744,
    height: 1133,
    deviceScaleFactor: 2,
    radius: 20,
    mobile: true,
    touch: true,
    ua: TABLET_UA,
  },
  {
    id: "tablet",
    label: "Tablet",
    group: "Tablets",
    width: 820,
    height: 1180,
    deviceScaleFactor: 2,
    radius: 22,
    mobile: true,
    touch: true,
    ua: TABLET_UA,
  },
  {
    id: "tablet-large",
    label: "Large tablet",
    group: "Tablets",
    width: 1024,
    height: 1366,
    deviceScaleFactor: 2,
    radius: 22,
    mobile: true,
    touch: true,
    ua: TABLET_UA,
  },
  {
    // A 14-inch laptop's *default scaled* resolution, not its panel: that is what
    // the browser lays out against, and it is the one screen size where the two
    // differ enough to matter.
    id: "laptop",
    label: 'Laptop 14"',
    group: "Screens",
    width: 1512,
    height: 982,
    deviceScaleFactor: 2,
    radius: 10,
    mobile: false,
    touch: false,
    ua: null,
  },
  {
    id: "desktop",
    label: 'Desktop 24"',
    group: "Screens",
    width: 1920,
    height: 1080,
    deviceScaleFactor: 1,
    radius: 8,
    mobile: false,
    touch: false,
    ua: null,
  },
  {
    id: "widescreen",
    label: 'Widescreen 27"',
    group: "Screens",
    width: 2560,
    height: 1440,
    deviceScaleFactor: 1,
    radius: 8,
    mobile: false,
    touch: false,
    ua: null,
  }
];

/** The groups in menu order, derived so adding a preset needs no second edit. */
export const DEVICE_GROUPS: ReadonlyArray<DevicePreset["group"]> = ["Phones", "Tablets", "Screens"];

export function presetById(id: string): DevicePreset | null {
  return DEVICE_PRESETS.find((p) => p.id === id) ?? null;
}

/**
 * What a pane emulates, as stored in its tab.
 *
 * Absent (rather than a disabled instance) when the pane is showing itself at
 * pane size, so a layout written before this existed and one with emulation
 * switched off are the same thing.
 */
export interface PaneEmulation {
  /** Preset id, or [`CUSTOM_DEVICE`]. Label only — the metrics below win. */
  device: string;
  /** Emulated viewport, in CSS pixels. */
  width: number;
  height: number;
  /** Emulated DPR; 0 keeps the host display's. */
  deviceScaleFactor: number;
  mobile: boolean;
  /**
   * Emit touch events for mouse input, and report a touch-capable device.
   *
   * The only field here that is not native Electron: it needs a CDP session, and
   * Chromium gives the built-in DevTools that session exclusively — so touch is
   * *suspended* while a pane's DevTools is open and resumes when it closes. The
   * pane says so rather than silently lying (`touchActive` in `BrowserState`).
   */
  touch: boolean;
  /** UA to send, already resolved; `null` keeps the shell's own. */
  ua: string | null;
  /** Scale the emulated viewport down so all of it fits in the pane. */
  fit: boolean;
  /** Screen corner radius in the device's own pixels; see `DevicePreset.radius`. */
  radius: number;
}

/** An emulation from a preset. `chrome` fills the UA template's `{chrome}`. */
export function emulationForPreset(
  preset: DevicePreset,
  opts: { landscape?: boolean; chrome?: string; fit?: boolean } = {},
): PaneEmulation {
  const landscape = opts.landscape ?? false;
  return {
    device: preset.id,
    width: landscape ? preset.height : preset.width,
    height: landscape ? preset.width : preset.height,
    deviceScaleFactor: preset.deviceScaleFactor,
    mobile: preset.mobile,
    touch: preset.touch,
    ua: preset.ua === null ? null : resolveUserAgent(preset.ua, opts.chrome),
    fit: opts.fit ?? true,
    radius: preset.radius,
  };
}

/**
 * An emulation at a hand-entered size.
 *
 * Keeps `base`'s device flags when there is one, so nudging a phone's width by
 * 10px does not also drop touch and the mobile UA — which is what makes the
 * custom size useful as "this preset, but narrower".
 */
export function customEmulation(
  width: number,
  height: number,
  base?: PaneEmulation | null,
): PaneEmulation {
  return {
    device: CUSTOM_DEVICE,
    width: clampDevicePx(width),
    height: clampDevicePx(height),
    deviceScaleFactor: base?.deviceScaleFactor ?? 0,
    mobile: base?.mobile ?? false,
    touch: base?.touch ?? false,
    ua: base?.ua ?? null,
    fit: base?.fit ?? true,
    // A hand-entered size is a *window* unless it inherited a device's shape:
    // rounding a viewport nobody claimed is a phone would be decoration.
    radius: base?.radius ?? CUSTOM_RADIUS,
  };
}

/**
 * The resizable viewport, starting at the size the pane can currently hold.
 *
 * Deliberately a plain desktop viewport: it is "the pane, but with a number on it
 * and handles to drag", so claiming a device pixel ratio, touch support or a mobile
 * user agent would make turning it on change how the page *behaves* rather than
 * just how wide it is. The toggles are there if you want any of that.
 */
export function responsiveEmulation(width: number, height: number): PaneEmulation {
  return {
    device: RESPONSIVE_DEVICE,
    width: clampDevicePx(width),
    height: clampDevicePx(height),
    deviceScaleFactor: 0,
    mobile: false,
    touch: false,
    ua: null,
    // Fitting on, so dragging *past* what the pane can show scales it down rather
    // than cropping it — the same rule as a screen preset too big for the pane.
    fit: true,
    radius: CUSTOM_RADIUS,
  };
}

/**
 * The emulation a drag lands on.
 *
 * Keeps every flag — a phone dragged narrower is still a phone, with its touch
 * events and its user agent — and only the identity changes: a dragged preset
 * becomes `custom`, because it is no longer the class the menu named, while the
 * responsive viewport stays itself. The shape goes with the flags, so a dragged
 * phone keeps its rounded screen.
 */
export function resizeEmulation(e: PaneEmulation, width: number, height: number): PaneEmulation {
  return {
    ...e,
    device: e.device === RESPONSIVE_DEVICE ? RESPONSIVE_DEVICE : CUSTOM_DEVICE,
    width: clampDevicePx(width),
    height: clampDevicePx(height),
  };
}

/**
 * Turn the mobile user agent on or off, whatever size is set.
 *
 * The one device claim worth changing independently of the size: "does my app
 * serve the mobile bundle at this width" and "does my layout survive this width"
 * are different questions, and a custom or responsive size has no preset to
 * inherit a UA from at all. Sends the tablet string above the phone classes' widest
 * point, since dropping `Mobile` is exactly how a server tells the two apart.
 */
export function withMobileUserAgent(
  e: PaneEmulation,
  on: boolean,
  chrome?: string,
): PaneEmulation {
  // Only the user agent, both ways. It used to set and clear `mobile` alongside —
  // Chromium's `screenPosition`, which governs viewport-meta handling, overlay
  // scrollbars and text autosizing, and which the preset owns. So unticking a menu
  // item labelled "Mobile user agent" on a Phone silently converted it to a
  // desktop-layout viewport, with no control able to put it back. The label is
  // exactly what this changes.
  if (!on) return { ...e, ua: null };
  const template = e.width > 600 ? TABLET_UA : MOBILE_UA;
  return { ...e, ua: resolveUserAgent(template, chrome) };
}

/** Swap width and height. The preset id is kept: it is still that device, held
 *  the other way round, and [`isLandscape`] tells the label which way. */
export function rotateEmulation(e: PaneEmulation): PaneEmulation {
  return { ...e, width: e.height, height: e.width };
}

/** Whether an emulation is wider than it is tall. Used for the label and to
 *  decide which way `rotate` is about to turn a preset. */
export function isLandscape(e: PaneEmulation): boolean {
  return e.width > e.height;
}

export function clampDevicePx(n: number): number {
  if (!Number.isFinite(n)) return MIN_DEVICE_PX;
  return Math.min(MAX_DEVICE_PX, Math.max(MIN_DEVICE_PX, Math.round(n)));
}

/** `393 × 852` — the size, with the multiplication sign a human would write. */
export function emulationSize(e: PaneEmulation): string {
  return `${e.width} × ${e.height}`;
}

/**
 * The device's name.
 *
 * A preset held sideways says so, because a rotated phone and a small tablet are
 * the same numbers and not the same test. An unknown id — a preset that has
 * since been renamed or removed — reads as "Custom", which is what an emulation
 * with metrics and no matching device is.
 */
export function emulationLabel(e: PaneEmulation): string {
  if (e.device === RESPONSIVE_DEVICE) return "Responsive";
  const preset = presetById(e.device);
  if (!preset) return "Custom";
  const rotated = isLandscape(e) !== preset.width > preset.height;
  return rotated ? `${preset.label} · landscape` : preset.label;
}

/** Where the emulated screen sits inside a pane, and how far it is scaled. All
 *  CSS pixels, relative to the pane's own box. */
export interface DeviceLayout {
  x: number;
  y: number;
  width: number;
  height: number;
  /** Factor the emulated viewport is rendered at; 1 when it fits, or when
   *  fitting is off (where an oversized device is cropped instead). */
  scale: number;
}

/**
 * Place the emulated screen in the pane: centred, inset by [`DEVICE_PADDING`],
 * scaled to fit if asked.
 *
 * **The single owner of this arithmetic**, for both backends and both processes.
 * The shell used to compute the scale itself, from the native view's bounds in
 * device-independent pixels — the only number a renderer cannot derive, since page
 * zoom scales its CSS pixels and not a native view's bounds. That put the box in
 * one place and the scale in another, which is a drift waiting to be a
 * half-off-screen device; now the renderer computes both and the shell applies
 * what it is given.
 *
 * Fitting considers **both** dimensions: the emulated screen *is* the view, so a
 * viewport scaled to the pane's width but taller than the pane is clipped with
 * nothing to scroll it into sight.
 *
 * With fitting **off** an oversized device is cropped rather than allowed to
 * overflow, and anchored top-left rather than centred — under Electron the view is
 * a native sibling that the pane's box does not clip, so an oversized rect would
 * paint over the neighbouring pane; and when only part of a page is visible, the
 * part worth showing is the top of it.
 */
export function deviceLayout(
  e: PaneEmulation,
  box: { width: number; height: number },
  padding: number = DEVICE_PADDING,
): DeviceLayout {
  const availWidth = box.width - padding * 2;
  const availHeight = box.height - padding * 2;
  // A pane too small to inset (mid-layout, or a sliver): fill it rather than
  // computing a negative box.
  if (!(availWidth >= 1) || !(availHeight >= 1)) {
    return { x: 0, y: 0, width: Math.max(0, box.width), height: Math.max(0, box.height), scale: 1 };
  }
  const scale = e.fit ? Math.min(1, availWidth / e.width, availHeight / e.height) : 1;
  const width = Math.min(e.width * scale, availWidth);
  const height = Math.min(e.height * scale, availHeight);
  return {
    x: padding + Math.max(0, (availWidth - width) / 2),
    y: padding + Math.max(0, (availHeight - height) / 2),
    width,
    height,
    scale,
  };
}

/**
 * The emulated size a drag has reached, from the pointer's travel.
 *
 * Here rather than inline in the component because it is arithmetic with two
 * corrections in it, and arithmetic with corrections is the kind that needs a test:
 *
 * 1. **Scale.** The screen is drawn at `layout.scale`, so a pointer moved 100px over
 *    a viewport shown at 50% has moved 200 of that viewport's own pixels.
 * 2. **Centre growth.** The screen grows about its middle, so the edge under the
 *    cursor travels only half of whatever the size changes by — doubling puts the
 *    edge back under the pointer.
 *
 * The second correction only applies while the screen has room to grow. Once the pane is
 * what limits the drawn size, the edge cannot move at all, and doubling then only
 * doubles how fast the number runs away from the pointer — in exactly the
 * big-screen-in-a-small-pane case this feature exists for. The caller passes that per
 * axis, from [`edgePinned`].
 *
 * **Both `scale` and `pinned` are sampled once, when the gesture starts**, and the whole
 * pointer travel is mapped with them. Reading them per move is the obvious idea and it
 * is wrong twice over: the published geometry a per-move read sees is coalesced to one
 * animation frame while a mouse reports faster than the display, so the answer flips
 * between moves — and because the gain applies to the *total* travel, every flip jumps
 * the size, which made the emulated width non-monotonic for a steadily advancing
 * cursor. Sampling once makes the gesture linear, which is what a drag should be. The
 * cost is that dragging from pinned into unpinned keeps the slower gain until you let go.
 */
export function dragSize(
  from: { width: number; height: number },
  delta: { x: number; y: number },
  axis: "x" | "y" | "both",
  scale: number,
  pinned: { width: boolean; height: boolean } = { width: false, height: false },
): { width: number; height: number } {
  // A zero or nonsense scale would divide the pointer into infinity; 1 is the only
  // safe reading of "we do not know how far this is scaled".
  const safe = Number.isFinite(scale) && scale > 0 ? scale : 1;
  const step = (pinnedOnAxis: boolean) => (pinnedOnAxis ? 1 : 2) / safe;
  return {
    width: axis === "y" ? from.width : clampDevicePx(from.width + delta.x * step(pinned.width)),
    height: axis === "x" ? from.height : clampDevicePx(from.height + delta.y * step(pinned.height)),
  };
}

/**
 * Which of the screen's edges cannot move, because the pane is already what limits
 * them.
 *
 * The question [`dragSize`] needs answered: pulling an edge that is pinned changes
 * the *number* without moving anything, so the centre-growth doubling has nothing to
 * correct for and only doubles how fast the number runs from the pointer.
 *
 * Derived from `deviceLayout` on purpose — that is the one owner of the placement, and
 * an axis is pinned exactly when the layout it produces is capped by the available
 * box. Two ways that happens: fitting binds on that axis (`scale = avail/size`, so the
 * drawn size *is* the available size no matter how the emulated one changes), or
 * fitting is off and the device is bigger than the pane, where the screen is cropped.
 *
 * Must be computed from the same box and the same emulated size as everything else in
 * the tick that uses it. A version of this read the *published* geometry instead — the
 * box last painted — which is coalesced to one animation frame while a mouse reports
 * faster than the display: the answer then flipped between moves and the emulated size
 * stopped being monotonic in pointer travel.
 */
export function edgePinned(
  e: PaneEmulation,
  box: { width: number; height: number },
  padding: number = DEVICE_PADDING,
): { width: boolean; height: boolean } {
  const availWidth = box.width - padding * 2;
  const availHeight = box.height - padding * 2;
  if (!(availWidth >= 1) || !(availHeight >= 1)) return { width: false, height: false };
  const layout = deviceLayout(e, box, padding);
  // Being *at* the cap is not the same as being held there, and the difference is the
  // whole point. A responsive viewport starts at exactly the available box: its drawn
  // width equals the available width, so a cap test alone called it pinned — and the
  // first drag then ran at half gain, so the edge moved half as far as the cursor while
  // every later drag tracked it exactly. Nothing was being shrunk at that instant, and
  // the edge could still move inward.
  //
  // Held means one of two things: fitting is actively scaling the screen down
  // (`scale < 1`, so growing the number cannot move the edge — it only buys more
  // shrinking), or fitting is off and the screen is genuinely larger than the pane, where
  // it is cropped. Equality with nothing shrinking it is neither.
  const shrinking = layout.scale < 1 - 1e-6;
  return {
    // A half-pixel of slack on the cap: these are floats out of a division, and "equal"
    // means "the layout hit the cap", not "the two doubles agree bit for bit".
    width:
      layout.width >= availWidth - 0.5 && (shrinking || (!e.fit && e.width > availWidth + 0.5)),
    height:
      layout.height >= availHeight - 0.5 && (shrinking || (!e.fit && e.height > availHeight + 0.5)),
  };
}

/** The screen's corner radius at the scale it is being shown at, so a phone at
 *  40% keeps its shape instead of its pixel count. */
export function scaledRadius(e: PaneEmulation, scale: number): number {
  return Math.round(Math.min(MAX_DEVICE_RADIUS, Math.max(0, e.radius)) * scale);
}

/** `Portrait` / `Landscape` — the state the Rotate control is currently in, which
 *  is otherwise only inferable by reading the two numbers. */
export function orientationLabel(e: PaneEmulation): string {
  return isLandscape(e) ? "Landscape" : "Portrait";
}

/**
 * A UA string that is safe to make into a request header, or `null`.
 *
 * Rejects rather than strips: a UA with a newline in it is not a UA with a typo,
 * and quietly repairing one hides where it came from. Printable ASCII only —
 * `setUserAgent` takes a header value, and CR/LF is header injection while
 * non-ASCII is not representable in one.
 *
 * The shell repeats this check (`safeUserAgent` in `desktop/src/validate.js`),
 * because a renderer is not a trust boundary; this copy is what keeps a
 * hand-edited `sessionStorage` from restoring a pane that then fails in the
 * shell with nothing to show for it.
 */
export function safeUserAgentText(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const text = raw.trim();
  if (text === "" || text.length > MAX_UA_LEN) return null;
  if (!/^[\x20-\x7e]+$/.test(text)) return null;
  return text;
}

/**
 * Fill a UA template's `{chrome}` with the host Chromium version.
 *
 * Falls back to dropping the version rather than emitting a literal `{chrome}`:
 * a UA that names no Chrome release is a UA a server may not recognise, while
 * one containing braces is a UA no server has ever seen.
 */
export function resolveUserAgent(template: string, chrome: string | undefined): string {
  if (!template.includes("{chrome}")) return template;
  const version = chrome && /^[0-9][0-9.]{0,15}$/.test(chrome) ? chrome : null;
  if (!version) return template.replace(/\s*Chrome\/\{chrome\}/g, "").replace("{chrome}", "");
  return template.replaceAll("{chrome}", version);
}

/** The Chromium version out of a UA string, or `undefined`. The /ide renderer's
 *  own UA is the honest source for it: whatever Chromium the shell is built on
 *  is the one that will make the request. */
export function chromeVersionFrom(ua: string | undefined): string | undefined {
  const m = ua?.match(/Chrome\/([0-9][0-9.]*)/);
  return m ? m[1] : undefined;
}

// ---------------------------------------------------------------------------
// Zoom
// ---------------------------------------------------------------------------

/**
 * Page zoom bounds, matching Chromium's own (25%–300%).
 *
 * Zoom is stored and applied as a *factor*, not Chromium's zoom level, because
 * the pane shows a percentage and `1.2 ^ level` is a needless conversion in
 * between.
 */
export const MIN_ZOOM = 0.25;
export const MAX_ZOOM = 3;
export const DEFAULT_ZOOM = 1;

/** The steps the device menu's − and + walk. Chrome's own ladder, uneven on purpose:
 *  the small end needs finer steps than the large one. (No keyboard accelerator is
 *  wired for these — the only one the shell forwards is the command palette's.) */
export const ZOOM_STEPS: readonly number[] = [
  0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3,
];

export function clampZoom(n: number): number {
  if (!Number.isFinite(n) || n <= 0) return DEFAULT_ZOOM;
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, n));
}

/**
 * The next step up or down from wherever the pane currently is.
 *
 * Snaps to the ladder rather than assuming the current factor is on it: a zoom
 * restored from storage, or set before the ladder changed, must still step
 * somewhere sensible instead of jumping to one end.
 */
export function zoomStep(current: number, direction: 1 | -1): number {
  const now = clampZoom(current);
  const steps = direction === 1 ? ZOOM_STEPS : [...ZOOM_STEPS].reverse();
  // A tolerance, because 0.67 and 0.6700000000000001 are the same step.
  const next = steps.find((s) => (direction === 1 ? s > now + 1e-6 : s < now - 1e-6));
  return next ?? now;
}

/** `100%`, `67%`, `42%` — a factor as a percentage, rounded. */
export function formatPercent(factor: number): string {
  return `${Math.round(factor * 100)}%`;
}

/**
 * A zoom factor as a percentage.
 *
 * Clamped, because this labels a control whose range is Chromium's — unlike
 * [`formatPercent`], which also renders the *fit* scale, and a viewport shrunk to
 * 12% of a narrow pane is a real 12% rather than the zoom floor.
 */
export function formatZoom(factor: number): string {
  return formatPercent(clampZoom(factor));
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

/**
 * Accept a stored emulation, or reject it.
 *
 * `sessionStorage` is where a stale build's — or a hand-edited — emulation sits
 * waiting to be handed to a view on restore, exactly as a pane's URL does
 * (`parseTab` in `model.ts`). Everything is clamped rather than trusted, and a
 * value that cannot be repaired into a number drops the whole emulation instead
 * of restoring a pane at a size nobody chose: the honest degradation is "no
 * emulation", which is a pane showing itself at pane size.
 */
export function sanitizeEmulation(raw: unknown): PaneEmulation | null {
  if (typeof raw !== "object" || raw === null) return null;
  const e = raw as Record<string, unknown>;
  if (!Number.isFinite(Number(e.width)) || !Number.isFinite(Number(e.height))) return null;
  const dsf = Number(e.deviceScaleFactor);
  return {
    device:
      e.device === RESPONSIVE_DEVICE ||
      (typeof e.device === "string" && presetById(e.device) !== null)
        ? e.device
        : CUSTOM_DEVICE,
    width: clampDevicePx(Number(e.width)),
    height: clampDevicePx(Number(e.height)),
    // 0 means "the host display's", which is also the honest answer for a value
    // that is missing, negative, or fractional past what Electron accepts.
    deviceScaleFactor: Number.isFinite(dsf) ? Math.min(4, Math.max(0, Math.round(dsf))) : 0,
    mobile: e.mobile === true,
    touch: e.touch === true,
    ua: safeUserAgentText(e.ua),
    // Absent means a square-ish screen, which is what an emulation written before
    // devices had a shape was being shown as anyway.
    radius: Number.isFinite(Number(e.radius))
      ? Math.min(MAX_DEVICE_RADIUS, Math.max(0, Math.round(Number(e.radius))))
      : 0,
    // Absent means fitting, which is what every emulation this build writes does
    // and the only setting under which a screen preset is usable in a pane.
    fit: e.fit !== false,
  };
}

/** Accept a stored zoom factor, or `null` for "the pane was at 100%". Stored
 *  only when it isn't, so an untouched pane writes nothing. */
export function sanitizeZoom(raw: unknown): number | null {
  if (raw === undefined || raw === null) return null;
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return null;
  const zoom = clampZoom(n);
  return zoom === DEFAULT_ZOOM ? null : zoom;
}

// ---------------------------------------------------------------------------
// Emulated media features
// ---------------------------------------------------------------------------
//
// The same question a device width asks, put to a media feature: *what does this
// page look like for someone whose OS says dark, or who has asked for less
// motion?* Unlike the metrics and the user agent, Electron exposes no API for it —
// it is `Emulation.setEmulatedMedia` over the same CDP session touch emulation
// uses, which is why a pane reports whether it is actually in force
// (`mediaActive`) rather than claiming a mode something else may have taken.
//
// Three features rather than one, because they are the same call at the same cost
// and the two extras are the accessibility half of the same job.

/** The features a pane can override, with the values each accepts. */
export const MEDIA_FEATURES = {
  "prefers-color-scheme": ["light", "dark"],
  "prefers-reduced-motion": ["reduce", "no-preference"],
  "forced-colors": ["active", "none"],
} as const;

export type MediaFeature = keyof typeof MEDIA_FEATURES;

/** A pane's overrides. An absent key means "whatever the host reports". */
export type PaneMedia = Partial<Record<MediaFeature, string>>;

/** How each feature and value is worded in the menu. */
export const MEDIA_LABELS: Record<MediaFeature, { title: string; values: Record<string, string> }> =
  {
    "prefers-color-scheme": {
      title: "Colour scheme",
      values: { light: "Light", dark: "Dark" },
    },
    "prefers-reduced-motion": {
      title: "Reduced motion",
      values: { reduce: "Reduce", "no-preference": "No preference" },
    },
    "forced-colors": {
      title: "Forced colours",
      values: { active: "Active", none: "None" },
    },
  };

/** Set or clear one feature, returning `null` when nothing is overridden any more
 *  — one representation for "no overrides", which is what decides whether the
 *  debugger is attached at all. */
export function withMediaFeature(
  media: PaneMedia | null,
  feature: MediaFeature,
  value: string | null,
): PaneMedia | null {
  const next: PaneMedia = { ...(media ?? {}) };
  if (value === null) delete next[feature];
  else next[feature] = value;
  return Object.keys(next).length > 0 ? next : null;
}

/**
 * The colour scheme a quick switch moves to next: System → Dark → Light → System.
 *
 * `null` is System — the *absence* of an override, which is the state that lets the
 * debugger be released, so it has to be reachable by cycling rather than only from
 * the menu. Dark comes first because it is the one people are reaching for; Light is
 * a real destination rather than "off", since a light-only layout bug is as ordinary
 * as a dark one and going to the menu for it is the reach problem this switch exists
 * to fix.
 *
 * Total rather than defensive: an unrecognised value returns System, which is the
 * same answer `"light"` gets. That is a property of the function, **not** a guard
 * against a live case — `sanitizeMedia` below drops any scheme outside
 * [`MEDIA_FEATURES`] on every layout load (`panes/model.ts`), so nothing but
 * `light`, `dark` or absent reaches here. Weakening that sanitiser is what would
 * make this branch matter, so do not read it as permission to.
 */
export function nextColorScheme(current: string | undefined): string | null {
  if (current === undefined) return "dark";
  if (current === "dark") return "light";
  return null;
}

/** What the pane's chrome says is being emulated, or `null` for nothing. */
export function mediaLabel(media: PaneMedia | null): string | null {
  const parts = (Object.keys(MEDIA_FEATURES) as MediaFeature[])
    .filter((feature) => media?.[feature])
    .map((feature) => MEDIA_LABELS[feature].values[media![feature]!] ?? media![feature]!);
  return parts.length > 0 ? parts.join(" · ") : null;
}

/**
 * Accept a stored media override, or reject it.
 *
 * Same rule as `sanitizeEmulation`: storage is where a stale build's — or a
 * hand-edited — value sits waiting to be handed to a view on restore. Unknown
 * features and values are dropped individually rather than failing the set, since
 * one bad key should not cost a perfectly good colour-scheme override.
 */
export function sanitizeMedia(raw: unknown): PaneMedia | null {
  if (typeof raw !== "object" || raw === null) return null;
  const source = raw as Record<string, unknown>;
  const out: PaneMedia = {};
  for (const [feature, allowed] of Object.entries(MEDIA_FEATURES)) {
    const value = source[feature];
    if (typeof value === "string" && (allowed as readonly string[]).includes(value)) {
      out[feature as MediaFeature] = value;
    }
  }
  return Object.keys(out).length > 0 ? out : null;
}
