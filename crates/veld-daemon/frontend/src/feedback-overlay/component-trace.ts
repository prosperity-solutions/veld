// Ancestor chains in framework-routed apps (Next.js app router, Nuxt, etc.)
// repeat the same ~15-20 boilerplate wrapper names once per nested layout
// segment, so a real page can produce 70+ raw fiber names though only the
// last handful nearest the clicked element are ever meaningful (the exact
// names the reviewer sees in the on-page tooltip). Cap here — at capture
// time — so every consumer (tooltip, popover pill, and the payload an agent
// receives) gets the same trimmed, non-noisy list instead of the tooltip
// silently truncating while the API payload ships the raw 70-entry dump.
const MAX_TRACE_LEN = 12;
const MAX_DEPTH = 100;

function capTrace(trace: string[]): string[] {
  return trace.length > MAX_TRACE_LEN ? trace.slice(trace.length - MAX_TRACE_LEN) : trace;
}

/** Inspect the element for React/Vue component traces. */
export function getComponentTrace(el: Element): string[] | null {
  const trace: string[] = [];

  // React: __reactFiber$* key
  const fiber = getReactFiber(el);
  if (fiber) {
    let node = fiber;
    let depth = 0;
    while (node && depth++ < MAX_DEPTH) {
      const name = getFiberName(node);
      if (name) trace.unshift(name);
      node = node.return;
    }
    if (trace.length) return capTrace(trace);
  }

  // Vue 3: __vueParentComponent
  if ((el as any).__vueParentComponent) {
    let inst = (el as any).__vueParentComponent;
    let depth = 0;
    while (inst && depth++ < MAX_DEPTH) {
      const vName = inst.type && (inst.type.name || inst.type.__name);
      if (vName) trace.unshift(vName);
      inst = inst.parent;
    }
    if (trace.length) return capTrace(trace);
  }

  // Vue 2: __vue__
  if ((el as any).__vue__) {
    let vm = (el as any).__vue__;
    let depth = 0;
    while (vm && depth++ < MAX_DEPTH) {
      const vmName = vm.$options && vm.$options.name;
      if (vmName) trace.unshift(vmName);
      vm = vm.$parent;
    }
    if (trace.length) return capTrace(trace);
  }

  return null;
}

/** Source location of the clicked element's own JSX/template tag, when the
 *  framework's dev build exposes it. React's dev JSX transform stamps
 *  `_debugSource` on the fiber for every element; Vue's dev loader stamps
 *  `__file` on the component options (no line number available there). */
export interface ComponentSource {
  file: string;
  line?: number;
}

export function getComponentSource(el: Element): ComponentSource | null {
  const fiber = getReactFiber(el);
  if (fiber) {
    // The element's own fiber usually carries _debugSource; if not (e.g. a
    // text/fragment node), walk up to the nearest ancestor that has one.
    let node = fiber;
    let depth = 0;
    while (node && depth++ < MAX_DEPTH) {
      const src = node._debugSource;
      if (src && src.fileName) {
        return { file: src.fileName, line: src.lineNumber };
      }
      node = node.return;
    }
  }

  if ((el as any).__vueParentComponent) {
    let inst = (el as any).__vueParentComponent;
    let depth = 0;
    while (inst && depth++ < MAX_DEPTH) {
      const file = inst.type && inst.type.__file;
      if (file) return { file };
      inst = inst.parent;
    }
  }

  return null;
}

function getReactFiber(el: Element): any {
  const keys = Object.keys(el);
  for (let i = 0; i < keys.length; i++) {
    if (
      keys[i].startsWith("__reactFiber$") ||
      keys[i].startsWith("__reactInternalInstance$")
    ) {
      return (el as any)[keys[i]];
    }
  }
  return null;
}

function getFiberName(fiber: any): string | null {
  if (!fiber || !fiber.type) return null;
  if (typeof fiber.type === "string") return null;
  return fiber.type.displayName || fiber.type.name || null;
}
