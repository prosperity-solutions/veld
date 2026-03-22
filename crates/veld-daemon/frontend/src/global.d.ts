interface DrawActivateOptions {
  inline?: boolean;
  pageSnapshot?: ImageBitmap | HTMLCanvasElement | HTMLImageElement | null;
  baseImage?: HTMLImageElement | HTMLCanvasElement | null;
  mountTarget?: HTMLElement | ShadowRoot;
  onDone?: (hasStrokes: boolean) => void;
  /** Lazy snapshot acquisition — called by blur tool when it needs page pixels. */
  acquireSnapshot?: () => Promise<ImageBitmap | null>;
}

interface VeldDraw {
  activate(canvas: HTMLCanvasElement, opts?: DrawActivateOptions): () => void;
  compositeOnto(baseBlob: Blob, canvas: HTMLCanvasElement): Promise<Blob>;
}

declare global {
  /**
   * Chromium-only extension: `preferCurrentTab` on getDisplayMedia options.
   * Standard `DisplayMediaStreamOptions` doesn't include it yet.
   */
  interface VeldDisplayMediaStreamOptions extends DisplayMediaStreamOptions {
    preferCurrentTab?: boolean;
    video?: MediaTrackConstraints & { displaySurface?: string };
  }

  /**
   * ImageCapture API — available in Chromium-based browsers.
   * Not yet in the TypeScript DOM lib, so we declare it here.
   */
  // eslint-disable-next-line no-var
  var ImageCapture: {
    prototype: ImageCapture;
    new (track: MediaStreamTrack): ImageCapture;
  };

  interface ImageCapture {
    grabFrame(): Promise<ImageBitmap>;
    takePhoto(): Promise<Blob>;
  }

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
