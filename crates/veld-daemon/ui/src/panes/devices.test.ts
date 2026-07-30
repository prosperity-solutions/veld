import { describe, expect, it } from "vitest";
import {
  type DevicePreset,
  CUSTOM_DEVICE,
  CUSTOM_RADIUS,
  DEVICE_GROUPS,
  DEVICE_PADDING,
  DEVICE_PRESETS,
  HANDLE_CORNER_GAP,
  HANDLE_CORNER_SIZE,
  HANDLE_EDGE_GAP,
  HANDLE_THICKNESS,
  MAX_DEVICE_PX,
  MAX_DEVICE_RADIUS,
  MIN_DEVICE_PADDING,
  MIN_DEVICE_PX,
  RESPONSIVE_DEVICE,
  ZOOM_STEPS,
  chromeVersionFrom,
  clampZoom,
  customEmulation,
  deviceLayout,
  dragSize,
  edgePinned,
  emulationForPreset,
  emulationLabel,
  emulationSize,
  formatPercent,
  formatZoom,
  isLandscape,
  orientationLabel,
  presetById,
  resizeEmulation,
  resolveUserAgent,
  responsiveEmulation,
  rotateEmulation,
  safeUserAgentText,
  sanitizeEmulation,
  sanitizeZoom,
  scaledRadius,
  withMobileUserAgent,
  zoomStep,
} from "./devices";

const phone = () => emulationForPreset(presetById("phone")!, { chrome: "143.0.0.0" });
const desktop = () => emulationForPreset(presetById("desktop")!, {});

describe("the preset table", () => {
  it("is internally consistent", () => {
    const ids = DEVICE_PRESETS.map((p) => p.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const preset of DEVICE_PRESETS) {
      // Every preset must be reachable from the menu, which is built by grouping.
      expect(DEVICE_GROUPS).toContain(preset.group);
      // Sizes inside the bounds the shell will clamp to, or picking a preset
      // would silently produce something other than what the menu says.
      expect(preset.width).toBeGreaterThanOrEqual(MIN_DEVICE_PX);
      expect(preset.height).toBeLessThanOrEqual(MAX_DEVICE_PX);
      // Electron types `deviceScaleFactor` Integer.
      expect(Number.isInteger(preset.deviceScaleFactor)).toBe(true);
      // A shape, within what the shell will apply.
      expect(preset.radius).toBeGreaterThanOrEqual(0);
      expect(preset.radius).toBeLessThanOrEqual(MAX_DEVICE_RADIUS);
      // A UA is a header value — the same rule the shell enforces.
      if (preset.ua !== null) {
        expect(safeUserAgentText(resolveUserAgent(preset.ua, "143.0.0.0"))).not.toBeNull();
      }
    }
  });

  it("gives a phone a rounder screen than a monitor", () => {
    // The shape is how an emulated pane reads as a *device* at a glance, so the
    // ordering is the point rather than the exact numbers.
    const radiusOf = (group: DevicePreset["group"]) =>
      DEVICE_PRESETS.filter((p) => p.group === group).map((p) => p.radius);
    expect(Math.min(...radiusOf("Phones"))).toBeGreaterThan(Math.max(...radiusOf("Tablets")));
    expect(Math.min(...radiusOf("Tablets"))).toBeGreaterThan(Math.max(...radiusOf("Screens")));
  });

  it("keeps the screen presets as desktop, UA and all", () => {
    // Emulating a laptop is a layout question, and the shell already is a desktop
    // browser: a second desktop UA would only be a string that can be wrong.
    for (const preset of DEVICE_PRESETS.filter((p) => p.group === "Screens")) {
      expect(preset.ua).toBeNull();
      expect(preset.touch).toBe(false);
      expect(preset.mobile).toBe(false);
    }
    // ...and every phone the other way round, or picking one would lay out narrow
    // and still be handed the desktop bundle.
    for (const preset of DEVICE_PRESETS.filter((p) => p.group === "Phones")) {
      expect(preset.ua).not.toBeNull();
      expect(preset.touch).toBe(true);
      expect(preset.mobile).toBe(true);
    }
  });
});

