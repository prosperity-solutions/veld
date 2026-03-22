/**
 * Draw overlay store — per-session state for the canvas annotation engine.
 *
 * Created inside activate(), captured by closure, nulled on cleanup.
 * Uses the shared createStore for freeze-on-dispatch guarantees.
 */
import { createStore, type Store } from "../shared/create-store";
import type { DrawTool, StrokeEntry, StrokeDraw, PinEntry } from "./types";

export interface DrawState {
  hasPressureDevice: boolean;
  activeWidthIdx: number;
  activeColorIdx: number;
  strokes: StrokeEntry[];
  undoneStrokes: StrokeEntry[];
  currentStroke: StrokeDraw | null;
  baseWidth: number;
  toolMode: DrawTool;
  shapeSnap: boolean;
  pinCounter: number;
  pendingPin: PinEntry | null;
  pendingPinAngle: number;
  drawing: boolean;
  rafPending: boolean;
  cursorPos: { x: number; y: number } | null;
  toolbarCollapsed: boolean;
  selectedStrokeIndex: number | null;
}

export type DrawAction =
  | { type: "ADD_STROKE"; stroke: StrokeEntry }
  | { type: "SET_CURRENT_STROKE"; stroke: StrokeDraw | null }
  | { type: "APPEND_POINT"; point: { x: number; y: number; pressure: number } }
  | { type: "SET_CURRENT_SHAPE"; shape: StrokeDraw["shape"] }
  | { type: "UNDO" }
  | { type: "REDO" }
  | { type: "SET_TOOL"; tool: DrawTool }
  | { type: "SET_COLOR"; idx: number }
  | { type: "SET_WIDTH"; idx: number }
  | { type: "SET_BASE_WIDTH"; width: number }
  | { type: "PLACE_PIN"; pin: PinEntry }
  | { type: "SET_PENDING_PIN"; pin: PinEntry | null }
  | { type: "SET_PENDING_PIN_ANGLE"; angle: number }
  | { type: "LOCK_PIN" }
  | { type: "CANCEL_PIN" }
  | { type: "INCREMENT_PIN_COUNTER" }
  | { type: "TOGGLE_SHAPE_SNAP" }
  | { type: "TOGGLE_COLLAPSE" }
  | { type: "SET_DRAWING"; drawing: boolean }
  | { type: "SET_RAF_PENDING"; pending: boolean }
  | { type: "SET_CURSOR_POS"; pos: { x: number; y: number } | null }
  | { type: "SET_HAS_PRESSURE"; has: boolean }
  | { type: "CLEAR_UNDONE" }
  | { type: "SELECT_STROKE"; index: number | null }
  | { type: "DELETE_SELECTED" }
  | { type: "MOVE_PIN"; index: number; x: number; y: number }
  | { type: "RECOLOR_SELECTED"; color: string }
  | { type: "SET_PIN_ANGLE"; index: number; angle: number }
  ;

