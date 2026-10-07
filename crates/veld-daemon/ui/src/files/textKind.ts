/**
 * Which renderer a file pane uses, decided by the file's name.
 *
 * **A mirror, and only for drawing.** The daemon and the CLI route with
 * `veld_core::files::text_kind`, which is the authority on *whether* a file opens
 * in a file pane at all; this copy only picks between the three renderers once
 * one has. So a name this list does not know is not refused here — it is shown as
 * plain text, which is the honest rendering of "a text file the daemon was willing
 * to hand over". Keep the extension list in step with the Rust one anyway: a kind
 * that disagrees renders a Markdown file as code, which is wrong rather than broken.
 *
 * **Both copies are checked against one fixture**, `veld-core/src/text_kinds.json`:
 * "agrees with the shared text-kind fixture" in `files.test.ts` here, and
 * `text_kind_lists_match_the_shared_fixture` in `veld_core::files`. Adding a kind
 * means the Rust list, the fixture and this file — see AGENTS.md, "Adding a file
 * type to the file pane".
 */

export type TextKind = "markdown" | "csv" | "tsv" | "code" | "plain";

/** Exported for the fixture test in `files.test.ts`, like the two sets below. */
export const BY_EXTENSION: Record<string, TextKind> = {
  md: "markdown",
  markdown: "markdown",
  csv: "csv",
  tsv: "tsv",
  txt: "plain",
  log: "plain",
};

/** Code by the Rust list's reckoning that has no grammar here — shown as code (line
 *  numbers, selection, gutter) without colour. An extension with a grammar goes in
 *  {@link LANGUAGES} instead, never both. */
export const CODE_EXTENSIONS = new Set([
  "xml", "html", "htm", "rb", "java", "kt", "swift", "c", "h", "cpp", "hpp", "cs", "php",
  "graphql", "proto",
]);

/** Names with no extension that are still code. */
export const CODE_NAMES = new Set(["Dockerfile", "Makefile", "justfile"]);

function baseName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

function extension(path: string): string {
  const name = baseName(path);
  const dot = name.lastIndexOf(".");
  // A leading dot is a hidden file's name, not an extension (`.env` is not an
  // `env` file) — and nothing in the list is spelled that way.
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/** The renderer for a path. Anything the table does not name is code if it has a
 *  grammar below or the Rust list calls it source, plain text otherwise — and both of
 *  those draw in the code view; the difference is only whether it is coloured. */
export function textKind(path: string): TextKind {
  const ext = extension(path);
  // `Object.hasOwn`, not `in` or a bare index: `foo.constructor` must not find
  // the object prototype's `constructor`.
  if (Object.hasOwn(BY_EXTENSION, ext)) return BY_EXTENSION[ext];
  if (CODE_NAMES.has(baseName(path)) || Object.hasOwn(LANGUAGES, ext) || CODE_EXTENSIONS.has(ext)) {
    return "code";
  }
  return "plain";
}

/**
 * Whether the daemon's file pane will show `path` at all — the mirror of
 * `veld_core::files::readable_text_kind`, where {@link textKind} only picks a
 * renderer and falls back to plain text for anything.
 *
 * For a list that offers files to open (Changed files): a row the daemon would
 * refuse by name — a picture, a PDF, `Cargo.lock`, `LICENSE`, `.gitignore` — is
 * better disabled with a reason than opened into a pane that says "not found".
 * The daemon stays the authority; a disagreement here only costs a disabled row
 * or a refusal message, never a file served that should not be.
 */
export function viewableAsText(path: string): boolean {
  const ext = extension(path);
  return (
    CODE_NAMES.has(baseName(path)) ||
    Object.hasOwn(BY_EXTENSION, ext) ||
    Object.hasOwn(LANGUAGES, ext) ||
    CODE_EXTENSIONS.has(ext)
  );
}

/**
 * The curated grammar set, by extension.
 *
 * **Curated because every grammar is in the `/ide` bundle forever.** The page is a
 * single self-contained HTML file, so Shiki's lazy per-language chunks are inlined
 * rather than fetched — the full set would add megabytes for languages almost
 * nobody opens in a review pane. The list is what an agent in a typical web or
 * systems repo writes; an extension outside it renders as unhighlighted text with
 * line numbers, which is still a usable view. `files/shikiCurated.ts` is the other
 * half: it is what makes this list the whole of what ships, and the two must name
 * the same grammars (`files.test.ts` pins that).
 */
export const LANGUAGES: Record<string, string> = {
  // One grammar for the whole JavaScript family. TSX parses TypeScript, JSX and
  // plain JavaScript; the only thing it reads differently is the old `<T>value`
  // cast, which a `.ts` file almost never uses any more. The separate TypeScript
  // and JavaScript grammars are ~190 KB each, for that one difference.
  ts: "tsx",
  mts: "tsx",
  cts: "tsx",
  tsx: "tsx",
  js: "tsx",
  mjs: "tsx",
  cjs: "tsx",
  jsx: "tsx",
  json: "json",
  jsonc: "jsonc",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  sh: "shellscript",
  bash: "shellscript",
  zsh: "shellscript",
  py: "python",
  rs: "rust",
  go: "go",
  sql: "sql",
  css: "css",
  scss: "scss",
  ini: "ini",
  diff: "diff",
  patch: "diff",
  md: "markdown",
  markdown: "markdown",
  // Absent on purpose: `html` statically embeds the JavaScript grammar and `xml`
  // the Java one, so either would bring a second large grammar with it. Both
  // still open, as plain text with line numbers.
};

const LANGUAGE_BY_NAME: Record<string, string> = {
  Dockerfile: "docker",
  Makefile: "make",
};

/** The grammar a path highlights with, or `"text"` for none. */
export function languageFor(path: string): string {
  return LANGUAGE_BY_NAME[baseName(path)] ?? LANGUAGES[extension(path)] ?? "text";
}

/** Every grammar `languageFor` can answer, for the curated bundle and its test. */
export function curatedLanguages(): string[] {
  return [...new Set([...Object.values(LANGUAGES), ...Object.values(LANGUAGE_BY_NAME)])].sort();
}