describe("emulationForPreset", () => {
  it("carries the preset's metrics and resolves its user agent", () => {
    const e = phone();
    expect(e).toMatchObject({ device: "phone", width: 402, height: 874, touch: true });
    expect(e.fit).toBe(true);
    // The mobile classes carry the host Chromium's version, so a UA never claims a
    // release older than the engine sending the request.
    expect(e.ua).toContain("Chrome/143.0.0.0 Mobile");
    expect(e.ua).not.toContain("{chrome}");
    // A tablet's UA drops `Mobile`, which is exactly how a server tells the two
    // classes apart.
    const tablet = emulationForPreset(presetById("tablet")!, { chrome: "143.0.0.0" });
    expect(tablet.ua).toContain("Chrome/143.0.0.0 Safari");
    expect(tablet.ua).not.toContain("Mobile");
  });

  it("opens landscape when asked, keeping the preset's identity", () => {
    const e = emulationForPreset(presetById("phone")!, { landscape: true });
    expect([e.width, e.height]).toEqual([874, 402]);
    expect(e.device).toBe("phone");
    expect(emulationLabel(e)).toBe("Phone · landscape");
  });
});

describe("rotate and label", () => {
  it("swaps the axes and says which way round it is", () => {
    const e = phone();
    expect(emulationLabel(e)).toBe("Phone");
    const sideways = rotateEmulation(e);
    expect([sideways.width, sideways.height]).toEqual([874, 402]);
    expect(isLandscape(sideways)).toBe(true);
    expect(emulationLabel(sideways)).toBe("Phone · landscape");
    // A screen preset is landscape *already*, so rotating one is what reads as
    // "· landscape" turning off rather than on.
    expect(emulationLabel(desktop())).toBe('Desktop 24"');
    expect(emulationLabel(rotateEmulation(desktop()))).toBe("Desktop 24\" · landscape");
    // Twice is a round trip.
    expect(rotateEmulation(sideways)).toEqual(e);
    // The state the Rotate control is in, which is otherwise only inferable by
    // reading the two numbers off the chip.
    expect(orientationLabel(e)).toBe("Portrait");
    expect(orientationLabel(sideways)).toBe("Landscape");
  });

  it("calls an emulation with no matching preset what it is", () => {
    // A preset that has since been renamed or removed. The metrics are still
    // valid, so the emulation survives — it just isn't that device any more.
    expect(emulationLabel({ ...phone(), device: "phone-42" })).toBe("Custom");
    expect(emulationLabel(customEmulation(500, 900))).toBe("Custom");
    expect(emulationSize(customEmulation(500, 900))).toBe("500 × 900");
  });
});

describe("customEmulation", () => {
  it("keeps the device flags of whatever is set now", () => {
    // The useful reading of "custom size": this phone, but narrower — not a
    // desktop viewport that happens to be phone-sized.
    const narrow = customEmulation(360, 800, phone());
    expect(narrow).toMatchObject({
      device: CUSTOM_DEVICE,
      width: 360,
      height: 800,
      touch: true,
      mobile: true,
      deviceScaleFactor: 3,
    });
    expect(narrow.ua).toBe(phone().ua);
  });

  it("clamps, and defaults to a plain desktop viewport with no base", () => {
    expect(customEmulation(1, 99999).width).toBe(MIN_DEVICE_PX);
    expect(customEmulation(1, 99999).height).toBe(MAX_DEVICE_PX);
    expect(customEmulation(400.6, 800.4).width).toBe(401);
    expect(customEmulation(400, 800)).toMatchObject({
      touch: false,
      mobile: false,
      ua: null,
      deviceScaleFactor: 0,
      fit: true,
      // Barely rounded: nobody said this viewport was a phone.
      radius: CUSTOM_RADIUS,
    });
    // ...but a size derived *from* a phone keeps the phone's shape.
    expect(customEmulation(360, 800, phone()).radius).toBe(phone().radius);
    expect(customEmulation(Number.NaN, 800).width).toBe(MIN_DEVICE_PX);
  });
});

