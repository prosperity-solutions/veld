/**
 * Veld Controls — live agent-to-user interactive value registry.
 *
 * Injected as `window.__veld_controls`. Framework-agnostic event emitter
 * that bridges agent-defined controls with application code.
 *
 * Usage from application code:
 *   window.__veld_controls.on('duration', (v) => el.style.transitionDuration = v + 'ms')
 *   window.__veld_controls.onAction('retry', () => fetchData())
 *
 * Usage from veld overlay:
 *   window.__veld_controls.set('duration', 340)  // fires all listeners
 *   window.__veld_controls.trigger('retry')       // fires action listeners
 */

export interface VeldControls {
  /** Get current value of a named control. */
  get(name: string): unknown;
  /** Set value of a named control, firing all listeners. */
  set(name: string, value: unknown): void;
  /** Subscribe to value changes. Use "*" for wildcard (any change). Returns unsubscribe fn. */
  on(name: string, callback: (value: unknown) => void): () => void;
  /** Subscribe to button/action events. Returns unsubscribe fn. */
  onAction(name: string, callback: () => void): () => void;
  /** Trigger a button/action event. */
  trigger(name: string): void;
  /** Get a snapshot of all current values. */
  values(): Record<string, unknown>;
}

export type ControlDef =
  | { type: "number"; name: string; value: number; min?: number; max?: number; step?: number; unit?: string; label?: string }
  | { type: "slider"; name: string; value: number; min: number; max: number; step?: number; unit?: string; label?: string }
  | { type: "select"; name: string; value: string; options: string[]; label?: string }
  | { type: "color"; name: string; value: string; label?: string }
  | { type: "text"; name: string; value: string; placeholder?: string; label?: string }
  | { type: "toggle"; name: string; value: boolean; label?: string }
  | { type: "button"; name: string; label: string }
  ;

export function createControlsRegistry(): VeldControls {
  const store = new Map<string, unknown>();
  const listeners = new Map<string, Set<(value: unknown) => void>>();
  const actionListeners = new Map<string, Set<() => void>>();
  // rAF-batched notifications: collect changed names, fire once per frame
  let pendingNames: Set<string> | null = null;

  function getListeners(name: string): Set<(value: unknown) => void> {
    let set = listeners.get(name);
    if (!set) {
      set = new Set();
      listeners.set(name, set);
    }
    return set;
  }

  function getActionListeners(name: string): Set<() => void> {
    let set = actionListeners.get(name);
    if (!set) {
      set = new Set();
      actionListeners.set(name, set);
    }
    return set;
  }

  return {
    get(name: string): unknown {
      return store.get(name);
    },

    set(name: string, value: unknown): void {
      store.set(name, value);
      // Batch listener notifications via rAF to avoid thrashing downstream hooks
      if (typeof requestAnimationFrame !== "undefined") {
        if (!pendingNames) {
          pendingNames = new Set();
          requestAnimationFrame(() => {
            const names = pendingNames!;
            pendingNames = null;
            for (const n of names) {
              const v = store.get(n);
              const specific = listeners.get(n);
              if (specific) specific.forEach((cb) => cb(v));
              const wildcard = listeners.get("*");
              if (wildcard) wildcard.forEach((cb) => cb(v));
            }
          });
        }
        pendingNames.add(name);
      } else {
        // No rAF (Node/test env) — fire synchronously
        const specific = listeners.get(name);
        if (specific) specific.forEach((cb) => cb(value));
        const wildcard = listeners.get("*");
        if (wildcard) wildcard.forEach((cb) => cb(value));
      }
    },

    on(name: string, callback: (value: unknown) => void): () => void {
      const set = getListeners(name);
      set.add(callback);
      return () => { set.delete(callback); };
    },

    onAction(name: string, callback: () => void): () => void {
      const set = getActionListeners(name);
      set.add(callback);
      return () => { set.delete(callback); };
    },

    trigger(name: string): void {
      const set = actionListeners.get(name);
      if (set) set.forEach((cb) => cb());
    },

    values(): Record<string, unknown> {
      const result: Record<string, unknown> = {};
      store.forEach((v, k) => { result[k] = v; });
      return result;
    },
  };
}
