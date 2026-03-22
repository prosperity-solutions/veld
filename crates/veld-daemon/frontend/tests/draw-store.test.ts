import { describe, it, expect } from "vitest";
import { createDrawStore, drawReducer, type DrawState, type DrawAction } from "../src/draw-overlay/store";
import type { PinEntry, StrokeDraw, BlurEntry, SpotlightEntry } from "../src/draw-overlay/types";

function makeStroke(overrides?: Partial<StrokeDraw>): StrokeDraw {
  return {
    points: [{ x: 0, y: 0, pressure: 0.5 }, { x: 10, y: 10, pressure: 0.5 }],
    color: "#ef4444",
    baseWidth: 5,
    compositeOp: "source-over",
    hasPressure: false,
    toolMode: "draw",
    ...overrides,
  };
}

function makePin(num: number): PinEntry {
  return { type: "pin", x: 100, y: 100, number: num, color: "#ef4444", angle: 0 };
}

describe("draw store — createDrawStore factory", () => {
  it("creates a store with default state", () => {
    const { getState } = createDrawStore();
    const s = getState();
    expect(s.strokes).toEqual([]);
    expect(s.undoneStrokes).toEqual([]);
    expect(s.toolMode).toBe("draw");
    expect(s.activeColorIdx).toBe(0);
    expect(s.activeWidthIdx).toBe(1);
    expect(s.shapeSnap).toBe(false);
    expect(s.pinCounter).toBe(0);
    expect(s.pendingPin).toBeNull();
    expect(s.drawing).toBe(false);
    expect(s.toolbarCollapsed).toBe(false);
    expect(s.cursorPos).toBeNull();
    expect(Object.isFrozen(s)).toBe(true);
  });
});

describe("draw store — ADD_STROKE, UNDO, REDO", () => {
  it("ADD_STROKE appends and clears undone", () => {
    const { getState, dispatch } = createDrawStore();
    const stroke = makeStroke();
    dispatch({ type: "ADD_STROKE", stroke });
    expect(getState().strokes.length).toBe(1);
    expect(getState().undoneStrokes.length).toBe(0);
  });

  it("UNDO moves last stroke to undone", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "ADD_STROKE", stroke: makeStroke({ color: "#000" }) });
    dispatch({ type: "UNDO" });
    expect(getState().strokes.length).toBe(1);
    expect(getState().undoneStrokes.length).toBe(1);
  });

  it("UNDO on empty strokes is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "UNDO" });
    expect(getState().strokes.length).toBe(0);
    expect(getState().undoneStrokes.length).toBe(0);
  });

  it("UNDO cancels pending pin first", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "SET_PENDING_PIN", pin: makePin(1) });
    dispatch({ type: "UNDO" });
    expect(getState().pendingPin).toBeNull();
    expect(getState().pinCounter).toBe(0);
  });

  it("REDO restores undone stroke", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "UNDO" });
    expect(getState().strokes.length).toBe(0);
    dispatch({ type: "REDO" });
    expect(getState().strokes.length).toBe(1);
    expect(getState().undoneStrokes.length).toBe(0);
  });

  it("REDO on empty undone is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "REDO" });
    expect(getState().strokes.length).toBe(0);
  });

  it("UNDO a pin stroke decrements pinCounter", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "ADD_STROKE", stroke: makePin(1) });
    expect(getState().pinCounter).toBe(1);
    dispatch({ type: "UNDO" });
    expect(getState().pinCounter).toBe(0);
  });

  it("REDO a pin stroke restores pinCounter", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "ADD_STROKE", stroke: makePin(1) });
    dispatch({ type: "UNDO" });
    dispatch({ type: "REDO" });
    expect(getState().pinCounter).toBe(1);
  });
});

describe("draw store — SET_TOOL, SET_COLOR, SET_WIDTH", () => {
  it("SET_TOOL changes tool mode", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_TOOL", tool: "eraser" });
    expect(getState().toolMode).toBe("eraser");
    dispatch({ type: "SET_TOOL", tool: "spotlight" });
    expect(getState().toolMode).toBe("spotlight");
    dispatch({ type: "SET_TOOL", tool: "draw" });
    expect(getState().toolMode).toBe("draw");
  });

  it("SET_COLOR changes active color index", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_COLOR", idx: 2 });
    expect(getState().activeColorIdx).toBe(2);
  });

  it("SET_WIDTH changes active width index", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_WIDTH", idx: 0 });
    expect(getState().activeWidthIdx).toBe(0);
    dispatch({ type: "SET_WIDTH", idx: 2 });
    expect(getState().activeWidthIdx).toBe(2);
  });
});

