export interface Point {
  x: number;
  y: number;
  pressure: number;
}

export type RecognizedShape =
  | { type: "line"; start: Point; end: Point }
  | { type: "arrow"; start: Point; end: Point; headTip: Point }
  | { type: "circle"; cx: number; cy: number; radius: number }
  | { type: "rect"; x: number; y: number; w: number; h: number };

export type DrawTool = "draw" | "eraser" | "spotlight" | "blur" | "number";

export interface BBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface StrokeDraw {
  type?: undefined;
  points: Point[];
  color: string;
  baseWidth: number;
  compositeOp: string;
  hasPressure: boolean;
  toolMode: DrawTool;
  shape?: RecognizedShape;
  width?: number; // legacy compat
}

export interface PinEntry {
  type: "pin";
  x: number;
  y: number;
  number: number;
  color: string;
  angle: number; // radians — direction the pointer arrow points
}

export interface BlurEntry {
  type: "blur";
  bbox: BBox;
  pixelCanvas: HTMLCanvasElement;
}

export interface SpotlightEntry {
  type: "spotlight";
  points: Point[];
  shape?: RecognizedShape | null;
}

export type StrokeEntry = StrokeDraw | PinEntry | BlurEntry | SpotlightEntry;

export interface DrawActivateOptions {
  inline?: boolean;
  pageSnapshot?: ImageBitmap | HTMLCanvasElement | HTMLImageElement | null;
  baseImage?: HTMLImageElement | HTMLCanvasElement | null;
  mountTarget?: HTMLElement | ShadowRoot;
  onDone?: (hasStrokes: boolean) => void;
  /** Lazy snapshot acquisition — called by blur tool when it needs page pixels. */
  acquireSnapshot?: () => Promise<ImageBitmap | null>;
}