describe("the resizable viewport", () => {
  it("starts as the pane, with nothing claimed about the device", () => {
    // "The pane, but with a number on it and edges you can drag." Claiming touch,
    // a DPR or a mobile UA would make turning it on change how the page behaves
    // rather than only how wide it is.
    const r = responsiveEmulation(900, 600);
    expect(r).toEqual({
      device: RESPONSIVE_DEVICE,
      width: 900,
      height: 600,
      deviceScaleFactor: 0,
      mobile: false,
      touch: false,
      ua: null,
      fit: true,
      radius: CUSTOM_RADIUS,
    });
    expect(emulationLabel(r)).toBe("Responsive");
    // Clamped like any other size, so a drag past the bounds cannot store one.
    expect(responsiveEmulation(1, 99999)).toMatchObject({
      width: MIN_DEVICE_PX,
      height: MAX_DEVICE_PX,
    });
  });

  it("survives a restore, unlike an unknown preset id", () => {
    // The one id that is not in the preset table and still means something, so
    // `sanitizeEmulation` has to know it — otherwise dragging a responsive pane and
    // reloading would silently demote it to "Custom".
    const restored = sanitizeEmulation(JSON.parse(JSON.stringify(responsiveEmulation(900, 600))));
    expect(restored?.device).toBe(RESPONSIVE_DEVICE);
    expect(sanitizeEmulation({ device: "made-up", width: 400, height: 800 })?.device).toBe(
      CUSTOM_DEVICE,
    );
  });

  it("keeps a dragged device's flags but not its identity", () => {
    // A phone dragged narrower is still a phone — touch, UA, shape — it is just no
    // longer the class the menu named.
    const dragged = resizeEmulation(phone(), 340, 700);
    expect(dragged).toMatchObject({
      device: CUSTOM_DEVICE,
      width: 340,
      height: 700,
      touch: true,
      mobile: true,
      radius: phone().radius,
    });
    expect(dragged.ua).toBe(phone().ua);
    // ...while the responsive viewport stays itself, because that is a mode rather
    // than a device.
    expect(resizeEmulation(responsiveEmulation(900, 600), 500, 400).device).toBe(RESPONSIVE_DEVICE);
    expect(resizeEmulation(phone(), 1, 99999)).toMatchObject({
      width: MIN_DEVICE_PX,
      height: MAX_DEVICE_PX,
    });
  });

  it("can be given a mobile user agent, and have it taken away", () => {
    // The reason this is separate from the size: a responsive or custom viewport has
    // no preset to inherit a UA from, and "does my app serve the mobile bundle at
    // this width" is a different question from "does my layout survive it".
    const narrow = withMobileUserAgent(responsiveEmulation(420, 800), true, "143.0.0.0");
    expect(narrow.ua).toContain("Mobile Safari");
    // Above the phone widths it sends the tablet string, since dropping `Mobile` is
    // how a server tells the two apart.
    const wide = withMobileUserAgent(responsiveEmulation(900, 1200), true, "143.0.0.0");
    expect(wide.ua).not.toContain("Mobile");
    expect(withMobileUserAgent(narrow, false).ua).toBeNull();
    // ...and *only* the user agent, in both directions. Clearing `mobile` here — the
    // Chromium screen position the preset owns — turned a Phone into a desktop-layout
    // viewport when you unticked a menu item that said "user agent", with nothing able
    // to put it back.
    const phoneNoUa = withMobileUserAgent(phone(), false);
    expect(phoneNoUa.mobile).toBe(true);
    expect(phoneNoUa.ua).toBeNull();
    expect(withMobileUserAgent(customEmulation(400, 800), true, "143").mobile).toBe(false);
  });
});