describe("draw store — pin actions", () => {
  it("PLACE_PIN adds pin to strokes and clears pending", () => {
    const { getState, dispatch } = createDrawStore();
    const pin = makePin(1);
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "SET_PENDING_PIN", pin });
    dispatch({ type: "PLACE_PIN", pin });
    expect(getState().strokes.length).toBe(1);
    expect(getState().pendingPin).toBeNull();
    expect(getState().undoneStrokes.length).toBe(0);
  });

  it("SET_PENDING_PIN sets the pending pin", () => {
    const { getState, dispatch } = createDrawStore();
    const pin = makePin(1);
    dispatch({ type: "SET_PENDING_PIN", pin });
    expect(getState().pendingPin).toEqual(pin);
    dispatch({ type: "SET_PENDING_PIN", pin: null });
    expect(getState().pendingPin).toBeNull();
  });

  it("LOCK_PIN commits pending pin with current angle", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "SET_PENDING_PIN", pin: makePin(1) });
    dispatch({ type: "SET_PENDING_PIN_ANGLE", angle: Math.PI / 4 });
    dispatch({ type: "LOCK_PIN" });
    expect(getState().pendingPin).toBeNull();
    expect(getState().strokes.length).toBe(1);
    const committed = getState().strokes[0] as PinEntry;
    expect(committed.angle).toBeCloseTo(Math.PI / 4);
  });

  it("LOCK_PIN without pending pin is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "LOCK_PIN" });
    expect(getState().strokes.length).toBe(0);
  });

  it("CANCEL_PIN removes pending pin and decrements counter", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "SET_PENDING_PIN", pin: makePin(1) });
    dispatch({ type: "CANCEL_PIN" });
    expect(getState().pendingPin).toBeNull();
    expect(getState().pinCounter).toBe(0);
  });

  it("CANCEL_PIN without pending pin is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "CANCEL_PIN" });
    expect(getState().pinCounter).toBe(1); // unchanged, no pending pin
  });
});

describe("draw store — TOGGLE_SHAPE_SNAP, TOGGLE_COLLAPSE", () => {
  it("TOGGLE_SHAPE_SNAP flips shapeSnap", () => {
    const { getState, dispatch } = createDrawStore();
    expect(getState().shapeSnap).toBe(false);
    dispatch({ type: "TOGGLE_SHAPE_SNAP" });
    expect(getState().shapeSnap).toBe(true);
    dispatch({ type: "TOGGLE_SHAPE_SNAP" });
    expect(getState().shapeSnap).toBe(false);
  });

  it("TOGGLE_COLLAPSE flips toolbarCollapsed", () => {
    const { getState, dispatch } = createDrawStore();
    expect(getState().toolbarCollapsed).toBe(false);
    dispatch({ type: "TOGGLE_COLLAPSE" });
    expect(getState().toolbarCollapsed).toBe(true);
    dispatch({ type: "TOGGLE_COLLAPSE" });
    expect(getState().toolbarCollapsed).toBe(false);
  });
});

