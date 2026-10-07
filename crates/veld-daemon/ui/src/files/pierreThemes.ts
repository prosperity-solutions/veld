/**
 * What `@pierre/theming/themes` resolves to in this bundle — the theme half of
 * `shikiCurated.ts`, aliased in `vite.config.ts` for the same reason.
 *
 * The real module registers every Pierre theme (ten, ~30 KB each) and every theme in
 * Shiki's catalogue as lazy loaders, and in a single-file bundle a lazy loader is
 * inlined regardless of whether it is ever called. The code view uses exactly two:
 * `pierre-dark` and `pierre-light`, picked by the app's own theme. So those are the
 * whole collection here, `shikiThemes` is empty (the library only consults it for a
 * theme name it does not already know, and the file pane never names one), and
 * `createTheme` is the original's three lines, re-stated because the package does
 * not export its module path.
 */

import { createThemeCollection } from "@pierre/theming";
import { normalizeTheme } from "shiki/core";

type ThemeLoader = () => Promise<unknown>;

export function createTheme(opts: {
  name: string;
  load: ThemeLoader;
  colorScheme: "light" | "dark";
  collection: string;
  displayName: string;
}) {
  return {
    name: opts.name,
    colorScheme: opts.colorScheme,
    collection: opts.collection,
    displayName: opts.displayName,
    load: async () => {
      const mod = (await opts.load()) as { default?: unknown };
      return normalizeTheme((mod?.default ?? mod) as never);
    },
  };
}

export const pierreThemes = createThemeCollection({
  themes: [
    createTheme({
      name: "pierre-light",
      collection: "pierre",
      colorScheme: "light",
      displayName: "Pierre Light",
      load: () => import("@pierre/theme/pierre-light"),
    }),
    createTheme({
      name: "pierre-dark",
      collection: "pierre",
      colorScheme: "dark",
      displayName: "Pierre Dark",
      load: () => import("@pierre/theme/pierre-dark"),
    }),
  ] as never,
});

export const shikiThemes = createThemeCollection({ themes: [] });
