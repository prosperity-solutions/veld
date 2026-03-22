import { describe, it, expect } from "vitest";
import { createDrawStore, drawReducer, type DrawState, type DrawAction } from "../src/draw-overlay/store";
import type { PinEntry, StrokeDraw } from "../src/draw-overlay/types";

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
});
