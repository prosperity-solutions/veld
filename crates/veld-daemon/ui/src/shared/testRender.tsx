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
