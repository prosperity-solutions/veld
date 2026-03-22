/**
 * Example React hook for binding to veld controls.
 *
 * Copy this into your component file or create a shared hook.
 * No npm package needed — it reads from window.__veld_controls
 * which is injected by the veld overlay.
 *
 * Usage:
 *   const duration = useVeldControl("duration", 200);
 *   const easing = useVeldControl("easing", "ease-out");
 *   const color = useVeldControl("accent", "#3b82f6");
 */

// eslint-disable-next-line @typescript-eslint/no-explicit-any
declare global {
  interface Window {
    __veld_controls?: {
      get(name: string): unknown;
      set(name: string, value: unknown): void;
      on(name: string, cb: (value: unknown) => void): () => void;
      onAction(name: string, cb: () => void): () => void;
      trigger(name: string): void;
      values(): Record<string, unknown>;
    };
  }
}

import { useState, useEffect } from "react";

export function useVeldControl<T>(name: string, defaultValue: T): T {
  const [value, setValue] = useState<T>(
    () => (window.__veld_controls?.get(name) as T) ?? defaultValue,
  );

  useEffect(() => {
    if (!window.__veld_controls) return;
    const unsub = window.__veld_controls.on(name, (v) => setValue(v as T));
    return unsub;
  }, [name]);

  return value;
}

/**
 * Hook for veld action buttons (retry, start, stop, etc.)
 *
 * Usage:
 *   useVeldAction("retry", () => fetchData());
 *   useVeldAction("stop", () => controller.abort());
 */
export function useVeldAction(name: string, callback: () => void): void {
  useEffect(() => {
    if (!window.__veld_controls) return;
    const unsub = window.__veld_controls.onAction(name, callback);
    return unsub;
  }, [name, callback]);
}