describe("the handles fit the gap", () => {
  it("keeps the resize handles clear of the window's own edge", () => {
    // The gap exists to hold the handles, and at 14px the corner one sat inside the
    // OS window-resize zone — reaching for it dragged the whole app. This is the guard
    // that stops the next person shrinking the gap (it is already a `deviceLayout`
    // parameter) and reopening that silently.
    expect(DEVICE_PADDING).toBeGreaterThanOrEqual(MIN_DEVICE_PADDING);
    expect(HANDLE_EDGE_GAP + HANDLE_THICKNESS).toBeLessThan(DEVICE_PADDING);
    expect(HANDLE_CORNER_GAP + HANDLE_CORNER_SIZE).toBeLessThan(DEVICE_PADDING);
  });
});

describe("the wire shape the shell validates", () => {
  it("has exactly these fields, so the shell's validator cannot drift", () => {
    // `safeEmulation` in `desktop/src/validate.js` whitelists the fields it forwards to
    // Electron, by hand, in plain JS with no type to check it against. So a field added
    // here passes the typechecker, passes every test, and is then *silently dropped* at
    // the trust boundary — the feature simply never reaches the desktop app while
    // working in a browser tab, which is this codebase's worst failure shape.
    //
    // This list is the drift gate. If it fails, you added or renamed a field: update
    // `safeEmulation` (and its test) in the same breath, then update this list.
    expect(Object.keys(phone()).sort()).toEqual([
      "device",
      "deviceScaleFactor",
      "fit",
      "height",
      "mobile",
      "radius",
      "touch",
      "ua",
      "width",
    ]);
  });
});

describe("dragSize", () => {
  const from = { width: 400, height: 800 };

  it("moves the edge with the cursor while the screen has room to grow", () => {
    // Scale *and* centre-growth: at 50% a 100px pull is 200 viewport pixels, and the
    // edge only travels half of what the size changes by, so it lands under the
    // pointer.
    expect(dragSize(from, { x: 100, y: 0 }, "x", 0.5)).toEqual({ width: 800, height: 800 });
    expect(dragSize(from, { x: 0, y: 50 }, "y", 1)).toEqual({ width: 400, height: 900 });
    expect(dragSize(from, { x: 25, y: 25 }, "both", 1)).toEqual({ width: 450, height: 850 });
    // The axis you are not dragging does not move.
    expect(dragSize(from, { x: 100, y: 100 }, "x", 1).height).toBe(800);
    expect(dragSize(from, { x: 100, y: 100 }, "y", 1).width).toBe(400);
  });

  it("stops doubling once the dragged axis is clamped to the pane", () => {
    // With the screen already filling the pane there is no edge left to track, so the
    // doubling would only double how fast the number runs away from the pointer — at
    // 30% that was nearly 7 device pixels per pixel of travel, in exactly the
    // big-screen-in-a-small-pane case this feature is for.
    const clamped = { width: true, height: false };
    expect(dragSize(from, { x: 100, y: 0 }, "x", 0.5, clamped).width).toBe(600);
    // The unclamped axis of the same gesture still doubles.
    expect(dragSize(from, { x: 0, y: 100 }, "both", 0.5, clamped).height).toBe(1200);
  });

  it("clamps at both ends and survives a nonsense scale", () => {
    expect(dragSize(from, { x: -9999, y: -9999 }, "both", 1)).toEqual({
      width: MIN_DEVICE_PX,
      height: MIN_DEVICE_PX,
    });
    expect(dragSize(from, { x: 99999, y: 99999 }, "both", 1)).toEqual({
      width: MAX_DEVICE_PX,
      height: MAX_DEVICE_PX,
    });
    // A scale of 0 would divide the pointer into infinity; 1 is the only safe reading
    // of "we do not know how far this is scaled".
    expect(dragSize(from, { x: 10, y: 0 }, "x", 0)).toEqual({ width: 420, height: 800 });
    expect(dragSize(from, { x: 10, y: 0 }, "x", Number.NaN).width).toBe(420);
  });
});

