/**
 * Turning files into terminal input — the decisions behind dropping a file on a
 * terminal pane and pasting an image into one.
 *
 * Pure and DOM-free for the same reason `terminalKeys.ts` is: every rule here is
 * a string transformation whose wrong answer is user-visible (a path that
 * silently loses half its name at the first space, a drop the pane accepts when
 * it was really a tab being moved), and the `environment: "node"` test runner
 * can exercise all of it. `terminalHost.ts` holds the listeners; this holds what
 * they decide.
 *
 * **What a drop and an image paste both produce is a path**, never bytes on the
 * wire. A pty carries a byte stream, so there is no protocol for handing a
 * program a picture — every terminal that "supports" dropping an image types its
 * path instead, and every coding agent (Claude Code, Codex) reads an image path
 * as an image. That is the whole mechanism, and it is why a clipboard image —
 * which has no path — has to be written to a file first.
 */

/** The drag type a pane tab carries. Owned here, next to the code that has to
 *  tell a tab drag apart from a file drag, and imported by `PaneArea.tsx`. */
export const TAB_MIME = "application/x-veld-pane-tab";

/**
 * Characters a path may contain unescaped.
 *
 * A conservative allow-list rather than a deny-list of shell metacharacters: the
 * set of characters a shell treats specially differs between `sh`, `zsh` and
 * `fish`, and being wrong in that direction executes something. Everything
 * outside the set gets escaped, which is never wrong — only occasionally ugly.
 *
 * Non-ASCII is deliberately *not* escaped: no shell gives a codepoint above
 * `0x7f` a meaning, and backslash-escaping every character of a Japanese
 * filename would produce a line nobody can read for no gain.
 */
const SAFE_PATH_CHAR = /[A-Za-z0-9_@%+=:,./-]/;

function isSafePathChar(ch: string): boolean {
  return SAFE_PATH_CHAR.test(ch) || ch.charCodeAt(0) > 0x7f;
}

/**
 * One path, ready to be typed into a shell or an agent's composer.
 *
 * **Backslash escaping is the default because it is what the terminal a user
 * compares us to does** — drop a file on Ghostty or iTerm2 and the path arrives
 * with its spaces backslashed. Claude Code's composer and every shell read that
 * form, so matching it is the difference between a dropped `My Photo.png`
 * working and arriving as two arguments.
 *
 * **A newline is the one character backslashing cannot carry**: `\` followed by
 * a newline is a line continuation in every POSIX shell, which *deletes* both
 * characters rather than quoting one — so a filename containing one would paste
 * as a different, shorter path, and (worse) submit the line early. Those fall
 * back to single quotes, where a newline is literal. Single-quoting everything
 * was the tempting simplification and is rejected: a quoted path is not what a
 * composer expecting a drag-and-drop path is looking at.
 */
export function escapePath(path: string): string {
  if (path.includes("\n") || path.includes("\r")) {
    // POSIX single-quoting: everything is literal inside, and the only sequence
    // that can end the quote is a quote — spelled `'\''` (close, escaped quote,
    // reopen).
    return `'${path.replaceAll("'", "'\\''")}'`;
  }
  let out = "";
  for (const ch of path) {
    if (!isSafePathChar(ch)) out += "\\";
    out += ch;
  }
  return out;
}

/**
 * The text a set of dropped paths types into the terminal.
 *
 * Space-separated, and with a **trailing space**: what follows a dropped path is
 * always more typing — another path, a question about the file — and the
 * alternative is every user's first keystroke being a space they had to notice
 * they needed. No trailing newline, deliberately: a drop must never submit. The
 * user decides when the line is finished.
 *
 * Empty in, empty out, so a caller need not special-case a drop that resolved to
 * nothing (a directory the browser could not read, an upload that failed).
 */
export function pathPayload(paths: readonly string[]): string {
  const usable = paths.filter((p) => p.length > 0);
  if (usable.length === 0) return "";
  return `${usable.map(escapePath).join(" ")} `;
}

/**
 * Whether a drag carries files this pane should take.
 *
 * `types` rather than the items themselves because `dragover` — which has to
 * answer this on every pointer move — is forbidden from reading drag *data*, only
 * its type list. A pane tab being dragged over a terminal is explicitly not a
 * file drop even though a cross-window tab drag can carry `Files` alongside its
 * own type; the tab is what the user is moving, and `PaneArea.tsx` owns it.
 */
export function isFileDrop(types: readonly string[]): boolean {
  return types.includes("Files") && !types.includes(TAB_MIME);
}

/** Extension for an image the clipboard handed us as bytes and no name. */
const IMAGE_EXTENSIONS: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/jpg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp",
  "image/tiff": "tiff",
  "image/svg+xml": "svg",
  "image/avif": "avif",
  "image/heic": "heic",
};

/**
 * A filename for a clipboard image, which arrives as a MIME type and bytes.
 *
 * The name is only ever a *hint* — the daemon prefixes a random component and
 * re-sanitises whatever it is given, so this cannot decide where the file lands.
 * It exists so the path the user ends up looking at says what the file is.
 *
 * An unrecognised image type keeps `bin` rather than trusting the MIME subtype
 * as an extension: `image/../../etc` is a string a page can put on a clipboard.
 */
export function clipboardImageName(mime: string): string {
  const ext = IMAGE_EXTENSIONS[mime.toLowerCase().split(";")[0].trim()] ?? "bin";
  return `pasted-image.${ext}`;
}

/** The shape of a `DataTransferItem` this module needs — so the decision below
 *  is testable without a live clipboard. */
export interface ClipboardEntry {
  /** `"file"` or `"string"`, straight from `DataTransferItem.kind`. */
  kind: string;
  /** The MIME type, straight from `DataTransferItem.type`. */
  type: string;
}

/**
 * Which clipboard entry, if any, this paste should upload as an image — its
 * index, or `-1` for "hand the whole thing to xterm as text".
 *
 * **Read off the items, never off `clipboardData.types`.** A copied image
 * advertises itself in the type list as the single entry `"Files"`; the actual
 * `image/png` lives on the item. Deciding from `types` therefore misses every
 * screenshot, which is the one case this feature exists for.
 *
 * **`text/plain` wins whenever it is present.** Copying an image out of a web
 * page puts `text/html` on the clipboard beside the picture — so "any text at
 * all beats the image" would break the second-most-common way to get an image
 * onto a clipboard. But a genuine *text* copy always carries `text/plain`, and
 * never carries an image file alongside it. So `text/plain` is the precise
 * discriminator between the two, where `text/*` is not.
 */
export function clipboardImageIndex(entries: readonly ClipboardEntry[]): number {
  const mime = (e: ClipboardEntry) => e.type.toLowerCase().split(";")[0].trim();
  if (entries.some((e) => e.kind === "string" && mime(e) === "text/plain")) return -1;
  return entries.findIndex((e) => e.kind === "file" && mime(e).startsWith("image/"));
}