describe("draw store — misc actions", () => {
  it("SET_DRAWING toggles drawing flag", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_DRAWING", drawing: true });
    expect(getState().drawing).toBe(true);
    dispatch({ type: "SET_DRAWING", drawing: false });
    expect(getState().drawing).toBe(false);
  });

  it("SET_CURSOR_POS updates cursor position", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_CURSOR_POS", pos: { x: 50, y: 75 } });
    expect(getState().cursorPos).toEqual({ x: 50, y: 75 });
    dispatch({ type: "SET_CURSOR_POS", pos: null });
    expect(getState().cursorPos).toBeNull();
  });

  it("SET_BASE_WIDTH updates base width", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_BASE_WIDTH", width: 10 });
    expect(getState().baseWidth).toBe(10);
  });

  it("SET_HAS_PRESSURE updates pressure flag", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_HAS_PRESSURE", has: true });
    expect(getState().hasPressureDevice).toBe(true);
  });

  it("INCREMENT_PIN_COUNTER bumps counter", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    expect(getState().pinCounter).toBe(2);
  });

  it("SET_CURRENT_STROKE and APPEND_POINT", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_CURRENT_STROKE", stroke: makeStroke() });
    expect(getState().currentStroke).not.toBeNull();
    dispatch({ type: "APPEND_POINT", point: { x: 20, y: 20, pressure: 0.5 } });
    expect(getState().currentStroke!.points.length).toBe(3);
  });

  it("APPEND_POINT without current stroke is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "APPEND_POINT", point: { x: 20, y: 20, pressure: 0.5 } });
    expect(getState().currentStroke).toBeNull();
  });

  it("CLEAR_UNDONE empties undone strokes", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "UNDO" });
    expect(getState().undoneStrokes.length).toBe(1);
    dispatch({ type: "CLEAR_UNDONE" });
    expect(getState().undoneStrokes.length).toBe(0);
  });

  it("SET_RAF_PENDING toggles raf flag", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_RAF_PENDING", pending: true });
    expect(getState().rafPending).toBe(true);
  });

  it("SET_CURRENT_SHAPE updates shape on current stroke", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_CURRENT_STROKE", stroke: makeStroke() });
    const shape = { type: "circle" as const, cx: 10, cy: 10, radius: 5 };
    dispatch({ type: "SET_CURRENT_SHAPE", shape });
    expect(getState().currentStroke!.shape).toEqual(shape);
  });

  it("SET_CURRENT_SHAPE without current stroke is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_CURRENT_SHAPE", shape: { type: "circle" as const, cx: 0, cy: 0, radius: 1 } });
    expect(getState().currentStroke).toBeNull();
  });

  it("state is frozen after every dispatch", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "SET_TOOL", tool: "eraser" });
    expect(Object.isFrozen(getState())).toBe(true);
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    expect(Object.isFrozen(getState())).toBe(true);
  });

  // --- Select tool ---

  it("SELECT_STROKE sets selectedStrokeIndex", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "SELECT_STROKE", index: 0 });
    expect(getState().selectedStrokeIndex).toBe(0);
  });

  it("SELECT_STROKE null deselects", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "SELECT_STROKE", index: 0 });
    dispatch({ type: "SELECT_STROKE", index: null });
    expect(getState().selectedStrokeIndex).toBeNull();
  });

  it("DELETE_SELECTED removes the selected stroke", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    expect(getState().strokes.length).toBe(2);
    dispatch({ type: "SELECT_STROKE", index: 0 });
    dispatch({ type: "DELETE_SELECTED" });
    expect(getState().strokes.length).toBe(1);
    expect(getState().selectedStrokeIndex).toBeNull();
  });

  it("DELETE_SELECTED adjusts pinCounter when deleting a pin", () => {
    const { getState, dispatch } = createDrawStore();
    const pin: PinEntry = { type: "pin", x: 100, y: 100, number: 1, color: "#ef4444", angle: 0 };
    dispatch({ type: "ADD_STROKE", stroke: pin });
    dispatch({ type: "INCREMENT_PIN_COUNTER" });
    expect(getState().pinCounter).toBe(1);
    dispatch({ type: "SELECT_STROKE", index: 0 });
    dispatch({ type: "DELETE_SELECTED" });
    expect(getState().pinCounter).toBe(0);
    expect(getState().strokes.length).toBe(0);
  });

  it("DELETE_SELECTED with no selection is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "DELETE_SELECTED" });
    expect(getState().strokes.length).toBe(1); // unchanged
  });

  it("MOVE_PIN updates pin position preserving angle", () => {
    const { getState, dispatch } = createDrawStore();
    const pin: PinEntry = { type: "pin", x: 100, y: 100, number: 1, color: "#ef4444", angle: 1.5 };
    dispatch({ type: "ADD_STROKE", stroke: pin });
    dispatch({ type: "MOVE_PIN", index: 0, x: 200, y: 300 });
    const moved = getState().strokes[0] as PinEntry;
    expect(moved.x).toBe(200);
    expect(moved.y).toBe(300);
    expect(moved.angle).toBe(1.5); // preserved
  });

  it("MOVE_PIN on non-pin stroke is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "MOVE_PIN", index: 0, x: 200, y: 300 });
    // Stroke should be unchanged (it's not a pin)
    const s = getState().strokes[0] as StrokeDraw;
    expect(s.points[0].x).toBe(0); // original position
  });

  // --- Recolor selected ---

  it("RECOLOR_SELECTED changes color of selected freehand stroke", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "SELECT_STROKE", index: 0 });
    dispatch({ type: "RECOLOR_SELECTED", color: "#C4F56A" });
    expect((getState().strokes[0] as StrokeDraw).color).toBe("#C4F56A");
  });

  it("RECOLOR_SELECTED changes color of selected pin", () => {
    const { getState, dispatch } = createDrawStore();
    const pin: PinEntry = { type: "pin", x: 50, y: 50, number: 1, color: "#ef4444", angle: 0 };
    dispatch({ type: "ADD_STROKE", stroke: pin });
    dispatch({ type: "SELECT_STROKE", index: 0 });
    dispatch({ type: "RECOLOR_SELECTED", color: "#000000" });
    expect((getState().strokes[0] as PinEntry).color).toBe("#000000");
  });

  it("RECOLOR_SELECTED with no selection is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "RECOLOR_SELECTED", color: "#000000" });
    expect((getState().strokes[0] as StrokeDraw).color).toBe("#ef4444"); // unchanged
  });

  // --- Set pin angle ---

  it("SET_PIN_ANGLE updates pin arrow direction", () => {
    const { getState, dispatch } = createDrawStore();
    const pin: PinEntry = { type: "pin", x: 100, y: 100, number: 1, color: "#ef4444", angle: 0 };
    dispatch({ type: "ADD_STROKE", stroke: pin });
    dispatch({ type: "SET_PIN_ANGLE", index: 0, angle: Math.PI / 2 });
    expect((getState().strokes[0] as PinEntry).angle).toBeCloseTo(Math.PI / 2);
  });

  it("SET_PIN_ANGLE on non-pin is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "SET_PIN_ANGLE", index: 0, angle: 1.0 });
    // Should not crash or modify the stroke
    expect(getState().strokes.length).toBe(1);
  });

  it("SET_TOOL clears selectedStrokeIndex", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "SELECT_STROKE", index: 0 });
    expect(getState().selectedStrokeIndex).toBe(0);
    dispatch({ type: "SET_TOOL", tool: "eraser" });
    expect(getState().selectedStrokeIndex).toBeNull();
  });
});

