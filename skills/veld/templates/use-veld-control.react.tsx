/**
 * Veld Controls — React hooks
 *
 * Drop this into your component file or create a shared hook module.
 * No npm package needed — reads from window.__veld_controls injected by veld.
 * SSR-safe: returns defaultValue during server rendering.
 *
 * Usage:
 *   const duration = useVeldControl("duration", 200);
 *   const easing = useVeldControl("easing", "ease-out");
 *
 * The agent will add/remove these hooks automatically. When the user
 * clicks "Apply" in the veld panel, the agent replaces the hook call
 * with the chosen constant value.
 */

/* eslint-disable @typescript-eslint/no-explicit-any */
declare global {
  interface Window {
    __veld_controls?: {
      get(name: string): any;
      set(name: string, value: any): void;
      on(name: string, cb: (value: any) => void): () => void;
      onAction(name: string, cb: () => void): () => void;
      trigger(name: string): void;
      values(): Record<string, any>;
    };
  }
}

import { useState, useEffect, useCallback } from "react";

const isBrowser = typeof window !== "undefined";

/**
 * Bind a React component to a veld control value.
 * Updates in real-time as the user scrubs sliders/inputs.
 * Returns defaultValue during SSR — no window access on the server.
 *
 * @param name — control name (must match the agent's control definition)
 * @param defaultValue — fallback when veld is not running or during SSR
 */
export function useVeldControl<T>(name: string, defaultValue: T): T {
  const [value, setValue] = useState<T>(() => {
    if (!isBrowser) return defaultValue;
    return (window.__veld_controls?.get(name) as T) ?? defaultValue;
  });

  useEffect(() => {
    if (!window.__veld_controls) return;
    const current = window.__veld_controls.get(name);
    if (current !== undefined) setValue(current as T);
    const unsub = window.__veld_controls.on(name, (v) => setValue(v as T));
    return unsub;
  }, [name]);

  return value;
}

/**
 * Bind a callback to a veld action button (retry, start, stop, etc.)
 * No-op during SSR.
 *
 * @param name — action name (must match the agent's button definition)
 * @param callback — fires when the user clicks the button in veld
 */
export function useVeldAction(name: string, callback: () => void): void {
  const stableCallback = useCallback(callback, [callback]);
  useEffect(() => {
    if (!window.__veld_controls) return;
    const unsub = window.__veld_controls.onAction(name, stableCallback);
    return unsub;
  }, [name, stableCallback]);
}
