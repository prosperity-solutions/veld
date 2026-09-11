import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

/**
 * What every `*.test.tsx` needs before it renders anything. Wired in as the
 * `dom` project's `setupFiles` (`vite.config.ts`), so no test imports it.
 *
 * Three jobs, and the first one is not optional.
 *
 * **Unmount between tests.** `@testing-library/react` normally registers its own
 * `afterEach(cleanup)` — but only when it can see a *global* `afterEach`, which
 * means only under `globals: true`. This package imports `describe`/`test`/
 * `expect` explicitly instead, so that auto-registration never happens and each
 * `render` leaves its tree in `document.body` for the next test to trip over.
 * The symptom is not a missing element but a duplicated one: `getByText` throws
 * "Found multiple elements with the text: …" on the *second* test in a file,
 * which reads like the component rendering twice rather than like the previous
 * test never leaving. Both of this file's first two component tests failed that
 * way before this hook existed.
 *
 * **Stub the two browser APIs jsdom lacks that Mantine calls.** `matchMedia`
 * (color-scheme, `visibleFrom`/`hiddenFrom`) and `ResizeObserver` (anything that
 * measures itself — `ScrollArea.Autosize`, a sticky `Table` header). Both stubs
 * are deliberately dumb: every media query answers "no", so a test always sees
 * the desktop branch, and the observer never reports a size change. That is the
 * right default for a unit test; a test that needs a real answer should stub it
 * locally and say why, rather than this file growing a fake layout engine.
 */

afterEach(() => {
  cleanup();
  // **Undo any `setPlatform` globally, so the correct teardown is structural.**
  // jsdom defines `platform` as a getter on `Navigator.prototype`, not on the
  // navigator instance, so the instinctive "save the own descriptor and restore
  // it" teardown saves `undefined` and restores nothing — the override survives
  // into every later test in the file, silently and order-dependently. That
  // shipped in the first draft of `ShortcutsDialog.test.tsx` and two tests
  // passed only because they were declared above the first override. Deleting
  // the own property un-shadows the prototype getter, which answers `""`.
  // Unconditional: deleting an absent own property is a no-op.
  Reflect.deleteProperty(globalThis.navigator, "platform");
  Reflect.deleteProperty(globalThis.navigator, "userAgentData");
});

// Defined only when genuinely missing, so a future jsdom that ships a real
// `matchMedia` wins over this stub instead of being shadowed by it.
if (!globalThis.window.matchMedia) {
  Object.defineProperty(globalThis.window, "matchMedia", {
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      // Mantine still reaches for the deprecated pair on some paths.
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }),
  });
}

if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}
