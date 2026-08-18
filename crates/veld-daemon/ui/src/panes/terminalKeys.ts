/**
 * Key handling that has to happen *before* xterm sees an event.
 *
 * Pure and DOM-free on purpose: it takes the two fields of a keyboard event it
 * cares about and a `send` callback, so the whole thing is exercisable by the
 * existing `environment: "node"` test runner. The alternative — asserting on a
 * live `Terminal` — needs jsdom, and the batch-5 notes are explicit that the
 * untested parts of `panes/` are where the user-visible regressions came from.
 */

/**
 * What Shift+Enter sends.
 *
 * A terminal cannot distinguish Shift+Enter from Enter: the wire protocol has
 * one carriage return and no modifier byte for it. So this is not a preference
 * that can be toggled on the daemon side — it is a key handler that substitutes
 * a *different* sequence, and the sequence has to be one that programs already
 * understand.
 *
 * `ESC CR` is what Claude Code's `/terminal-setup` configures iTerm2 and VS Code
 * to send for exactly this purpose, so matching it means Claude Code's composer
 * (and every coding agent that took the same convention) reads it as "insert a
 * newline" with no setup inside Veld at all.
 *
 * It is also what xterm already sends for Alt+Enter, which is the compatibility
 * trade being made deliberately: a TUI that binds meta-Enter now sees the same
 * bytes from two chords rather than one. Alt+Enter itself is left completely
 * alone, so nothing that worked before stops working.
 */
export const SHIFT_ENTER_SEQUENCE = "\x1b\r";

/**
 * The macOS line-editing chords, and the bytes each one stands in for.
 *
 * **Every mac text field does this, and a terminal only does it if its emulator
 * types the control character for it.** `⌘←` is "caret to start of line" in
 * Cocoa; a pty has no such command, so Ghostty, iTerm2 and Terminal.app all ship
 * a binding that sends `^A` instead — which readline, zsh's ZLE, fish, and every
 * agent composer (Claude Code, Codex) already understand as exactly that. Veld
 * shipped without them, so `⌘←` inside a Claude Code prompt did nothing at all.
 *
 * That "nothing at all" is not an accident of ours: xterm.js's key evaluator
 * has an explicit `if (ev.metaKey) break` in its arrow-key arm, so a ⌘ arrow
 * produces no bytes to send. `⌘⌫` is the odd one out — Backspace's arm checks
 * only Shift and Alt, so ⌘⌫ currently sends a bare `\x7f` and deletes a single
 * character, which is plain `⌫`'s job and no use to anyone.
 *
 * **⌥ is deliberately absent.** xterm already sends `\x1b[1;3D`/`\x1b[1;3C` for
 * ⌥ arrows and `ESC DEL` for `⌥⌫`, and shells bind all three as word motions —
 * so those work today, and re-spelling them as `ESC b`/`ESC f` would only change
 * bytes a TUI may already have bound. Verified by the maintainer on a real mac
 * before this shipped, not assumed.
 *
 * **`⌘↑`/`⌘↓` are absent too**, because there is no byte for them: Cocoa means
 * "start/end of the document" and a shell line has no document. Sending `^A`
 * for them would be inventing a binding rather than matching one.
 */
const MAC_LINE_EDIT: Record<string, string> = {
  // ^A — start of line.
  ArrowLeft: "\x01",
  // ^E — end of line.
  ArrowRight: "\x05",
  // ^U — kill to the start of the line. This is what "delete the whole thing I
  // just typed" means in a shell, and what `⌘⌫` means in a mac text field.
  Backspace: "\x15",
};

/**
 * The bytes a mac line-editing chord stands in for, or `null`.
 *
 * `mac` is passed rather than sniffed so this module stays DOM-free, and gates
 * the whole family: `metaKey` off macOS is the Super/Windows key, which belongs
 * to the window manager and means nothing like "start of line".
 *
 * Every other modifier is excluded rather than ignored — `⌘⇧←` is
 * select-to-start-of-line in a text field and is a chord programs bind for
 * themselves in a terminal, and `⌃⌘←` is a Spaces gesture. Claiming a superset
 * of the chord the user pressed is how a handler eats somebody else's binding.
 */
function macLineEdit(e: KeyboardEvent, mac: boolean): string | null {
  if (!mac || !e.metaKey || e.shiftKey || e.altKey || e.ctrlKey) return null;
  return MAC_LINE_EDIT[e.key] ?? null;
}

/** Whether an event is the Shift+Enter chord and nothing more. */
function isShiftEnter(e: KeyboardEvent): boolean {
  // Other modifiers excluded rather than ignored: Ctrl+Shift+Enter and
  // Alt+Shift+Enter are chords programs bind on their own, and swallowing them
  // here would be this handler quietly eating someone else's keybinding.
  return (
    (e.code === "Enter" || e.code === "NumpadEnter") &&
    e.shiftKey &&
    !e.ctrlKey &&
    !e.altKey &&
    !e.metaKey
  );
}