describe("edgePinned", () => {
  const pane = (width: number, height: number) => ({
    width: width + DEVICE_PADDING * 2,
    height: height + DEVICE_PADDING * 2,
  });

  it("pins the axis that fitting binds on — the case the doubling must not apply to", () => {
    // A 1920x1080 screen in a 600-wide pane: width binds, so the drawn width *is* the
    // available width whatever the emulated number does. The predicate this replaced
    // compared the drawn box against `size * scale`, which `deviceLayout` makes exactly
    // equal on the binding axis — so it could never fire, and the doubling stayed on in
    // the one case it was written to switch off.
    expect(edgePinned(desktop(), pane(600, 900))).toEqual({ width: true, height: false });
    // Short pane, same device: height binds instead.
    expect(edgePinned(desktop(), pane(1920, 300))).toEqual({ width: false, height: true });
  });

  it("pins nothing when the screen fits", () => {
    expect(edgePinned(phone(), pane(1200, 1200))).toEqual({ width: false, height: false });
  });

  it("pins both axes of an unfitted oversized screen, which is cropped", () => {
    expect(edgePinned({ ...desktop(), fit: false }, pane(400, 300))).toEqual({
      width: true,
      height: true,
    });
  });

  it("pins nothing in a pane too small to lay out", () => {
    expect(edgePinned(phone(), { width: 4, height: 4 })).toEqual({ width: false, height: false });
  });
});

describe("deviceLayout", () => {
  // The pane's box, plus the inset on both sides, so a test can say "a pane with
  // exactly this much room for a screen".
  const pane = (width: number, height: number) => ({
    width: width + DEVICE_PADDING * 2,
    height: height + DEVICE_PADDING * 2,
  });

  it("is the reason emulation belongs in a pane at all", () => {
    // A 1440-wide layout in a 720px-wide pane: the case a real browser window
    // cannot give you without a second monitor.
    const l = deviceLayout(desktop(), pane(960, 900));
    expect(l.scale).toBe(0.5);
    expect(l.width).toBe(960);
    expect(l.height).toBe(540);
    // Height binds too — the emulated screen *is* the pane, so a viewport scaled
    // to the width but taller than the box is clipped with nothing to scroll it.
    expect(deviceLayout(desktop(), pane(1920, 540)).scale).toBe(0.5);
    // Never magnified: a phone in a wide pane stays phone-sized.
    expect(deviceLayout(phone(), pane(1200, 1200)).scale).toBe(1);
  });

  it("centres the screen and always leaves the inset", () => {
    // The gap is what makes "a device inside a pane" readable rather than the pane
    // simply being that page, and the centring is what makes it look placed.
    const l = deviceLayout(phone(), pane(1000, 1000));
    expect(l.x).toBe(DEVICE_PADDING + (1000 - 402) / 2);
    expect(l.y).toBe(DEVICE_PADDING + (1000 - 874) / 2);
    // A screen scaled to exactly fit is inset on all four sides, never flush.
    const tight = deviceLayout(desktop(), pane(960, 540));
    expect(tight.x).toBe(DEVICE_PADDING);
    expect(tight.y).toBe(DEVICE_PADDING);
    expect(tight.width).toBe(960);
    expect(tight.height).toBe(540);
  });

  it("crops an unfitted oversized device instead of overflowing the pane", () => {
    // Under Electron the view is a native sibling that the pane's box does not
    // clip, so a rect larger than the pane would paint over its neighbour. And the
    // part of a page worth showing is the top of it, so this anchors rather than
    // centres.
    const l = deviceLayout({ ...desktop(), fit: false }, pane(400, 300));
    expect(l.scale).toBe(1);
    expect(l.width).toBe(400);
    expect(l.height).toBe(300);
    expect(l.x).toBe(DEVICE_PADDING);
    expect(l.y).toBe(DEVICE_PADDING);
    // Unfitted but small enough: no cropping, and centred like any other.
    const fits = deviceLayout({ ...phone(), fit: false }, pane(1000, 1000));
    expect(fits.width).toBe(402);
    expect(fits.x).toBe(DEVICE_PADDING + (1000 - 402) / 2);
  });

  it("fills a pane too small to inset rather than computing a negative box", () => {
    // A container mid-layout, or a dock dragged to a sliver.
    const l = deviceLayout(desktop(), { width: 10, height: 10 });
    expect(l).toEqual({ x: 0, y: 0, width: 10, height: 10, scale: 1 });
    expect(deviceLayout(desktop(), { width: 0, height: 0 })).toEqual({
      x: 0,
      y: 0,
      width: 0,
      height: 0,
      scale: 1,
    });
  });

  it("scales the corner radius with the screen", () => {
    // A phone at 40% that kept a 48px radius would read as a rounded rectangle
    // someone forgot to scale.
    expect(scaledRadius(phone(), 1)).toBe(44);
    expect(scaledRadius(phone(), 0.5)).toBe(22);
    expect(scaledRadius(desktop(), 1)).toBe(8);
    // Bounded and non-negative whatever storage held.
    expect(scaledRadius({ ...phone(), radius: 9999 }, 1)).toBe(MAX_DEVICE_RADIUS);
    expect(scaledRadius({ ...phone(), radius: -5 }, 1)).toBe(0);
  });
});

