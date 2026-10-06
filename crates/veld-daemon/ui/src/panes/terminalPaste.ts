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
 * **A newline cannot be carried at all**, in any quoting — see [`isPastable`],
 * which is why one never reaches this function.
 */
export function escapePath(path: string): string {
  let out = "";
  for (const ch of path) {
    if (!isSafePathChar(ch)) out += "\\";
    out += ch;
  }
  return out;
}

/**
 * Why a newline is refused rather than quoted.
 *
 * The first version single-quoted such a path, reasoning that `\` + newline is a
 * shell line continuation which would delete both characters and submit early.
 * That reasoning was **false for this send path**: the payload goes out through
 * `term.paste`, and xterm's `prepareTextForTerminal`
 * (`node_modules/@xterm/xterm/src/browser/Clipboard.ts:14`) runs
 * `text.replace(/\r?\n/g, '\r')` *before* it brackets anything. So the newline
 * never arrives as a newline in any quoting — it arrives as a carriage return,
 * i.e. a submit, and inside the quotes that is worse than the case the quoting
 * was added to prevent.
 *
 * There is no spelling that survives, so the honest answer is to drop the file
 * and tell the user, which is what the caller does with the count.
 */
export function isPastable(path: string): boolean {
  return path.length > 0 && !path.includes("\n") && !path.includes("\r");
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
 *
 * Paths a terminal cannot carry are dropped here too — see [`isPastable`].
 */
export function pathPayload(paths: readonly string[]): string {
  const usable = paths.filter(isPastable);
  if (usable.length === 0) return "";
  return `${usable.map(escapePath).join(" ")} `;
}

/**
 * The pastes a queued prompt with attachments goes in, in order.
 *
 * **The text, then each path as its own paste** — the order a person would
 * produce by typing the prompt and then dropping the files on the pane. That is
 * what makes Claude Code attach them: it recognises an image path when a paste
 * *is* one, so a path buried inside the prompt's own paste would arrive as text
 * the agent may or may not go and read. A blank line closes the prose when paths
 * follow, so a composer that keeps the text keeps the two apart.
 *
 * Paths a terminal cannot carry are left out, as [`pathPayload`] does — the
 * caller has already counted them as failures and said so.
 */
export function promptPastes(text: string, paths: readonly string[]): string[] {
  const files = paths.map((p) => pathPayload([p])).filter((p) => p !== "");
  if (text === "") return files;
  return [files.length > 0 ? `${text}\n\n` : text, ...files];
}

/**
 * The first message an agent takes at launch: the prompt, then a blank line,
 * then each file as an `@"path"` mention.
 *
 * `@` is Claude Code's own "read this file" — it attaches an image *as an image*,
 * and the quotes carry a path with spaces in it (both measured against 2.1.291
 * started with the message as its argument). One string, because it travels as
 * one argument.
 *
 * `null` when a path cannot be written that way — a `"` or a line break in it has
 * no escape inside the quotes. The window then types the prompt and paths in once
 * the agent is up, which carries any path [`isPastable`] accepts.
 */
export function launchPrompt(text: string, paths: readonly string[]): string | null {
  if (paths.some((p) => !isPastable(p) || p.includes('"'))) return null;
  const mentions = paths.map((p) => `@"${p}"`).join(" ");
  const message = [text, mentions].filter((part) => part !== "").join("\n\n");
  return message === "" ? null : message;
}

/** The slice of an xterm `Terminal` [`echoed`] listens on. */
export interface WriteSource {
  onWriteParsed(listener: () => void): { dispose(): void };
}

/**
 * Resolve once the program has answered a paste: output after this call that
 * passes `until`, followed by `quietMs` of none. `true` if it answered, `false`
 * if `capMs` passed first.
 *
 * **Call it before the paste, await it after** — output that lands between the
 * two would otherwise be missed and the wait would run to the cap.
 *
 * This is what a fixed pause after a paste cannot be. A path is echoed back the
 * moment it arrives, and for text that redraw is the answer — `until` defaults
 * to any output. An *image* path is not answered by the first redraw: Claude
 * Code reads, resizes and re-encodes the file before `[Image #N]` appears, and
 * a Return arriving in that window is held and then **discarded** when the
 * read finishes (2.1.291: the paste handler's `B()` clears the held Return,
 * where a text paste's `Z()` replays it). So for an image the caller passes an
 * `until` that looks for the placeholder itself — see [`pasteLanded`]. The
 * quiet window is what lets a redraw that comes in several writes finish
 * before the next keystroke.
 */
export function echoed(
  source: WriteSource,
  quietMs: number,
  capMs: number,
  until: () => boolean = () => true,
): Promise<boolean> {
  return new Promise((done) => {
    let quiet: ReturnType<typeof setTimeout> | undefined;
    const finish = (answered: boolean) => {
      clearTimeout(quiet);
      clearTimeout(cap);
      sub.dispose();
      done(answered);
    };
    const cap = setTimeout(() => finish(false), capMs);
    const sub = source.onWriteParsed(() => {
      clearTimeout(quiet);
      if (!until()) return;
      quiet = setTimeout(() => finish(true), quietMs);
    });
  });
}

/**
 * Whether an agent will read this path as an image rather than as text: the
 * extensions Claude Code's paste handler takes (`/\.(png|jpe?g|gif|webp)$/i`
 * in 2.1.291). Only these are worth waiting on — any other path is answered by
 * its own echo.
 */
export function isImagePath(path: string): boolean {
  return /\.(png|jpe?g|gif|webp)$/i.test(path);
}

function occurrences(haystack: string, needle: string): number {
  if (needle === "") return 0;
  let n = 0;
  for (let i = haystack.indexOf(needle); i !== -1; i = haystack.indexOf(needle, i + needle.length)) n += 1;
  return n;
}

/**
 * Whether the screen shows an image paste has been dealt with, comparing it now
 * against before the paste.
 *
 * Either answer counts: one more image placeholder (`[Image #2]` in Claude
 * Code; matched loosely, as `[image`, so another agent's chip counts too), or
 * the file's name appearing once more — which is what an agent that could not
 * read the image, or does not read images at all, shows instead. A screen
 * showing neither is an agent still reading, and a Return then is lost.
 */
export function pasteLanded(before: string, after: string, path: string): boolean {
  const marker = /\[image\b/gi;
  if ((after.match(marker)?.length ?? 0) > (before.match(marker)?.length ?? 0)) return true;
  const name = path.slice(Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\")) + 1);
  return [name, escapePath(name)].some((n) => occurrences(after, n) > occurrences(before, n));
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

/**
 * The bytes a queued prompt may actually be pasted as.
 *
 * **A prompt is not inert text, and `Terminal.paste` does not make it so.** The
 * installed xterm rewrites `\r?\n` to `\r` and wraps the result in
 * `ESC[200~`/`ESC[201~` — and that is all it does. So a prompt pasted out of
 * terminal output carries whatever was in it:
 *
 * - **A literal `ESC[201~`** ends the bracket early, and everything after it is
 *   read by the program as ordinary keystrokes rather than as pasted text —
 *   which for an agent composer means the tail of the prompt is interpreted as
 *   commands. `ESC[200~` is dropped for the same reason from the other side.
 * - **Any other C0 control** reaches the program as the key it encodes: an
 *   `ESC` from an ANSI colour run starts an escape sequence in the TUI, and a
 *   stray `\x03` is Ctrl-C.
 *
 * `\n` and `\t` survive, and **`\r\n` and a lone `\r` become `\n`** — a
 * multi-line prompt is the case bracketed paste exists for, a tab is legitimate
 * inside pasted prose, and a prompt pasted from a Windows editor or an old
 * terminal capture means its line breaks. Everything else in `Cc` goes, along
 * with the characters that reorder or hide what the composer renders: the bidi
 * embeddings, overrides and isolates, the line and paragraph separators, the BOM
 * and the interlinear annotation marks. That is `is_forbidden` in
 * `crates/veld-daemon/src/desktop.rs` exactly, which refuses the same set in a
 * worktree name for the same reason — and the equivalence is worth keeping
 * literal, because the next reader will act on it.
 *
 * The line-ending pass runs *before* the control strip, which is why the strip
 * class can name `\r` without eating a line break: by then there are none left.
 * Leaving `\r` out of the class instead is what the first version did, and it
 * left the one control byte that means "submit" in the payload while the comment
 * above claimed everything in `Cc` was gone.
 *
 * Stripped rather than refused, unlike a name: the user's words are the point,
 * and losing an escape byte out of the middle of a pasted paragraph costs them
 * nothing they meant to say.
 */
export function promptPayload(prompt: string): string {
  return prompt
    .replace(/\u001b\[20[01]~/g, "")
    .replace(/\r\n?/g, "\n")
    .replace(/[\u0000-\u0008\u000b-\u000d\u000e-\u001f\u007f-\u009f]/g, "")
    .replace(/[\u2028\u2029\u202a-\u202e\u2066-\u2069\ufeff\ufff9-\ufffb]/g, "");
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

/**
 * Whether the window-level guard should swallow this drag.
 *
 * The whole decision of [`guardStrayFileDrops`], extracted so it can be tested:
 * the listener around it is two `addEventListener` calls, and this is the part
 * whose failure loses the page.
 */
export function shouldSwallowDrop(types: readonly string[], defaultPrevented: boolean): boolean {
  // Not a file drag at all (a pane tab, a text selection): not ours to touch.
  if (!isFileDrop(types)) return false;
  // A pane already took it. Deferring here is what keeps the guard from running
  // last and repainting the one working target's `copy` cursor as `none`.
  if (defaultPrevented) return false;
  return true;
}

/**
 * Stop a file dropped **anywhere but a terminal pane** from navigating the page.
 *
 * A browser's default action for a dropped file is to open it — replacing the
 * document. So a drop that misses the pane by a few pixels and lands on a tab
 * strip, the rail, or the gap between them does not merely do nothing: it throws
 * away the whole `/ide`, and with it the view of every running shell. (The
 * sessions survive on the daemon; the page does not.)
 *
 * That hazard is **created by this feature**. Before it there was no reason to
 * drag a file at Veld at all, so nobody was aiming; now the app invites the
 * gesture, and a miss has to be free. The desktop shell blocks the navigation a
 * second way (`will-navigate` in `windows.js`), which is exactly why the guard
 * belongs here too — a plain browser tab has no such backstop.
 *
 * Registered on `window` in the **bubble** phase, so a real drop target has
 * already had the event — and `defaultPrevented` is how this tells the two apart.
 * That check is load-bearing rather than tidy: without it the guard would run
 * *after* the pane's handler and overwrite the `copy` cursor it just set with
 * `none`, so the one place a drop actually works would be the one place the
 * pointer said it would not.
 *
 * Returns an unsubscribe, for symmetry with the other watchers booted beside it.
 */
export function guardStrayFileDrops(): () => void {
  const swallow = (e: DragEvent) => {
    if (!shouldSwallowDrop([...(e.dataTransfer?.types ?? [])], e.defaultPrevented)) return;
    e.preventDefault();
    // Say so with the pointer. Swallowing silently would leave a `copy` cursor
    // over the whole window promising a drop that does nothing.
    if (e.dataTransfer) e.dataTransfer.dropEffect = "none";
  };
  // Both are required: without `dragover` the browser never treats the page as a
  // drop target, and the `drop` event is then never delivered to prevent.
  window.addEventListener("dragover", swallow);
  window.addEventListener("drop", swallow);
  return () => {
    window.removeEventListener("dragover", swallow);
    window.removeEventListener("drop", swallow);
  };
}