export function drawReducer(s: Readonly<DrawState>, action: DrawAction): DrawState {
  switch (action.type) {
    case "ADD_STROKE":
      return { ...s, strokes: [...s.strokes, action.stroke], undoneStrokes: [] };
    case "SET_CURRENT_STROKE":
      return { ...s, currentStroke: action.stroke };
    case "APPEND_POINT": {
      if (!s.currentStroke) return { ...s };
      return {
        ...s,
        currentStroke: {
          ...s.currentStroke,
          points: [...s.currentStroke.points, action.point],
        },
      };
    }
    case "SET_CURRENT_SHAPE": {
      if (!s.currentStroke) return { ...s };
      return {
        ...s,
        currentStroke: { ...s.currentStroke, shape: action.shape },
      };
    }
    case "UNDO": {
      if (s.pendingPin) {
        return {
          ...s,
          pinCounter: Math.max(0, s.pinCounter - 1),
          pendingPin: null,
        };
      }
      if (s.strokes.length === 0) return { ...s };
      const removed = s.strokes[s.strokes.length - 1];
      const newStrokes = s.strokes.slice(0, -1);
      let newPinCounter = s.pinCounter;
      if ((removed as PinEntry).type === "pin") {
        newPinCounter = Math.max(0, newPinCounter - 1);
      }
      return {
        ...s,
        strokes: newStrokes,
        undoneStrokes: [...s.undoneStrokes, removed],
        pinCounter: newPinCounter,
      };
    }
    case "REDO": {
      if (s.undoneStrokes.length === 0) return { ...s };
      const restored = s.undoneStrokes[s.undoneStrokes.length - 1];
      let newPinCounter = s.pinCounter;
      if ((restored as PinEntry).type === "pin") {
        newPinCounter = (restored as PinEntry).number;
      }
      return {
        ...s,
        strokes: [...s.strokes, restored],
        undoneStrokes: s.undoneStrokes.slice(0, -1),
        pinCounter: newPinCounter,
      };
    }
    case "SET_TOOL":
      return { ...s, toolMode: action.tool };
    case "SET_COLOR":
      return { ...s, activeColorIdx: action.idx };
    case "SET_WIDTH":
      return { ...s, activeWidthIdx: action.idx };
    case "SET_BASE_WIDTH":
      return { ...s, baseWidth: action.width };
    case "PLACE_PIN":
      return {
        ...s,
        strokes: [...s.strokes, action.pin],
        pendingPin: null,
        undoneStrokes: [],
      };
    case "SET_PENDING_PIN":
      return { ...s, pendingPin: action.pin };
    case "SET_PENDING_PIN_ANGLE":
      return { ...s, pendingPinAngle: action.angle };
    case "LOCK_PIN": {
      if (!s.pendingPin) return { ...s };
      const locked: PinEntry = { ...s.pendingPin, angle: s.pendingPinAngle };
      return {
        ...s,
        strokes: [...s.strokes, locked],
        pendingPin: null,
        undoneStrokes: [],
      };
    }
    case "CANCEL_PIN": {
      if (!s.pendingPin) return { ...s };
      return {
        ...s,
        pinCounter: Math.max(0, s.pinCounter - 1),
        pendingPin: null,
      };
    }
    case "INCREMENT_PIN_COUNTER":
      return { ...s, pinCounter: s.pinCounter + 1 };
    case "TOGGLE_SHAPE_SNAP":
      return { ...s, shapeSnap: !s.shapeSnap };
    case "TOGGLE_COLLAPSE":
      return { ...s, toolbarCollapsed: !s.toolbarCollapsed };
    case "SET_DRAWING":
      return { ...s, drawing: action.drawing };
    case "SET_RAF_PENDING":
      return { ...s, rafPending: action.pending };
    case "SET_CURSOR_POS":
      return { ...s, cursorPos: action.pos };
    case "SET_HAS_PRESSURE":
      return { ...s, hasPressureDevice: action.has };
    case "CLEAR_UNDONE":
      return { ...s, undoneStrokes: [] };
    case "SELECT_STROKE":
      return { ...s, selectedStrokeIndex: action.index };
    case "DELETE_SELECTED": {
      if (s.selectedStrokeIndex === null || s.selectedStrokeIndex >= s.strokes.length) {
        return { ...s, selectedStrokeIndex: null };
      }
      const deleted = s.strokes[s.selectedStrokeIndex];
      const newStrokes = s.strokes.filter((_, i) => i !== s.selectedStrokeIndex);
      const newPinCounter = (deleted as PinEntry).type === "pin"
        ? Math.max(0, s.pinCounter - 1)
        : s.pinCounter;
      return { ...s, strokes: newStrokes, selectedStrokeIndex: null, pinCounter: newPinCounter };
    }
    case "MOVE_PIN": {
      if (action.index >= s.strokes.length) return { ...s };
      const entry = s.strokes[action.index];
      if ((entry as PinEntry).type !== "pin") return { ...s };
      const pin = entry as PinEntry;
      const updated: PinEntry = { ...pin, x: action.x, y: action.y };
      const strokes = s.strokes.map((stroke, i) => i === action.index ? updated : stroke);
      return { ...s, strokes };
    }
    case "RECOLOR_SELECTED": {
      if (s.selectedStrokeIndex === null || s.selectedStrokeIndex >= s.strokes.length) return { ...s };
      const entry = s.strokes[s.selectedStrokeIndex];
      const recolored = { ...entry, color: action.color };
      const strokes = s.strokes.map((stroke, i) => i === s.selectedStrokeIndex ? recolored : stroke);
      return { ...s, strokes };
    }
    case "SET_PIN_ANGLE": {
      if (action.index >= s.strokes.length) return { ...s };
      const entry = s.strokes[action.index];
      if ((entry as PinEntry).type !== "pin") return { ...s };
      const pin = entry as PinEntry;
      const updated: PinEntry = { ...pin, angle: action.angle };
      const strokes = s.strokes.map((stroke, i) => i === action.index ? updated : stroke);
      return { ...s, strokes };
    }
    default:
      return { ...s };
  }
}

const DEFAULT_BASE_WIDTH = 5;

export function createDrawStore(): Store<DrawState, DrawAction> {
  return createStore<DrawState, DrawAction>(drawReducer, {
    hasPressureDevice: false,
    activeWidthIdx: 1,
    activeColorIdx: 0,
    strokes: [],
    undoneStrokes: [],
    currentStroke: null,
    baseWidth: DEFAULT_BASE_WIDTH,
    toolMode: "draw",
    shapeSnap: false,
    pinCounter: 0,
    pendingPin: null,
    pendingPinAngle: 0,
    drawing: false,
    rafPending: false,
    cursorPos: null,
    toolbarCollapsed: false,
    selectedStrokeIndex: null,
  });
}