/**
 * The window-level shortcuts added alongside the Shortcuts overview — focus
 * mode, the IDE/Runs switch, update main, cycling the run selector,
 * start/stop, restart, and opening the overview itself — all of which must reach
 * `App.tsx`'s keydown effect from a focused terminal for the same reason
 * `isSettingsChord` below does: xterm cancels every key it handles, and a
 * focused terminal would otherwise swallow these before the window listener
 * ever saw them.
 *
 * `⌘T`/`⌘W`/`⌘⇧W` are deliberately **not** here: they are Electron *menu*
 * accelerators (`desktop/src/main.js`), handled before web contents — xterm
 * included — ever see the key. The navigation family **is** here, because a
 * `Tab` accelerator does not work: Chromium's focus manager handles `Tab`
 * before the menu layer, so a `Control+Tab` accelerator never consumes the key
 * and the page tabs through its focusable elements anyway. Measured, not
 * assumed.
 *
 * `l`/`x`, not `f`/`v`: the veld feedback overlay claims mod+Shift+F and
 * mod+Shift+V for its own bindings, so `App.tsx` moved focus mode and the
 * view switch off them — see the matching comment there.
 *
 * Matched on `e.key`, never `e.code`, for every letter here — mirroring
 * exactly what `App.tsx`'s own handler tests, since a mismatch here would
 * swallow the key on one layout while the window listener expects a different
 * physical key on another (see the ⌘B comment in `App.tsx` for why `code` is
 * wrong for a letter). The arrow keys have no such layout hazard.
 *
 * `/` allows Shift (it sits behind Shift on German and Spanish layouts — the
 * same class of hazard `App.tsx`'s comma-chord comment already names) but is
 * gated on `e.metaKey` alone, not `mod`: the literal-Ctrl variant is
 * readline's undo (`Ctrl+_`/`Ctrl+/`), the same class of shell binding
 * `Ctrl+K` below is left to. So on Linux/Windows, opening the Shortcuts
 * overview from a focused terminal is reachable everywhere except the
 * terminal itself — a narrower version of the same trade. The
 * `e.shiftKey && e.code === "Digit7"` arm alongside it is the same German/
 * Spanish-layout fallback `App.tsx`'s own `/`-chord comment explains (and
 * flags as an unconfirmed suspected cause, not a verified one).
 */
function isAppShortcutChord(e: KeyboardEvent): boolean {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.shiftKey && !e.altKey) {
    if (["l", "x", "u", "o", "k", "d"].includes(e.key.toLowerCase())) return true;
    if (e.key === "Enter") return true;
  }
  // **The navigation family**: `⌃Tab`/`⌃⇧Tab` (tabs) and `⌥Tab`/`⌥⇧Tab` or
  // `mod+⇧+B/N` (worktrees). A terminal is where most tabs live, so this is the
  // pane in which a dead "next tab" is noticed first.
  if (e.key === "Tab" && !e.metaKey) {
    if (e.ctrlKey && !e.altKey) return true;
    if (e.altKey && !e.ctrlKey) return true;
  }
  // Literal Ctrl: this is the non-macOS worktree chord — see `App.tsx`.
  if (e.ctrlKey && !e.metaKey && e.shiftKey && !e.altKey && ["b", "n"].includes(e.key.toLowerCase()))
    return true;

  // **No arrow chord is claimed, deliberately, and that is a bug fix.**
  //
  // Worktree navigation shipped on `mod`+arrow and this function returned true
  // for it, so xterm never saw the key — which meant `⌘←`/`⌘→` inside a terminal
  // running Claude Code, Codex or anything else expecting iTerm/cmux line
  // editing moved the *rail* instead of the caret. Reported by a user, and
  // correct: `⌘`+arrow is caret-to-line-bounds in every Cocoa text field and is
  // mapped to `^A`/`^E` by every terminal that emulates it. Veld taking it was
  // the anomaly. The whole arrow space now goes back to the terminal and to text
  // fields, which is why the navigation chords above are Tab-shaped.
  if (e.metaKey && !e.altKey && (e.key === "/" || (e.shiftKey && e.code === "Digit7"))) return true;
  return false;
}

/**
 * `⌘,` / `Ctrl+,` — the settings accelerator, which the app binds at the window.
 *
 * A focused terminal swallows every key, so without letting this one propagate the
 * shortcut would work everywhere except the pane people spend the most time in.
 * Matched on `e.key` rather than `e.code` because comma is not on the key named
 * `Comma` on a German or French layout, and not claimed with Shift or Alt held so
 * a shell binding on those still reaches the pty.
 */
