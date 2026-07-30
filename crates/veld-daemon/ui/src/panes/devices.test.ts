import { describe, expect, it } from "vitest";
import {
  CUSTOM_DEVICE,
  DEVICE_GROUPS,
  DEVICE_PRESETS,
  MAX_DEVICE_PX,
  MIN_DEVICE_PX,
  ZOOM_STEPS,
  chromeVersionFrom,
  clampZoom,
  customEmulation,
  emulationForPreset,
  emulationLabel,
  emulationSize,
  fitScale,
  formatPercent,
  formatZoom,
  isLandscape,
  presetById,
  resolveUserAgent,
  rotateEmulation,
  safeUserAgentText,
  sanitizeEmulation,
  sanitizeZoom,
  zoomStep,
} from "./devices";

const phone = () => emulationForPreset(presetById("iphone-pro")!, { chrome: "143.0.0.0" });
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
      // A UA is a header value — the same rule the shell enforces.
      if (preset.ua !== null) {
        expect(safeUserAgentText(resolveUserAgent(preset.ua, "143.0.0.0"))).not.toBeNull();
      }
    }
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
    expect(e).toMatchObject({ device: "iphone-pro", width: 393, height: 852, touch: true });
    expect(e.fit).toBe(true);
    const pixel = emulationForPreset(presetById("pixel")!, { chrome: "143.0.0.0" });
    expect(pixel.ua).toContain("Chrome/143.0.0.0 Mobile");
    expect(pixel.ua).not.toContain("{chrome}");
  });

  it("opens landscape when asked, keeping the preset's identity", () => {
    const e = emulationForPreset(presetById("iphone-pro")!, { landscape: true });
    expect([e.width, e.height]).toEqual([852, 393]);
    expect(e.device).toBe("iphone-pro");
    expect(emulationLabel(e)).toBe("iPhone Pro · landscape");
  });
});

describe("rotate and label", () => {
  it("swaps the axes and says which way round it is", () => {
    const e = phone();
    expect(emulationLabel(e)).toBe("iPhone Pro");
    const sideways = rotateEmulation(e);
    expect([sideways.width, sideways.height]).toEqual([852, 393]);
    expect(isLandscape(sideways)).toBe(true);
    expect(emulationLabel(sideways)).toBe("iPhone Pro · landscape");
    // A screen preset is landscape *already*, so rotating one is what reads as
    // "· landscape" turning off rather than on.
    expect(emulationLabel(desktop())).toBe("Desktop");
    expect(emulationLabel(rotateEmulation(desktop()))).toBe("Desktop · landscape");
    // Twice is a round trip.
    expect(rotateEmulation(sideways)).toEqual(e);
  });

  it("calls an emulation with no matching preset what it is", () => {
    // A preset that has since been renamed or removed. The metrics are still
    // valid, so the emulation survives — it just isn't that device any more.
    expect(emulationLabel({ ...phone(), device: "iphone-42" })).toBe("Custom");
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
    });
    expect(customEmulation(Number.NaN, 800).width).toBe(MIN_DEVICE_PX);
  });
});

describe("fitScale", () => {
  it("is the reason emulation belongs in a pane at all", () => {
    // A 1440-wide layout in a 600px pane: the case a real browser window cannot
    // give you without a second monitor.
    expect(fitScale(desktop(), { width: 720, height: 900 })).toBe(0.5);
    // Height binds too — the emulated screen *is* the pane, so a viewport scaled
    // to the width but taller than the box is clipped with nothing to scroll it.
    expect(fitScale(desktop(), { width: 1440, height: 450 })).toBe(0.5);
    // Never magnified: a phone in a wide pane stays phone-sized.
    expect(fitScale(phone(), { width: 1200, height: 1200 })).toBe(1);
  });

  it("is 1 with fitting off, or with nothing to measure", () => {
    expect(fitScale({ ...desktop(), fit: false }, { width: 100, height: 100 })).toBe(1);
    // A container mid-layout reports zero, and a pane that has not been laid out
    // yet must not scale to nothing.
    expect(fitScale(desktop(), { width: 0, height: 0 })).toBe(1);
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
      device: "iphone-pro",
      width: 99999,
      height: -3,
      deviceScaleFactor: 2.625,
      mobile: "yes",
      touch: 1,
      ua: "UA/1.0\r\nX-Injected: 1",
      fit: false,
    });
    expect(stored).toEqual({
      device: "iphone-pro",
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
    });
  });

  it("degrades an unusable emulation to none at all", () => {
    // "No emulation" is a correct state — the pane at pane size — while a pane
    // sized from `NaN` is not.
    for (const junk of [null, undefined, 42, "iphone-pro", [], {}, { width: 400 }]) {
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
