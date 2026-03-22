interface DrawActivateOptions {
  inline?: boolean;
  pageSnapshot?: ImageBitmap | HTMLCanvasElement | HTMLImageElement | null;
  baseImage?: HTMLImageElement | HTMLCanvasElement | null;
  mountTarget?: HTMLElement;
  onDone?: (hasStrokes: boolean) => void;
}

interface VeldDraw {
  activate(canvas: HTMLCanvasElement, opts?: DrawActivateOptions): () => void;
  compositeOnto(baseBlob: Blob, canvas: HTMLCanvasElement): Promise<Blob>;
}

declare global {
  interface Window {
    __veld_feedback_initialised?: boolean;
    __veld_draw?: VeldDraw;
    __veld_cl?: number;
    __veld_early_logs?: Array<{
      l: string;
      a?: unknown[];
      m?: string;
      s?: string;
      t: number;
    }>;
    __veld_early_originals?: Record<string, (...args: unknown[]) => void>;
  }
}

export {};
