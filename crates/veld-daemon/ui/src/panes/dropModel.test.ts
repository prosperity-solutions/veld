import { describe, expect, it } from "vitest";
import { type DropZone, droppedOutside, edgeWidth, sameZone, zoneAt } from "./dropModel";

/** A pane area 1000px wide starting at x=100. At that width the 96px cap binds,
 *  so its edge zones are 100..196 and 1004..1100. */
const area = { left: 100, right: 1100, width: 1000 };

describe("edgeWidth", () => {
  it("is proportional only between its two bounds", () => {
    // 16% is between the floor and the cap for widths of 175..600 only.
    expect(edgeWidth(500)).toBe(80);
    expect(edgeWidth(300)).toBe(48);
    // Past 600 the cap binds, which is the case a maximised window is in.
    expect(edgeWidth(1000)).toBe(96);
  });

  it("floors, so a narrow pane still has an edge to aim at", () => {
    // At 16% a 200px area would give a 32px edge and a 100px one 16px — too
    // small to hit with a drag.
    expect(edgeWidth(100)).toBe(28);
    expect(edgeWidth(0)).toBe(0);
  });

  it("caps, so a wide window is not a third split zone", () => {
    expect(edgeWidth(2000)).toBe(96);
    expect(edgeWidth(6000)).toBe(96);
  });

  it("never returns a zone for a degenerate area", () => {
    // A dock measured mid-layout can report 0; an edge of 28px against a width
    // of 0 would make every drop a "left" split.
    for (const bad of [0, -50, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(edgeWidth(bad)).toBe(0);
    }
  });
});

describe("zoneAt", () => {
  it("reads the edges off the pane area, not the dock under the cursor", () => {
    // The whole point: the same x means the same thing whichever dock is there.
    expect(zoneAt(area, 150, 0)).toEqual({ where: "left" });
    expect(zoneAt(area, 150, 1)).toEqual({ where: "left" });
    expect(zoneAt(area, 1050, 0)).toEqual({ where: "right" });
    expect(zoneAt(area, 1050, 1)).toEqual({ where: "right" });
  });

  it("is 'into' everywhere between the edges, carrying the hovered dock", () => {
    expect(zoneAt(area, 600, 0)).toEqual({ where: "into", dock: 0 });
    expect(zoneAt(area, 600, 1)).toEqual({ where: "into", dock: 1 });
  });

  it("puts the boundary itself in the edge zone", () => {
    // Inclusive on purpose: the pixel where the highlight appears must be one
    // the drop honours, or the preview and the result disagree by one pixel.
    expect(zoneAt(area, 196, 0)).toEqual({ where: "left" });
    expect(zoneAt(area, 197, 0)).toEqual({ where: "into", dock: 0 });
    expect(zoneAt(area, 1004, 1)).toEqual({ where: "right" });
    expect(zoneAt(area, 1003, 1)).toEqual({ where: "into", dock: 1 });
  });

  it("resolves a pointer outside the area to the nearer edge", () => {
    expect(zoneAt(area, -20, 0)).toEqual({ where: "left" });
    expect(zoneAt(area, 5000, 1)).toEqual({ where: "right" });
  });

  it("prefers left when the zones would overlap in a tiny area", () => {
    // 28px floor on each side of a 40px area: the two zones overlap, and the
    // order of the checks is what decides. Left is the one that also exists in
    // the one-dock case, so it is the safer answer to make deterministic.
    const tiny = { left: 0, right: 40, width: 40 };
    expect(zoneAt(tiny, 20, 0)).toEqual({ where: "left" });
  });
});

describe("sameZone", () => {
  it("distinguishes the two docks of an 'into'", () => {
    const a: DropZone = { where: "into", dock: 0 };
    const b: DropZone = { where: "into", dock: 1 };
    expect(sameZone(a, a)).toBe(true);
    expect(sameZone(a, { where: "into", dock: 0 })).toBe(true);
    expect(sameZone(a, b)).toBe(false);
  });

  it("distinguishes the edges from each other and from null", () => {
    expect(sameZone({ where: "left" }, { where: "left" })).toBe(true);
    expect(sameZone({ where: "left" }, { where: "right" })).toBe(false);
    expect(sameZone(null, null)).toBe(true);
    expect(sameZone(null, { where: "left" })).toBe(false);
    expect(sameZone({ where: "left" }, null)).toBe(false);
  });
});

describe("droppedOutside", () => {
  const box = { screenX: 100, screenY: 100, outerWidth: 800, outerHeight: 600 };

  it("requires the drop to have been refused", () => {
    // A successful drop on one of our own targets is never a detach, however
    // far the pointer travelled.
    expect(droppedOutside(box, { screenX: 5000, screenY: 5000 }, "move")).toBe(false);
    expect(droppedOutside(box, { screenX: 5000, screenY: 5000 }, "copy")).toBe(false);
  });

  it("does not detach on a fumbled drop inside the window", () => {
    // The reason the coordinate test exists at all: `dropEffect === "none"` also
    // describes releasing a tab over a pane's dead space, and detaching there
    // would open a window on every mis-drag.
    expect(droppedOutside(box, { screenX: 400, screenY: 300 }, "none")).toBe(false);
  });

  it("detaches past any edge", () => {
    expect(droppedOutside(box, { screenX: 99, screenY: 300 }, "none")).toBe(true);
    expect(droppedOutside(box, { screenX: 400, screenY: 99 }, "none")).toBe(true);
    expect(droppedOutside(box, { screenX: 901, screenY: 300 }, "none")).toBe(true);
    expect(droppedOutside(box, { screenX: 400, screenY: 701 }, "none")).toBe(true);
  });

  it("treats the window's own border as inside", () => {
    expect(droppedOutside(box, { screenX: 100, screenY: 100 }, "none")).toBe(false);
    expect(droppedOutside(box, { screenX: 900, screenY: 700 }, "none")).toBe(false);
  });

  it("ignores the 0,0 report some platforms give for a menu bar", () => {
    expect(droppedOutside(box, { screenX: 0, screenY: 0 }, "none")).toBe(false);
  });

  it("works for a window on a display left of the primary one", () => {
    // Negative screen coordinates are ordinary on a multi-monitor desk, which
    // is the setup this whole feature is for.
    const left = { screenX: -1800, screenY: 0, outerWidth: 800, outerHeight: 600 };
    expect(droppedOutside(left, { screenX: -1400, screenY: 300 }, "none")).toBe(false);
    expect(droppedOutside(left, { screenX: 200, screenY: 300 }, "none")).toBe(true);
  });
});
