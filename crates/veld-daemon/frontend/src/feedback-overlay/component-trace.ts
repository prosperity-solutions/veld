/** Inspect the element for React/Vue component traces. */
export function getComponentTrace(el: Element): string[] | null {
  const trace: string[] = [];
  const MAX_DEPTH = 100;

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
    if (trace.length) return trace;
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
    if (trace.length) return trace;
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
    if (trace.length) return trace;
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
