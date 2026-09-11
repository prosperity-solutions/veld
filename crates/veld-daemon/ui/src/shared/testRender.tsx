import { MantineProvider } from "@mantine/core";
import { render as rtlRender } from "@testing-library/react";
import type { ReactNode } from "react";
import { theme } from "../theme";

/**
 * Render a component the way `/ide` renders it, for a `*.test.tsx` file.
 *
 * Only `.test.tsx` files get a DOM — see the two test projects in
 * `vite.config.ts` — and `shared/testSetup.ts` is what makes jsdom survive
 * Mantine at all. This module is the last piece: the provider.
 *
 * A bare `@testing-library/react` `render` is not enough here, because every
 * component in this package is a Mantine component and one without a
 * `MantineProvider` above it throws (`@mantine/core: MantineProvider was not
 * found in component tree`) rather than rendering something imperfect. The app's
 * own `theme` is used, not a default one, so a test reads the sizes, radii and
 * colours a user actually gets.
 */
export function render(ui: ReactNode) {
  return rtlRender(<MantineProvider theme={theme}>{ui}</MantineProvider>);
}

/**
 * Pretend this machine is `value` for the rest of the current test.
 *
 * `registry.ts`'s `isMac()` reads `navigator.platform` before `navigator
 * .userAgent`, so this is how a `.test.tsx` picks the platform branch —
 * `setPlatform("MacIntel")` for mac, `setPlatform("Linux x86_64")` for not.
 *
 * **Do not write your own teardown for this.** `shared/testSetup.ts`'s global
 * `afterEach` deletes the override after every test; a per-file "save the
 * descriptor and put it back" is the natural instinct and is *wrong* here, for
 * the reason spelled out there. Left to itself it leaks into later tests in the
 * same file without failing anything, which is how it went unnoticed once
 * already.
 */
export function setPlatform(value: string) {
  Object.defineProperty(globalThis.navigator, "platform", {
    value,
    configurable: true,
  });
  // `isMac()` reads `navigator.userAgentData?.platform` *first* and only falls
  // through to `navigator.platform` when it is absent. jsdom 30 does not
  // implement `userAgentData`, so today the fall-through always happens — but a
  // jsdom that adds it, sourcing it from the host, would silently outrank this
  // and make the platform tests pass on CI and fail on a developer's Mac (or
  // the reverse). Pinning it to `undefined` here means this function sets the
  // platform rather than merely suggesting it.
  Object.defineProperty(globalThis.navigator, "userAgentData", {
    value: undefined,
    configurable: true,
  });
}