describe("user agents", () => {
  it("refuses anything that could not be a header value", () => {
    expect(safeUserAgentText("Mozilla/5.0 (X)")).toBe("Mozilla/5.0 (X)");
    expect(safeUserAgentText("  padded/1.0 ")).toBe("padded/1.0");
    // The shell enforces the same rule (`safeUserAgent` in
    // desktop/src/validate.js); this copy is what stops a hand-edited
    // sessionStorage restoring a pane that then fails there with nothing to show.
    expect(safeUserAgentText("UA/1.0\r\nX-Injected: 1")).toBeNull();
    expect(safeUserAgentText("UA/1.0\nX: 1")).toBeNull();
    expect(safeUserAgentText("UA/1.0 ünïcode")).toBeNull();
    expect(safeUserAgentText("a".repeat(513))).toBeNull();
    for (const junk of ["", "   ", null, undefined, 42, {}]) {
      expect(safeUserAgentText(junk)).toBeNull();
    }
  });

  it("fills {chrome} from the host, and drops the claim when it cannot", () => {
    expect(resolveUserAgent("A Chrome/{chrome} Mobile", "143.0.0.0")).toBe(
      "A Chrome/143.0.0.0 Mobile",
    );
    // No version to hand: a UA naming no Chrome release is one a server may not
    // recognise, but a UA containing literal braces is one no server has seen.
    expect(resolveUserAgent("A Chrome/{chrome} Mobile", undefined)).toBe("A Mobile");
    expect(resolveUserAgent("A Chrome/{chrome} Mobile", "not-a-version")).toBe("A Mobile");
    expect(resolveUserAgent("no placeholder", "143")).toBe("no placeholder");
  });

  it("reads the host Chromium version out of a real UA", () => {
    expect(
      chromeVersionFrom(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) veld/1.0 Chrome/143.0.0.0 Electron/43.1.1 Safari/537.36",
      ),
    ).toBe("143.0.0.0");
    expect(chromeVersionFrom("Mozilla/5.0 (iPhone) Safari/604.1")).toBeUndefined();
    expect(chromeVersionFrom(undefined)).toBeUndefined();
  });
});

