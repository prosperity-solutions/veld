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

/** Whether an event is the command palette's terminal-safe accelerator. */
function isPaletteChord(e: KeyboardEvent): boolean {
  return (e.ctrlKey || e.metaKey) && e.shiftKey && e.code === "KeyP";
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
 * Two things need to be false:
 *
 * - **The palette accelerator** (`Ctrl/⌘+Shift+P`), which must keep propagating
 *   to the window listener in `App.tsx`. xterm cancels the keys it handles
 *   (`preventDefault` + `stopPropagation` on its own textarea), so a focused
 *   terminal would otherwise swallow anything the app binds. `Ctrl+K`
 *   deliberately is not in this list: that one is readline's
 *   kill-to-end-of-line and belongs to the shell.
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
): boolean {
  if (e.type !== "keydown") {
    // Only keydown is acted on, but the matching keyup must not be handed to
    // xterm either — it would arrive with no keydown to match. The Shift+Enter
    // half is conditional for the same reason the keydown is: with the preference
    // off we never claimed the keydown, so swallowing the keyup would drop a
    // release xterm is expecting.
    return !(
      isPaletteChord(e) ||
      isSettingsChord(e) ||
      (shiftEnterNewline && isShiftEnter(e))
    );
  }
  if (isPaletteChord(e) || isSettingsChord(e)) return false;
  if (shiftEnterNewline && isShiftEnter(e)) {
    e.preventDefault();
    send(SHIFT_ENTER_SEQUENCE);
    return false;
  }
  return true;
}