function isSettingsChord(e: KeyboardEvent): boolean {
  return (
    (e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key === ","
  );
}

/**
 * xterm's `attachCustomKeyEventHandler` contract: return `true` to let xterm
 * handle the event normally, `false` to make it ignore the event *before* it
 * cancels it.
 *
 * Several things need to be false:
 *
 * - **`isSettingsChord`** (`⌘,`/`Ctrl+,`), which must keep propagating to the
 *   window listener in `App.tsx`. xterm cancels the keys it handles
 *   (`preventDefault` + `stopPropagation` on its own textarea), so a focused
 *   terminal would otherwise swallow anything the app binds.
 * - **Every window-level shortcut** (`isAppShortcutChord`) — focus mode, the
 *   view switch, update main, run cycling, start/stop, restart, and opening the
 *   Shortcuts overview, **and the Tab-shaped navigation family** — for the same
 *   reason. Navigation is page-dispatched, so it genuinely needs this; only
 *   `⌘T`/`⌘W`/`⌘⇧W` are menu accelerators and bypass xterm on their own. Arrows
 *   are absent because they belong to the terminal: `⌘`+arrow is the caret
 *   motion Claude Code and Codex expect — and since this file also *implements*
 *   that motion (see [`MAC_LINE_EDIT`]), claiming an arrow here would now be
 *   this handler taking a chord from itself.
 *   `Ctrl+K` (the command palette's other chord) deliberately
 *   is not among them: that one is readline's kill-to-end-of-line and
 *   belongs to the shell, so the palette is reachable from a focused
 *   terminal only via ⌘K, not Ctrl+K.
 * - **The macOS line-editing chords** ([`MAC_LINE_EDIT`] — `⌘←`, `⌘→`, `⌘⌫`),
 *   which this handler answers itself by sending the control character every
 *   other mac terminal emulator sends for them. Note the difference from the
 *   entries above: these are *not* forwarded to the window, they are
 *   **substituted** — the terminal is where they belong, and the app binds
 *   nothing on them.
 * - **Shift+Enter**, which this handler answers itself by sending
 *   [`SHIFT_ENTER_SEQUENCE`]. `preventDefault` here is load-bearing: without it
 *   the browser still delivers the key to xterm's hidden textarea and the shell
 *   gets a bare `CR` as well, i.e. the newline *and* a submit.
 */
export function handleKeyEvent(
  e: KeyboardEvent,
  send: (data: string) => void,
  /**
   * Whether Shift+Enter should send [`SHIFT_ENTER_SEQUENCE`]
   * (`terminal.shiftEnterNewline`). When off, Shift+Enter is handed to xterm like
   * any other key, which is what a TUI binding meta-Enter needs.
   *
   * Defaults to on so a caller that has not read settings behaves like the
   * release that shipped this hardcoded.
   */
  shiftEnterNewline = true,
  /**
   * Whether this is a Mac, enabling the [`MAC_LINE_EDIT`] chords. Passed rather
   * than sniffed so this module stays DOM-free; `terminalHost.ts` reads it from
   * `isMac()` in the shortcuts registry, the same one the overview renders with.
   *
   * Defaults to `false` — the platform where none of these chords exist — so a
   * caller that has not been updated keeps exactly the behaviour it had.
   */
  mac = false,
): boolean {
  if (e.type !== "keydown") {
    // Only keydown is acted on, but the matching keyup must not be handed to
    // xterm either — it would arrive with no keydown to match. The Shift+Enter
    // half is conditional for the same reason the keydown is: with the preference
    // off we never claimed the keydown, so swallowing the keyup would drop a
    // release xterm is expecting.
    return !(
      isSettingsChord(e) ||
      isAppShortcutChord(e) ||
      macLineEdit(e, mac) !== null ||
      (shiftEnterNewline && isShiftEnter(e))
    );
  }
  if (isSettingsChord(e) || isAppShortcutChord(e)) return false;
  const lineEdit = macLineEdit(e, mac);
  if (lineEdit !== null) {
    // `preventDefault` for the same reason Shift+Enter needs it: without it the
    // browser still delivers the key to xterm's hidden textarea. `⌘⌫` is the
    // one that would actually misfire — xterm answers it with a bare `\x7f`, so
    // the shell would get "kill the line" *and* "delete one character".
    e.preventDefault();
    send(lineEdit);
    return false;
  }
  if (shiftEnterNewline && isShiftEnter(e)) {
    e.preventDefault();
    send(SHIFT_ENTER_SEQUENCE);
    return false;
  }
  return true;
}