describe("zoom", () => {
  it("steps along the ladder and stops at both ends", () => {
    expect(zoomStep(1, 1)).toBe(1.1);
    expect(zoomStep(1, -1)).toBe(0.9);
    expect(zoomStep(ZOOM_STEPS[ZOOM_STEPS.length - 1], 1)).toBe(3);
    expect(zoomStep(ZOOM_STEPS[0], -1)).toBe(0.25);
    // A factor that is not on the ladder — restored from storage, or set before
    // the ladder changed — still has to step somewhere sensible.
    expect(zoomStep(0.85, 1)).toBe(0.9);
    expect(zoomStep(0.85, -1)).toBe(0.8);
    // Floating-point noise is the same step, not the next one.
    expect(zoomStep(0.67000000000001, 1)).toBe(0.75);
  });

  it("clamps and formats", () => {
    expect(clampZoom(99)).toBe(3);
    expect(clampZoom(0)).toBe(1);
    expect(clampZoom(Number.NaN)).toBe(1);
    expect(formatZoom(0.6700000000000001)).toBe("67%");
    // The *fit* scale is not a zoom: a viewport shrunk to 12% of a narrow pane is
    // a real 12%, and clamping it to the zoom floor would report 25%.
    expect(formatPercent(0.12)).toBe("12%");
    expect(formatZoom(0.12)).toBe("25%");
  });
});

describe("restore", () => {
  it("round-trips what this build writes", () => {
    const e = phone();
    expect(sanitizeEmulation(JSON.parse(JSON.stringify(e)))).toEqual(e);
    expect(sanitizeZoom(1.25)).toBe(1.25);
  });

  it("clamps a stale or hand-edited emulation instead of trusting it", () => {
    const stored = sanitizeEmulation({
      device: "phone",
      width: 99999,
      height: -3,
      deviceScaleFactor: 2.625,
      mobile: "yes",
      touch: 1,
      ua: "UA/1.0\r\nX-Injected: 1",
      fit: false,
      radius: 9999,
    });
    expect(stored).toEqual({
      device: "phone",
      width: MAX_DEVICE_PX,
      height: MIN_DEVICE_PX,
      deviceScaleFactor: 3,
      // Only a literal `true` is true: a stored `"yes"` is data of an unknown
      // shape, not a flag.
      mobile: false,
      touch: false,
      // A hostile UA drops the UA, not the emulation — the size is still fine.
      ua: null,
      fit: false,
      radius: MAX_DEVICE_RADIUS,
    });
  });

  it("degrades an unusable emulation to none at all", () => {
    // "No emulation" is a correct state — the pane at pane size — while a pane
    // sized from `NaN` is not.
    for (const junk of [null, undefined, 42, "phone", [], {}, { width: 400 }]) {
      expect(sanitizeEmulation(junk)).toBeNull();
    }
    // An unknown preset id keeps its metrics and becomes Custom, rather than
    // throwing the size away with the name.
    expect(sanitizeEmulation({ device: "nokia-3310", width: 400, height: 800 })?.device).toBe(
      CUSTOM_DEVICE,
    );
    // `fit` defaults on: an emulation written before the toggle existed must
    // still fit, or a 1920-wide device is unusable in a pane.
    expect(sanitizeEmulation({ width: 400, height: 800 })?.fit).toBe(true);
    // No shape recorded means a square-ish screen, which is what an emulation
    // written before devices had one was being shown as anyway.
    expect(sanitizeEmulation({ width: 400, height: 800 })?.radius).toBe(0);
  });

  it("treats 100% zoom as nothing to store", () => {
    expect(sanitizeZoom(1)).toBeNull();
    expect(sanitizeZoom(undefined)).toBeNull();
    expect(sanitizeZoom(0)).toBeNull();
    expect(sanitizeZoom("x")).toBeNull();
    // Out of range is clamped rather than dropped: the intent is legible.
    expect(sanitizeZoom(99)).toBe(3);
    expect(sanitizeZoom(0.01)).toBe(0.25);
  });
});