describe("draw store — MOVE_STROKE", () => {
  it("offsets a freehand stroke's points by dx/dy", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: 5, dy: -3 });
    const moved = getState().strokes[0] as StrokeDraw;
    expect(moved.points[0].x).toBe(5);
    expect(moved.points[0].y).toBe(-3);
    expect(moved.points[1].x).toBe(15);
    expect(moved.points[1].y).toBe(7);
  });

  it("offsets a freehand stroke's shape if present", () => {
    const { getState, dispatch } = createDrawStore();
    const stroke = makeStroke({
      shape: { type: "rect", x: 10, y: 20, w: 50, h: 30 },
    });
    dispatch({ type: "ADD_STROKE", stroke });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: 7, dy: 11 });
    const moved = getState().strokes[0] as StrokeDraw;
    expect(moved.shape).toBeDefined();
    if (moved.shape && moved.shape.type === "rect") {
      expect(moved.shape.x).toBe(17);
      expect(moved.shape.y).toBe(31);
    }
  });

  it("offsets a pin's x/y by dx/dy", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makePin(1) });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: 10, dy: -20 });
    const moved = getState().strokes[0] as PinEntry;
    expect(moved.x).toBe(110);
    expect(moved.y).toBe(80);
    expect(moved.angle).toBe(0); // preserved
  });

  it("offsets a blur entry's bbox by dx/dy", () => {
    const { getState, dispatch } = createDrawStore();
    // BlurEntry needs a pixelCanvas — use a minimal stub
    const fakeCanvas = { width: 10, height: 10 } as unknown as HTMLCanvasElement;
    const blur: BlurEntry = { type: "blur", bbox: { x: 50, y: 60, w: 100, h: 80 }, pixelCanvas: fakeCanvas };
    dispatch({ type: "ADD_STROKE", stroke: blur });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: -5, dy: 15 });
    const moved = getState().strokes[0] as BlurEntry;
    expect(moved.bbox.x).toBe(45);
    expect(moved.bbox.y).toBe(75);
    expect(moved.bbox.w).toBe(100); // unchanged
    expect(moved.bbox.h).toBe(80);  // unchanged
  });

  it("offsets a spotlight entry's points by dx/dy", () => {
    const { getState, dispatch } = createDrawStore();
    const spot: SpotlightEntry = {
      type: "spotlight",
      points: [
        { x: 10, y: 20, pressure: 0.5 },
        { x: 30, y: 40, pressure: 0.5 },
      ],
    };
    dispatch({ type: "ADD_STROKE", stroke: spot });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: 3, dy: 7 });
    const moved = getState().strokes[0] as SpotlightEntry;
    expect(moved.points[0].x).toBe(13);
    expect(moved.points[0].y).toBe(27);
    expect(moved.points[1].x).toBe(33);
    expect(moved.points[1].y).toBe(47);
  });

  it("offsets a spotlight with circle shape", () => {
    const { getState, dispatch } = createDrawStore();
    const spot: SpotlightEntry = {
      type: "spotlight",
      points: [{ x: 0, y: 0, pressure: 0.5 }],
      shape: { type: "circle", cx: 50, cy: 60, radius: 20 },
    };
    dispatch({ type: "ADD_STROKE", stroke: spot });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: 10, dy: -5 });
    const moved = getState().strokes[0] as SpotlightEntry;
    if (moved.shape && moved.shape.type === "circle") {
      expect(moved.shape.cx).toBe(60);
      expect(moved.shape.cy).toBe(55);
      expect(moved.shape.radius).toBe(20);
    }
  });

  it("out-of-range index is a no-op", () => {
    const { getState, dispatch } = createDrawStore();
    dispatch({ type: "ADD_STROKE", stroke: makeStroke() });
    dispatch({ type: "MOVE_STROKE", index: 5, dx: 10, dy: 10 });
    // Should not crash
    expect(getState().strokes.length).toBe(1);
  });

  it("offsets a stroke with line shape", () => {
    const { getState, dispatch } = createDrawStore();
    const stroke = makeStroke({
      shape: {
        type: "line",
        start: { x: 0, y: 0, pressure: 0.5 },
        end: { x: 100, y: 100, pressure: 0.5 },
      },
    });
    dispatch({ type: "ADD_STROKE", stroke });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: 5, dy: 10 });
    const moved = getState().strokes[0] as StrokeDraw;
    if (moved.shape && moved.shape.type === "line") {
      expect(moved.shape.start.x).toBe(5);
      expect(moved.shape.start.y).toBe(10);
      expect(moved.shape.end.x).toBe(105);
      expect(moved.shape.end.y).toBe(110);
    }
  });

  it("offsets a stroke with arrow shape", () => {
    const { getState, dispatch } = createDrawStore();
    const stroke = makeStroke({
      shape: {
        type: "arrow",
        start: { x: 10, y: 10, pressure: 0.5 },
        end: { x: 50, y: 50, pressure: 0.5 },
        headTip: { x: 55, y: 45, pressure: 0.5 },
      },
    });
    dispatch({ type: "ADD_STROKE", stroke });
    dispatch({ type: "MOVE_STROKE", index: 0, dx: -3, dy: 7 });
    const moved = getState().strokes[0] as StrokeDraw;
    if (moved.shape && moved.shape.type === "arrow") {
      expect(moved.shape.start.x).toBe(7);
      expect(moved.shape.start.y).toBe(17);
      expect(moved.shape.end.x).toBe(47);
      expect(moved.shape.end.y).toBe(57);
      expect(moved.shape.headTip.x).toBe(52);
      expect(moved.shape.headTip.y).toBe(52);
    }
  });
});
