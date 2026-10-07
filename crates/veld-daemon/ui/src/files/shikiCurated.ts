/**
 * What `import … from "shiki"` resolves to in this bundle — see the `resolve.alias`
 * in `vite.config.ts`.
 *
 * **Why the code view's highlighter is aliased rather than configured.**
 * `@pierre/diffs` imports Shiki's main entry, which is the *full* bundle: a lazy
 * loader for every grammar and theme Shiki ships, plus the Oniguruma WebAssembly
 * engine. In an app that fetches chunks that costs nothing until used. `/ide` is one
 * self-contained HTML file (`vite-plugin-singlefile`), so every lazy chunk is inlined
 * up front — the whole catalogue, forever, in a page whose code view is used for a
 * handful of languages. There is no option on the library that narrows the set; its
 * language lookup reads `bundledLanguages` straight off this module.
 *
 * So this module is that entry, narrowed: the same names the library imports, with
 * `bundledLanguages` holding only the grammars `textKind.ts` maps to, and the
 * JavaScript regex engine as the only engine. Every other export the library uses
 * comes from `shiki/core`, which is the engine-free, grammar-free half of the same
 * package — so this is a subset of Shiki, not a fork of it.
 *
 * **Keep the export list in step with what `@pierre/diffs` imports from "shiki".**
 * A name missing here is a build error, not a silent fallback, which is the right
 * way round; `curatedLanguages()` and this map are pinned to agree by a test. That
 * import list is the library's internals, which is why `package.json` pins the
 * library to an exact version: re-check this file when Renovate moves it.
 */

import {
  createBundledHighlighter,
  createCssVariablesTheme,
  createSingletonShorthands,
  getTokenStyleObject,
  guessEmbeddedLanguages,
  stringifyTokenStyle,
} from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";

type LangModule = Promise<{ default: unknown }>;

/** The grammars this bundle carries. Must match `curatedLanguages()`. */
export const bundledLanguages: Record<string, () => LangModule> = {
  css: () => import("shiki/langs/css.mjs"),
  diff: () => import("shiki/langs/diff.mjs"),
  docker: () => import("shiki/langs/docker.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  ini: () => import("shiki/langs/ini.mjs"),
  json: () => import("shiki/langs/json.mjs"),
  jsonc: () => import("shiki/langs/jsonc.mjs"),
  make: () => import("shiki/langs/make.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  scss: () => import("shiki/langs/scss.mjs"),
  shellscript: () => import("shiki/langs/shellscript.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
};

/**
 * No Oniguruma. The library only reaches for it when asked for `shiki-wasm`, which
 * the file pane never does — but a reference to it would inline the WebAssembly
 * blob. Throwing names the problem if a future option ever does ask.
 */
export function createOnigurumaEngine(): never {
  throw new Error("The Oniguruma engine is not bundled in /ide; use the JavaScript engine.");
}

// Themes come from `@pierre/theming`, never from Shiki's catalogue, so the
// highlighter is created with none of Shiki's and the library attaches its own.
export const createHighlighter = createBundledHighlighter({
  langs: bundledLanguages as never,
  themes: {},
  engine: () => createJavaScriptRegexEngine(),
});

// The cast is the same one Shiki's own bundle gets away with by inference: the
// guesser is typed for a highlighter with any theme, ours has none of Shiki's.
export const { codeToHtml } = createSingletonShorthands(createHighlighter, {
  guessEmbeddedLanguages: guessEmbeddedLanguages as never,
});

export {
  createCssVariablesTheme,
  createJavaScriptRegexEngine,
  getTokenStyleObject,
  stringifyTokenStyle,
};
