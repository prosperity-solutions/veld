/**
 * Veld Controls — Vue 3 composable
 *
 * Drop this into your component or a shared composables module.
 * No npm package needed — reads from window.__veld_controls injected by veld.
 *
 * Usage (Composition API):
 *   const duration = useVeldControl("duration", 200);
 *   const easing = useVeldControl("easing", "ease-out");
 *
 * In template:
 *   <div :style="{ transitionDuration: duration + 'ms' }">...</div>
 *
 * The agent will add/remove these hooks automatically.
 */

import { ref, onMounted, onUnmounted, type Ref } from "vue";

/**
 * Bind a Vue ref to a veld control value.
 * Updates reactively as the user scrubs sliders/inputs.
 */
export function useVeldControl<T>(name: string, defaultValue: T): Ref<T> {
  const value = ref<T>(defaultValue) as Ref<T>;
  let unsub: (() => void) | undefined;

  onMounted(() => {
    const controls = (window as any).__veld_controls;
    if (!controls) return;
    const current = controls.get(name);
    if (current !== undefined) value.value = current as T;
    unsub = controls.on(name, (v: T) => { value.value = v; });
  });

  onUnmounted(() => {
    if (unsub) unsub();
  });

  return value;
}

/**
 * Bind a callback to a veld action button.
 */
export function useVeldAction(name: string, callback: () => void): void {
  let unsub: (() => void) | undefined;

  onMounted(() => {
    const controls = (window as any).__veld_controls;
    if (!controls) return;
    unsub = controls.onAction(name, callback);
  });

  onUnmounted(() => {
    if (unsub) unsub();
  });
}
