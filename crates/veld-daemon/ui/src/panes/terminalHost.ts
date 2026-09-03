/**
 * Live terminal sessions, owned outside React.
 *
 * A terminal is not re-creatable state: unmounting one closes its socket,
 * which kills the shell and everything running in it. React unmounts freely —
 * on a tab switch, and on every worktree switch, because each worktree has its
 * own layout. So the xterm instance and its container element live in a
 * module-level registry keyed by tab id, and the React component only
 * *reparents* that element into itself. Switching away and back re-adopts the
 * same shell with its scrollback intact.
 *
 * The registry is the reason `PaneTab.id` must be unique for the life of the
 * page (see `newTerminalId`): a reused id would adopt someone else's shell.
 */

import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { api, type PaneLaunchMode } from "../api";
import { inbox, isOsc9Notification, parseOsc133 } from "../inbox/inbox";
import { ANSI_DARK, ANSI_LIGHT } from "../shared/ansi";
import { notifyError, notifyRedirect } from "../shared/notify";
import { terminalPrefs, type TerminalPrefs } from "../shared/settings";
import { chromeless, layoutSlot, pathForFile, windowSeed } from "../shell";
import { isMac } from "../shortcuts/registry";
import {
  type PaneMount,
  parseLayouts,
  type RestartKind,
  shouldCloseOnExit,
  startPlanFor,
  storedTerminalIds,
  terminalIds,
} from "./model";
import { handleKeyEvent } from "./terminalKeys";
import {
  clipboardImageIndex,
  clipboardImageName,
  isFileDrop,
  isPastable,
  pathPayload,
} from "./terminalPaste";

/**
 * Terminal ids this page expects to *resume*.
 *
 * It answers a question the daemon's `resumed: false` cannot: was this a
 * brand-new terminal, or one whose shell we expected to still be there? Without
 * it, a lost shell is silently replaced by an empty prompt.
 *
 * **It grows rather than being captured once**, because the answer no longer
 * lives in this page's storage. A main window's panes come from the daemon, one
 * worktree at a time as they are opened, so `noteExpectedResumes` is called with
 * each layout that arrives. Which is the fix for the case the whole change
 * exists for: a browser tab had no store at all, so every terminal in a worktree
 * the desktop app was running looked brand new to it — and it spawned a second
 * shell beside the one that was already there instead of re-attaching.
 *
 * The seed and the detached window's slot store are still read at module load:
 * a detached window's tabs are its own, and a window opened by dragging a
 * terminal out of another one has no layout to fetch.
 */
const EXPECTED_RESUMES: Set<string> = (() => {
  try {
    // `windowSeed` for the reason it exists: a window opened by detaching a
    // terminal has no store yet, so without it every transferred shell would
    // look brand new — and a transfer that arrived to find its shell gone would
    // say nothing at all, which is the case this set exists to catch.
    return new Set([
      ...storedTerminalIds(layoutSlot, chromeless),
      ...Object.values(parseLayouts(windowSeed)).flatMap(terminalIds),
    ]);
  } catch {
    return new Set<string>();
  }
})();

/**
 * Note that these shells were expected to be running when this page found them.
 *
 * Called with every layout the daemon hands over, **before** the panes it names
 * are rendered — a terminal only connects once it has mounted, which is at
 * least one commit after the layout reached React state, so the set is
 * populated by the time anything reads it. Additive on purpose: a page visits
 * several worktrees, and a shell does not stop being expected because the user
 * looked at something else.
 */
export function noteExpectedResumes(ids: string[]): void {
  for (const id of ids) EXPECTED_RESUMES.add(id);
}

export type TerminalState =
  | "absent"
  | "idle"
  | "connecting"
  | "live"
  | "ended"
  | "error";

interface Session {
  id: string;
  worktreeId: number;
  /** The `ide.panes[].id` this pane runs, for a config-declared pane. A plain
   *  terminal leaves it undefined and runs a login shell. */
  spec?: string;
  /**
   * Whether this session is known to have a token — i.e. its command actually
   * started under one, in *this* window.
   *
   * `paneSessions` is fetched once when a worktree is selected and never
   * again, so a pane that launched afterwards is missing from it. Without this,
   * such a pane hitting `error` (a daemon restart mid-`veld update`, a reaped
   * holder, a dropped socket) offered only "Start fresh" — which mints a new
   * token and abandons the conversation, the exact loss the resume path exists
   * to prevent.
   */
  launched: boolean;
  /** The pane's `close_on_exit`, captured at mount.
   *
   *  Held here rather than read in the renderer because a tab that is not the
   *  active one has **no component mounted** — its session keeps running and
   *  keeps receiving frames, so an exit observed there has to be acted on by
   *  the host or it is silently deferred until the user next clicks the tab,
   *  which reads as "clicking the tab closed my pane". */
  closeOnExit: boolean;
  /** Whether this pane's process may rename its tab with an OSC 0/2 title.
   *  A plain terminal (no `spec`) always may; a config pane only when its
   *  `allow_terminal_renaming` is set. Captured at mount like `closeOnExit`,
   *  for the same reason: the decision belongs to the host, which runs even
   *  while no pane component is mounted. */
  allowTerminalRenaming: boolean;
  term: Terminal;
  fit: FitAddon;
  /** Detached until a pane mounts it; never destroyed by a mere unmount. */
  container: HTMLDivElement;
  /** `term.open()` measures the font, so it can only run once the container
   *  is actually in the document. */
  opened: boolean;
  ws: WebSocket | null;
  state: TerminalState;
  detail: string;
  /** Set while a user-requested restart is in flight — see [`RestartKind`].
   *  Cleared by [`setState`] the moment the session leaves `connecting`, so it
   *  can never outlive the restart it describes. */
  restarting: RestartKind | null;
  /**
   * The status the session's process exited with, or `null` if it has not
   * exited *as itself*.
   *
   * Only the `exit` control frame sets this. A takeover and a dropped socket
   * also land in `ended`/`error`, and neither is an exit — reading a code out of
   * those is how "the pane closes when its command finishes cleanly" would turn
   * into "the pane vanishes when another window steals it".
   */
  exitCode: number | null;
  observer: ResizeObserver | null;
  /** Generation counter: a socket from a superseded connect attempt must not
   *  write into a terminal that has since been restarted. */
  generation: number;
  /**
   * Auto-reconnect budget remaining in the current retry cycle, or `null` when
   * not retrying (a healthy connection, or auto-reconnect disabled).
   *
   * `null` rather than `0` because the two need to be told apart: "not in a
   * cycle" must re-arm from the setting, while "budget exhausted" must stay
   * dead and wait for a click. See [`maybeAutoReconnect`].
   */
  reconnectLeft: number | null;
  /** Whether the cycle's first attempt has fired, so the second and later wait
   *  the backoff rather than the near-immediate first delay again. */
  reconnectFiredFirst: boolean;
  /** Handle for the pending reconnect timer, so a manual restart or reconnect
   *  cancels it instead of racing the next attempt. */
  reconnectTimer: number | null;
  /**
   * True while replayed scrollback is being fed to the terminal.
   *
   * Nothing the terminal tries to *send* may leave while this is set. Recorded
   * output can contain queries the shell once made (device attributes, cursor
   * position, colour); parsing them again makes xterm answer them again, and
   * that answer reaches a shell that asked nothing — arriving as keystrokes.
   * That is where the stray `1;2c` at the prompt after a reload came from.
   */
  replaying: boolean;
  /** Replay chunks handed to `term.write` that it has not finished parsing.
   *  xterm parses asynchronously, so `replay_end` alone is too early to
   *  un-gate: the queries are answered during parsing, not on receipt. */
  replayWrites: number;
  /** Whether `replay_end` has arrived. */
  replayEnded: boolean;
  listeners: Set<() => void>;
}

const sessions = new Map<string, Session>();
let currentTheme: "dark" | "light" = "dark";

/**
 * The terminal preferences every session is constructed with and re-styled to.
 *
 * Module-level rather than passed per call because `ensure()` is reached from a
 * React render path that does not have the settings document, and threading it
 * through would put a preference on the identity of a cached session. The app
 * publishes changes with `applyTerminalPrefs`.
 *
 * `null` until the app publishes: `ensure()` falls back to `terminalPrefs({})`,
 * which is the previous release's behaviour rather than a second copy of the
 * defaults. In practice the app does not mount a terminal before settings
 * resolve — this is the belt, not the braces.
 */
let currentPrefs: TerminalPrefs | null = null;

function prefs(): TerminalPrefs {
  return currentPrefs ?? terminalPrefs({});
}

// The ANSI palettes live in `shared/ansi.ts`, which is also what the logs panel
// renders colour with. One owner: the same output shown in a shell and in the
// logs would otherwise be two different sets of colours, and the divergence would
// read as a bug in whichever one you saw second. The background/foreground still
// come from the theme tokens below, so a terminal matches the panel around it.

/** Read a design token, since xterm needs a literal colour string. Falls back
 *  when the variable is missing or holds a value xterm can't parse (the
 *  status colours are `oklch()`, which its parser rejects). */
function token(name: string, fallback: string): string {
  const v = getComputedStyle(document.body).getPropertyValue(name).trim();
  return v && (v.startsWith("#") || v.startsWith("rgb")) ? v : fallback;
}

function xtermTheme() {
  const dark = currentTheme === "dark";
  const fg = token("--text", dark ? "#e7e9ec" : "#171a1d");
  // `--term-bg`, not `--bg`: the terminal gets its own surface so the light
  // theme can be near-white for monospace contrast without lightening the app
  // chrome around it. Must match `.term-pane`'s background or the pane shows a
  // seam wherever the grid doesn't reach the edge.
  const bg = token("--term-bg", dark ? "#0d0e10" : "#ffffff");
  return {
    ...(dark ? ANSI_DARK : ANSI_LIGHT),
    background: bg,
    foreground: fg,
    cursor: token("--accent", dark ? "#3fbf7f" : "#28965f"),
    cursorAccent: bg,
    // xterm needs alpha on the selection or it hides the glyphs under it.
    selectionBackground: dark ? "rgba(90, 162, 224, 0.35)" : "rgba(47, 111, 181, 0.25)",
  };
}

/** Re-theme every live terminal. Called when the app's theme changes; xterm
 *  can't inherit CSS variables, so this is the only way its colours track. */
export function applyTerminalTheme(theme: "dark" | "light"): void {
  currentTheme = theme;
  for (const s of sessions.values()) {
    s.term.options.theme = xtermTheme();
  }
}

/**
 * Ligatures, as a CSS font-feature rule on the host element.
 *
 * Not an xterm option (there isn't one) and not `@xterm/addon-ligatures`, which
 * parses the font binary through `font-finder`/`font-ligatures` and so needs
 * Node's `fs` — unavailable on the browser path, which is the same bundle the
 * desktop shell loads by URL.
 *
 * Both halves are written explicitly, and the `off` half is the load-bearing one.
 * xterm's DOM renderer always sets a `letter-spacing` on its row container to
 * absorb DPR rounding (`DomRenderer._setDefaultSpacing`), and a non-zero
 * letter-spacing makes Blink and WebKit drop ligatures on their own. So "on" has
 * to override that suppression, and "off" cannot simply be `normal` — at a
 * font/DPR combination where the residual lands on exactly 0, `normal` would let
 * ligatures through with the switch off.
 *
 * `calt` is the whole switch; `liga` and `clig` are held at 0 in *both* halves.
 * Programming fonts put these substitutions in `calt` — both bundled faces ship
 * `calt` and neither has a `liga` table — and pixel-comparing the four
 * combinations at the renderer's own metrics says so outright: `liga`+`clig`
 * with `calt` off renders identically to everything off, and `calt` alone
 * renders identically to all three on. Naming them bought nothing.
 *
 * Naming them also cost something. `liga` is where a text-derived monospace font
 * keeps its `fi`/`fl`/`ff` ligatures, and *those* are not width-preserving: with
 * the letter-spacing taken away, `office difficult fluffy affix` measures 963px
 * in Menlo against 1117px unligated. The non-zero letter-spacing above is what
 * suppresses them today — but it is a residual derived from font metrics, and
 * the font/DPR combination that lands it on exactly 0 is the same edge the `off`
 * half is written out for. There, tagging `liga` would trade a broken grid for
 * glyphs no font we offer even has.
 *
 * Set on the host element rather than on `.xterm`, so it also inherits into the
 * renderer's own hidden measuring container — measurement and painting must not
 * disagree about which glyphs they are looking at. Enabling this changes no
 * advance width in either bundled font, and a GSUB dump says why rather than
 * leaving it to luck: `calt` reaches only `SingleSubst` lookups in both faces —
 * neither contains a `LigatureSubst` at all — so `!=` stays two glyphs, a left
 * half and a right half, instead of collapsing into one. The cell box and the
 * grid are unaffected; only the painted shapes differ.
 */
const LIGATURES_ON = '"liga" 0, "clig" 0, "calt" 1';
const LIGATURES_OFF = '"liga" 0, "clig" 0, "calt" 0';

function applyLigatures(container: HTMLElement, on: boolean): void {
  container.style.fontFeatureSettings = on ? LIGATURES_ON : LIGATURES_OFF;
}

/**
 * Re-style every live terminal and re-measure each one.
 *
 * **Every** session, not only the visible one. Hidden terminals stay in the
 * registry and keep running (that is the point of the session model), so a font
 * change that only touched the focused pane would leave every other terminal
 * rendering at the old metrics with a grid sized for them — and the mismatch only
 * becomes visible when you switch to it, by which time the cause is three actions
 * ago.
 *
 * A font change alters the cell box, so the grid has to be recomputed and the new
 * dimensions sent to the pty (`TIOCSWINSZ`) — otherwise the shell keeps wrapping
 * to the old width. `requestFit` handles both, and skips a container that is not
 * laid out yet; those get their fit from the mount/ResizeObserver path instead.
 */
export function applyTerminalPrefs(next: TerminalPrefs): void {
  const before = currentPrefs;
  currentPrefs = next;
  // Any change re-fits, rather than a hand-maintained list of the prefs that
  // affect the cell box. That list was `fontSize` and `fontFamily`, and the next
  // metric added — line height, letter spacing — would have set the xterm option
  // and skipped the fit, leaving every open shell wrapping at the old width with
  // nothing to catch it. A fit is rAF-deferred and bails on an unlaid-out
  // container, so over-fitting costs a frame and under-fitting costs a wrong grid.
  const changed = !before || JSON.stringify(before) !== JSON.stringify(next);
  for (const s of sessions.values()) {
    s.term.options.fontSize = next.fontSize;
    s.term.options.fontFamily = next.fontFamily;
    s.term.options.cursorStyle = next.cursorStyle;
    s.term.options.cursorBlink = next.cursorBlink;
    applyLigatures(s.container, next.ligatures);
    // Lowering scrollback drops the oldest lines immediately, which is what the
    // settings copy promises.
    s.term.options.scrollback = next.scrollback;
    if (changed) requestFit(s);
  }
}

function notify(s: Session): void {
  for (const fn of s.listeners) fn();
}

function setState(s: Session, state: TerminalState, detail = ""): void {
  s.state = state;
  s.detail = detail;
  // A restart is over the moment the session stops connecting, whichever way it
  // went — live, ended, or a spawn that failed. Clearing it here rather than at
  // each of those call sites is what stops a "Restarting…" card outliving a
  // restart that already failed and covering the error that says why.
  if (state !== "connecting") s.restarting = null;
  notify(s);
}

/** How long a transient note on a still-live terminal stays on the chip. */
const TRANSIENT_MS = 6000;

/**
 * Report something about a terminal that is still running, on the pane's chip
 * rather than in the terminal's own output — see [`writeNotice`] for why
 * nothing may be written into a live shell's screen.
 */
function flash(s: Session, detail: string): void {
  setState(s, "live", detail);
  window.setTimeout(() => {
    // Only clear our own note: the session may have ended, or flashed
    // something newer, while this was pending.
    if (s.state === "live" && s.detail === detail) setState(s, "live");
  }, TRANSIENT_MS);
}

/**
 * Listeners for sessions that don't exist yet.
 *
 * Subscribing before the session exists is the normal case for anything that
 * works from the layout rather than from a mounted pane — a top-bar indicator,
 * a status column. Returning a silent no-op for an unknown id (as this did)
 * makes such a caller receive nothing, forever, with no error to notice.
 */
const pending = new Map<string, Set<() => void>>();

/** Subscribe to a session's connection state. Returns an unsubscribe.
 *
 *  Safe to call before the session exists: the listener is held and attached
 *  when it is created. */
export function subscribeTerminal(id: string, fn: () => void): () => void {
  const s = sessions.get(id);
  if (s) {
    s.listeners.add(fn);
    return () => s.listeners.delete(fn);
  }
  const waiting = pending.get(id) ?? new Set();
  waiting.add(fn);
  pending.set(id, waiting);
  return () => {
    waiting.delete(fn);
    if (waiting.size === 0) pending.delete(id);
    sessions.get(id)?.listeners.delete(fn);
  };
}

/**
 * A session's state, or `absent` when no shell has been started for this tab
 * yet (nothing has mounted it).
 *
 * `absent` is distinct from `connecting` on purpose: reporting an unopened
 * terminal as "connecting…" makes every not-yet-mounted tab look like it is
 * hanging.
 */
export function terminalStatus(id: string): {
  state: TerminalState;
  detail: string;
  exitCode: number | null;
  /** Whether this session ran under a token in this window — see
   *  [`Session.launched`]. */
  launched: boolean;
  /** Non-null while a user-requested restart is in flight — see
   *  [`RestartKind`]. Only ever set alongside `connecting`. */
  restarting: RestartKind | null;
} {
  const s = sessions.get(id);
  return s
    ? {
        state: s.state,
        detail: s.detail,
        exitCode: s.exitCode,
        launched: s.launched,
        restarting: s.restarting,
      }
    : {
        state: "absent",
        detail: "",
        exitCode: null,
        launched: false,
        restarting: null,
      };
}

/** How a session should start, or `null` to sit idle until the user says. */
/**
 * Called when a config pane's command exits cleanly and the pane asked to be
 * tidied away. Registered once by the app, which owns the layout.
 *
 * A callback rather than a React effect because the pane that exited may not be
 * mounted — see [`Session.closeOnExit`].
 */
let paneCloseHandler: ((id: string, worktreeId: number) => void) | null = null;

export function setPaneCloseHandler(
  fn: ((id: string, worktreeId: number) => void) | null,
): void {
  paneCloseHandler = fn;
}

// One AudioContext, reused across every terminal — creating one per bell (and
// per terminal) is the kind of churn that gets autoplay-policy-throttled. A
// short 800 Hz tone is the classic terminal bell; Web Audio needs no asset.
let bellCtx: AudioContext | null = null;

/** Whether focus mode is currently silencing the bell — published the same
 *  way as `currentPrefs`, since `playBell` is reached from module-level event
 *  handlers with no access to the settings document. */
let bellSuppressed = false;

/** Called alongside `applyTerminalPrefs` whenever the settings document
 *  changes, so `playBell` always reads the latest focus-mode state. */
export function setBellSuppressed(next: boolean): void {
  bellSuppressed = next;
}

/** Ring the terminal bell as a short tone. Best-effort: a browser that
 *  autoplay-policies audio into silence loses the sound, never the terminal.
 *  Volume is the user's `terminal.bellVolume` percentage (0–100). Silenced
 *  entirely while focus mode is suppressing the bell. */
function playBell(): void {
  if (bellSuppressed) return;
  try {
    bellCtx ??= new AudioContext();
    const ctx = bellCtx;
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = 800;
    const t = ctx.currentTime;
    // Percentage → peak gain. A full-scale sine is harsh, so 0.5 at 100% is
    // loud without clipping; 0 stays silent.
    const peak = (prefs().bellVolume / 100) * 0.5;
    gain.gain.setValueAtTime(peak, t);
    gain.gain.exponentialRampToValueAtTime(Math.max(peak * 0.01, 0.0001), t + 0.15);
    osc.connect(gain);
    gain.connect(ctx.destination);
    osc.start(t);
    osc.stop(t + 0.15);
  } catch {
    // Silent bell is acceptable.
  }
}

/**
 * Most files one drop may carry.
 *
 * Each one costs a request the daemon buffers whole (up to `MAX_PASTE_BYTES`),
 * and a drop is the one gesture that can hand over a whole folder by accident.
 * Twenty is well past any deliberate drop and far short of a directory.
 */
const MAX_DROP_FILES = 20;

/**
 * Files a terminal pane accepts: a drop onto it, and an image pasted into it.
 *
 * **Both end as a path typed at the prompt**, never as bytes on the wire — a pty
 * carries a byte stream, so there is no protocol for handing a program a
 * picture. Every terminal emulator that "supports" dropping an image types its
 * path, and every coding agent (Claude Code, Codex) reads an image path as an
 * image. `terminalPaste.ts` holds the rules; this holds the listeners and the
 * one asynchronous step.
 *
 * Where the path comes from differs by shell, and only here:
 *
 * - **Desktop, dropped file** — Electron resolves the real path
 *   (`webUtils.getPathForFile`). Nothing is copied; the terminal points at the
 *   file the user already has.
 * - **Browser tab, dropped file** — the File API withholds the path by design,
 *   so the bytes are uploaded and the daemon's copy is what gets typed.
 * - **Either shell, pasted image** — a screenshot is bytes with no path anywhere,
 *   so it is uploaded. A copied image *file* is the exception: it does have a
 *   path, and in the desktop app that real path is used rather than a second
 *   copy of something the user already has.
 *
 * Two corners are decided rather than handled, and named here because the review
 * that found them will otherwise find them again:
 *
 * - **⌘V acts on images only**, and everything else is handed to xterm exactly as
 *   before — whatever xterm then makes of it. Two earlier versions of this note
 *   claimed a copied *document* pastes its name, which is a guess about what the
 *   browser puts on the clipboard that nobody here has measured; the reviewable
 *   fact is only that this handler does not touch it. Dropping that same file
 *   does insert its path, so a drop is the gesture to reach for.
 * - **A dropped directory** is typed as a path in the desktop app (which is
 *   useful: `ls`, `cd`) and refused in a browser tab, where it has no readable
 *   bytes and the daemon answers "empty file". The toast now carries that
 *   message rather than a generic one.
 *
 * Listeners go on the session's own container, which outlives every mount: a
 * pane moved between docks or pulled into another window keeps this without
 * re-registering, the same property the terminal itself has.
 *
 * Nothing is written to the socket directly — everything goes through
 * `term.paste`, so it reaches the pty by the same route as typing and through
 * the same `canSend` gate. `canSend` is passed in rather than re-derived so the
 * gesture can be *refused out loud* when the terminal cannot accept input,
 * instead of being swallowed by that gate after the file is already on disk.
 */
function attachFileInput(s: Session, canSend: () => boolean): void {
  /**
   * Put the paths in the terminal, and say so when one of them could not be had.
   *
   * **`term.paste`, not `send` — and that distinction is the whole feature.** A
   * coding agent decides whether a path is a *file it should attach* or merely
   * text by whether it arrived as a paste: Claude Code attaches an image path
   * pasted into its composer as `[Image #1]`, and leaves the identical characters
   * as literal text when they are typed one at a time. Measured both ways against
   * a real Claude Code — typing `…/red.png` shows the path, pasting it shows the
   * image — and it is the same route cmux and iTerm2 take.
   *
   * `paste` is also the only correct way to send this at all: it wraps the text
   * in bracketed-paste markers **when, and only when, the program has enabled
   * that mode** (DECSET 2004). Emitting the markers unconditionally would spray
   * `[200~` into any program that had not asked for them.
   *
   * A non-image path is unaffected either way: an agent finds no image extension
   * and keeps the text, which is exactly what dropping a source file should do.
   */
  const typePaths = (paths: string[], failures: number, cause?: unknown) => {
    const payload = pathPayload(paths);
    if (payload) {
      // **Refuse rather than drop it on the floor.** `term.paste` reaches the
      // socket through the same `canSend` gate as typing, so during a scrollback
      // replay or a reconnect the payload is silently discarded — and the file
      // has already been uploaded and written to disk by then, so the gesture
      // vanishes with nothing said. Two review angles found this independently.
      if (!canSend()) {
        notifyError(
          "Adding a file to the terminal",
          new Error("the terminal is not ready for input yet — try again in a moment"),
        );
        // Falls through rather than returning: a drop can be both un-sendable
        // *and* have had files fail to upload, and reporting only the first
        // would silently discard the second.
      } else {
        s.term.paste(payload);
      }
    }
    if (failures > 0) {
      notifyError(
        failures === 1 ? "Adding a file to the terminal" : `Adding ${failures} files to the terminal`,
        // The daemon's own message where there is one ("file is too large" for a
        // dropped video, "empty file", "no such terminal session"); the generic
        // line only when nothing threw. Discarding the real cause made the most
        // likely failure of all — a file past the 32 MB cap — unreadable.
        cause ?? new Error("could not be read"),
      );
    }
  };

  /**
   * Resolve one dropped file to a path — the shell's own, or the daemon's copy.
   *
   * Returns the failure alongside rather than throwing: one unreadable file in a
   * multi-file drop must not cost the user the others, but the reason still has
   * to reach them. Returned rather than stashed in a shared variable, so two
   * overlapping drops cannot clear each other's cause.
   */
  const resolve = async (file: File): Promise<[string | null, unknown]> => {
    const local = pathForFile(file);
    if (local) return [local, undefined];
    try {
      return [await api.ptyPasteFile(s.id, file, file.name), undefined];
    } catch (e) {
      console.warn("veld: could not upload a dropped file", e);
      return [null, e];
    }
  };

  s.container.addEventListener("dragover", (e) => {
    if (!isFileDrop([...(e.dataTransfer?.types ?? [])])) return;
    // Without preventDefault the browser refuses the drop outright — and in the
    // desktop app it would then *navigate* the window to the file instead.
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
  });

  s.container.addEventListener("drop", (e) => {
    if (!isFileDrop([...(e.dataTransfer?.types ?? [])])) return;
    e.preventDefault();
    // Ordered, because a multi-file drop types its paths in the order the user
    // sees them; `Promise.all` preserves it where a race would not.
    const files = [...(e.dataTransfer?.files ?? [])];
    if (files.length === 0) return;
    // **Bounded.** One `fetch` per file, each allowed 32 MB and each buffered
    // whole on the daemon side, so a dropped folder of screenshots is the
    // ordinary way to ask for a gigabyte at once. Nothing downstream caps the
    // aggregate — `MAX_PASTE_BYTES` is per request and the prune is age-based.
    if (files.length > MAX_DROP_FILES) {
      notifyError(
        "Adding files to the terminal",
        new Error(`too many files at once — ${files.length} dropped, ${MAX_DROP_FILES} is the limit`),
      );
      return;
    }
    void (async () => {
      // **One at a time.** `Promise.all` started every upload at once, so a drop
      // of `MAX_DROP_FILES` large files asked the daemon to buffer up to
      // 20 x 32 MB simultaneously — the per-request cap bounds a request, not a
      // gesture. Sequential is also simpler than the bounded-concurrency version
      // and loses nothing: the order is required anyway, since the paths are
      // typed in the order the user dropped them.
      const resolved: string[] = [];
      let cause: unknown;
      for (const file of files) {
        const [path, err] = await resolve(file);
        if (path !== null) resolved.push(path);
        // The first real reason, kept per drop rather than per session: a second
        // drop starting while this one is in flight would otherwise clear it and
        // send this drop's toast back to the generic message.
        else cause ??= err;
      }
      // A path a terminal cannot carry — a newline in the name — is dropped by
      // `pathPayload`, so it is counted as a failure here rather than vanishing.
      const carried = resolved.filter(isPastable);
      typePaths(carried, files.length - carried.length, cause);
    })();
  });

  // **Capture phase.** The event's target is xterm's own hidden textarea, which
  // is a descendant of this container — so capturing is what runs this *before*
  // xterm's handler rather than after it has already pasted.
  s.container.addEventListener(
    "paste",
    (e) => {
      const data = e.clipboardData;
      if (!data) return;
      const index = clipboardImageIndex([...data.items].map((i) => ({ kind: i.kind, type: i.type })));
      // -1 is the overwhelmingly common case — ordinary text — and it must reach
      // xterm untouched. Doing nothing here is what lets it.
      if (index === -1) return;
      const file = data.items[index].getAsFile();
      if (!file) return;
      e.preventDefault();
      e.stopPropagation();
      // **Anything that got here with a real path should use it.** A screenshot
      // has none — it is bytes — and falls through to the upload below. A copied
      // image *file* may have one, and uploading it would write a second copy of
      // something the user already has and hand the agent the copy's path instead
      // of the original's. Asking costs one call and is right either way, so this
      // does not depend on knowing which flavours a given browser reports.
      const local = pathForFile(file);
      if (local) {
        typePaths([local], 0);
        return;
      }
      // A clipboard image proper has no name of its own; the daemon re-sanitises
      // whatever it is given anyway, so this only decides readability.
      const name = file.name || clipboardImageName(file.type);
      void api.ptyPasteFile(s.id, file, name).then(
        (path) => typePaths([path], 0),
        (err) => notifyError("Pasting an image into the terminal", err),
      );
    },
    true,
  );
}

/** Create the session (idempotent) without touching the DOM. */
function ensure(
  id: string,
  worktreeId: number,
  pane: PaneMount | undefined,
): { session: Session; created: boolean } {
  const existing = sessions.get(id);
  if (existing) {
    // The daemon refuses a cross-worktree resume with a 409; mirror that here,
    // because a stale or hand-edited sessionStorage holding one tab id under two
    // worktrees would otherwise show worktree A's shell inside worktree B's pane
    // with nothing said. Loud, since it means our own state is inconsistent.
    if (existing.worktreeId !== worktreeId) {
      throw new Error(
        `terminal ${id} belongs to worktree ${existing.worktreeId}, not ${worktreeId}`,
      );
    }
    return { session: existing, created: false };
  }

  const p = prefs();
  const term = new Terminal({
    allowProposedApi: true,
    cursorBlink: p.cursorBlink,
    cursorStyle: p.cursorStyle,
    fontFamily: p.fontFamily,
    fontSize: p.fontSize,
    // The shell's own scrollback plus room for a verbose build; settable because
    // "room for a verbose build" is a different number for everyone. A line costs
    // 12 bytes per column in xterm, so the default is ~14 MB per terminal at 120
    // columns — see DEFAULT_SCROLLBACK in veld-core's settings module.
    scrollback: p.scrollback,
    theme: xtermTheme(),
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  // URLs in the output become links. The addon is worth the dependency for one
  // reason: it stitches a URL back together across the rows a terminal *wrapped*
  // it onto (`isWrapped`), and a login URL is always long enough to wrap. Its
  // regex is http(s)-only and it re-validates with `new URL()`, which is the same
  // gate a browser pane applies on the way in.
  term.loadAddon(
    new WebLinksAddon((event, uri) => {
      void activateLink(id, uri, event);
    }),
  );

  // Attached below, once the session object the handler sends through exists.

  const container = document.createElement("div");
  container.className = "term-host";
  applyLigatures(container, p.ligatures);

  const s: Session = {
    id,
    worktreeId,
    spec: pane?.spec,
    term,
    fit,
    container,
    opened: false,
    ws: null,
    exitCode: null,
    launched: false,
    closeOnExit: pane?.closeOnExit ?? false,
    // A plain terminal always adopts its OSC title; a config pane only when
    // the project opted in. `spec` is the discriminator: undefined means login
    // shell, and login shells are free to rename themselves.
    allowTerminalRenaming:
      pane?.spec === undefined || (pane?.allowTerminalRenaming ?? false),
    state: "connecting",
    detail: "",
    restarting: null,
    observer: null,
    generation: 0,
    reconnectLeft: null,
    reconnectFiredFirst: false,
    reconnectTimer: null,
    replaying: false,
    replayWrites: 0,
    replayEnded: false,
    listeners: new Set(),
  };
  sessions.set(id, s);
  // Tell the inbox which worktree this pane belongs to before any signal arrives. The
  // daemon's relay carries a worktree id of its own, so this is not the only source —
  // but an OSC 133 mark is filed from inside this session and has nowhere else to get
  // one from.
  inbox.register(id, worktreeId);
  // Adopt anything that subscribed before this session existed.
  const waiting = pending.get(id);
  if (waiting) {
    for (const fn of waiting) s.listeners.add(fn);
    pending.delete(id);
  }

  /** Whether the terminal may send right now. */
  const canSend = () => s.ws?.readyState === WebSocket.OPEN && !s.replaying;

  // Keystrokes go out as binary frames so a multi-byte character split across
  // sends can't be mangled by UTF-8 validation; the daemon reserves text
  // frames for control messages.
  const encoder = new TextEncoder();
  const send = (data: string) => {
    if (canSend()) s.ws!.send(encoder.encode(data));
  };
  term.onData((data) => {
    // Read-on-type. Typing into a pane is the strongest "I have seen this" there is,
    // and it is the case a focus rule alone misses: answering a `sudo` prompt or a
    // permission dialog *in place* must make the badge go away without the user also
    // having to click it. Cheap — the store returns immediately when there is nothing
    // unseen, which is every keystroke but the first.
    inbox.read(s.id);
    send(data);
  });
  // Keys that must be answered before xterm sees them: the palette accelerator
  // and Shift+Enter. Sending goes through the same `canSend` gate as ordinary
  // typing, so a replay in progress cannot be interrupted by a keystroke.
  // Read at event time, not at construction: the preference can change while a
  // shell is open and re-attaching a handler per session would be pointless work.
  term.attachCustomKeyEventHandler((e) =>
    handleKeyEvent(e, send, prefs().shiftEnterNewline, isMac()),
  );
  attachFileInput(s, canSend);
  // `onBinary` carries already-8-bit payloads (mouse reports), one byte per
  // char code — encoding those as UTF-8 would corrupt them.
  term.onBinary((data) => {
    if (!canSend()) return;
    const bytes = new Uint8Array(data.length);
    for (let i = 0; i < data.length; i += 1) bytes[i] = data.charCodeAt(i) & 0xff;
    s.ws!.send(bytes);
  });
  term.onResize(({ cols, rows }) => {
    if (s.ws?.readyState === WebSocket.OPEN) {
      s.ws.send(JSON.stringify({ type: "resize", cols, rows }));
    }
  });
  // A wheel over a full-screen program (the alternate buffer: a pager like
  // `git log`'s, a TUI) scrolls it, the way a native terminal does — xterm.js
  // leaves the alternate buffer alone, so without this the wheel does nothing
  // over `git log`/`less` while it scrolls fine in a plain terminal. Guarded on
  // the two cases that must not also get cursor keys: the normal buffer (where
  // the wheel scrolls xterm's own scrollback) and a program that has taken the
  // mouse over itself (`term.modes.mouseTrackingMode` — vim/htop/`less --mouse`
  // report their own wheel events and must not receive a second copy).
  container.addEventListener("wheel", (e) => {
    if (term.buffer.active.type !== "alternate") return;
    if (term.modes.mouseTrackingMode !== "none") return;
    e.preventDefault();
    // One screen-scroll step per notch is the native feel; send the same
    // sequences the wheel in a real terminal sends, so any full-screen program
    // that scrolls with arrows behaves.
    send(e.deltaY > 0 ? "\x1b[B" : "\x1b[A");
  });

  // A process sets its own tab title with OSC 0/2 (`ESC ] 0;title BEL`). xterm
  // parses the sequence and reports the result here — the only parser worth
  // trusting, since BEL doubles as an OSC terminator and a naive byte scan
  // would misread it. Fired only when this pane is allowed to rename itself
  // (a plain terminal always is; a config pane only with its flag), so the
  // consumer never re-checks the decision.
  term.onTitleChange((title) => {
    if (!s.allowTerminalRenaming) return;
    for (const fn of titleListeners) fn({ sessionId: id, title });
  });
  // A BEL (U+0007) is the "something finished" baseline — a terminal rings it
  // and the user should hear it. xterm 5 exposed the bell only as `onBell` (its
  // `bellStyle`/`bellSound` options are gone), so the sound is played here.
  // Sound only: the richer notification half is OSC 9 below.
  //
  // Gated on the replay for the same reason OSC 9 is: a reattach writes the
  // scrollback back through xterm, and every BEL still in it would ring again —
  // a burst of beeps for output that was heard the first time.
  term.onBell(() => {
    if (!s.replaying) playBell();
  });
  // OSC 9 (`ESC ] 9;message BEL`) is the "show a notification" sequence — the
  // one macOS Terminal and iTerm2 turn into a system banner, and the thing
  // Claude Code's "task finished" notification rides on. xterm does not act on
  // it, so this is where it becomes one. Return true so xterm treats it as
  // consumed rather than passing it through.
  term.parser.registerOscHandler(9, (data) => {
    // During a reattach the scrollback is replayed through xterm; an OSC 9 in
    // it would otherwise re-notify for output that has already been seen.
    //
    // **`OSC 9;4` is not a notification.** It is the ConEmu/Windows-Terminal progress
    // sequence (`ESC ] 9 ; 4 ; <state> ; <percent> BEL`), and Claude Code emits it — so
    // before this check every progress tick rang the bell and raised a banner whose
    // body read `4;1;50`. Consumed either way (`return true`), because passing an
    // unhandled OSC 9 through to xterm prints nothing useful.
    if (!s.replaying && isOsc9Notification(data)) {
      // Sound as well as banner, at the same `terminal.bellVolume`: OSC 9 is
      // the *stronger* "notice me" of the two sequences, so it would be odd for
      // it to be the quieter one. It never doubles up with the handler above —
      // xterm consumes the terminating BEL as the OSC's string terminator and
      // never reports it as a bell.
      playBell();
      // Straight into the inbox and nowhere else. There used to be a listener seam here
      // as well, whose subscriber raised a toast and a system banner of its own — so an
      // OSC 9 was the one notification with no off switch, and away from the window it
      // fired twice. The inbox classifies it as `attention` from a `command` producer,
      // which puts it under `activity.notifyNoticed` with everything else.
      inbox.report(id, s.worktreeId, { type: "notify", message: data });
    }
    return true;
  });
  // OSC 133 semantic prompt marks, from veld's own shell integration
  // (`veld-daemon/src/pty/shims.rs`, `terminal.shellIntegration`). This is what tells
  // the worktree's inbox that a command ran and how it ended — an exit code, not a
  // guess. Nothing renders these, so the handler exists purely to file them.
  //
  // Gated on the replay like the two above: a reattach writes the scrollback back
  // through xterm, and every mark still in it would re-report commands that finished
  // before the page was reloaded.
  term.parser.registerOscHandler(133, (data) => {
    if (!s.replaying) {
      const signal = parseOsc133(data);
      if (signal) inbox.report(id, s.worktreeId, signal);
    }
    return true;
  });

  // `shell` and `reattach` both mean "no command to choose" — a login shell has
  // none, and a reattach runs whatever is already there. The connection itself
  // is established by the caller (`mountTerminal`) *after* the terminal has
  // been opened and fitted, so the pty is minted at the real size rather than
  // xterm's 80x24 default.
  return { session: s, created: true };
}

function attachUrl(ticket: string, cols: number, rows: number): string {
  const u = new URL("/api/pty/attach", window.location.href);
  u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
  u.searchParams.set("ticket", ticket);
  // The size we already know, so the shell's first prompt is the right width
  // instead of rendering at 80x24 and reflowing.
  u.searchParams.set("cols", String(cols));
  u.searchParams.set("rows", String(rows));
  return u.toString();
}

async function connect(s: Session, mode?: PaneLaunchMode): Promise<void> {
  const generation = s.generation;
  // Cleared before anything can observe the new attempt: a stale `0` from the
  // previous process would make a pane that closes on clean exit close itself
  // the instant it was restarted.
  s.exitCode = null;
  setState(s, "connecting");
  let ticket: string;
  try {
    // The tab id *is* the daemon session id, so this reattaches to a surviving
    // shell when there is one and starts a fresh one otherwise. `pane` names
    // which command a config-declared pane should run if one has to be spawned;
    // the daemon ignores it when the session is already live.
    const pane =
      s.spec !== undefined && mode !== undefined
        ? { spec: s.spec, mode }
        : undefined;
    const minted = await api.ptyTicket(s.worktreeId, s.id, pane);
    // **The only path to `idle`.** A config pane attaching with no mode is
    // asking "is my session still there?" — from `startPlanFor`'s `reattach`, or
    // from the Reconnect button. If it is not, spawning is the wrong answer
    // twice over: the daemon has no command without a `pane`, so it would run a
    // login shell under a tab still labelled and iconed "Claude", and no
    // `pane_sessions` row would be written. Answer honestly and let the user
    // choose between resuming and starting over.
    if (s.spec !== undefined && mode === undefined && !minted.resumed) {
      if (s.generation !== generation) return;
      setState(s, "idle");
      return;
    }
    ticket = minted.ticket;
    // A tab restored from sessionStorage expected its shell to still be there.
    // If it isn't (the daemon restarted, or the detach grace expired) say so —
    // otherwise a running build is silently replaced by an empty prompt, which
    // is exactly what the docs promise won't happen.
    if (!minted.resumed && EXPECTED_RESUMES.delete(s.id)) {
      writeNotice(s, "the previous shell is gone — this is a new one");
    }
  } catch (e) {
    if (s.generation !== generation) return;
    setState(s, "error", e instanceof Error ? e.message : String(e));
    return;
  }
  // A restart (or disposal) while the ticket was in flight: this ticket is
  // now for a terminal nobody is looking at. Dropping it un-redeemed is fine
  // — it expires on its own.
  if (s.generation !== generation) return;

  const ws = new WebSocket(attachUrl(ticket, s.term.cols, s.term.rows));
  ws.binaryType = "arraybuffer";
  s.ws = ws;
  // Per-connection, so a reconnect's close is judged on its own attempt.
  let everReady = false;

  ws.onmessage = (ev) => {
    if (s.generation !== generation) return;
    if (typeof ev.data === "string") {
      if (ev.data.includes('"ready"')) {
        everReady = true;
        // A live connection ends any auto-reconnect cycle: the next drop arms
        // fresh with the full budget rather than continuing a spent one.
        s.reconnectLeft = null;
        s.reconnectFiredFirst = false;
      }
      handleControl(s, ev.data);
      return;
    }
    const bytes = new Uint8Array(ev.data as ArrayBuffer);
    if (s.replaying) {
      // Track the write so the input gate lifts only once xterm has finished
      // parsing — it answers embedded queries during parsing, not on receipt.
      s.replayWrites += 1;
      s.term.write(bytes, () => {
        s.replayWrites -= 1;
        endReplayIfDone(s);
      });
      return;
    }
    s.term.write(bytes);
  };
  ws.onerror = () => {
    // `onclose` always follows, and carries the useful information; reporting
    // here too would overwrite a real exit message with a generic one.
  };
  ws.onclose = () => {
    if (s.generation !== generation) return;
    s.ws = null;
    if (s.state === "ended") return;
    // A close with no `exit` frame is an abnormal drop. Which kind depends on
    // whether the session ever came up: a browser cannot read the status or body
    // of a failed handshake, so a refused upgrade (no shell available, at
    // capacity, a stale ticket) is indistinguishable from a network drop *except*
    // by never having reached `ready`. Saying which one it was is the difference
    // between a user retrying and a user checking the daemon log.
    if (everReady) {
      setState(s, "error", "connection lost");
      // Narrate only the first drop of a retry cycle: an auto-reconnect attempt
      // failing again would otherwise write "connection lost" once per attempt
      // into the scrollback. The state is already `error` — the pane and the
      // overlay show it — so retry failures stay silent.
      if (s.reconnectLeft === null) writeNotice(s, "connection lost");
    } else {
      setState(s, "error", "could not open a terminal — see the daemon log");
      if (s.reconnectLeft === null) {
        writeNotice(s, "could not open a terminal — see the daemon log");
      }
    }
    // Whatever ended the socket, the shell is (or should be) still running —
    // that is what the holder process and the detach grace are for — so try to
    // get back to it on the user's behalf before offering the button.
    maybeAutoReconnect(s);
  };
}

/**
 * A click on a link in the terminal.
 *
 * The **daemon** decides where it opens (see `api.ptyOpenUrl`), which is what
 * makes a click and a `$BROWSER` invocation from a process in the same shell
 * behave identically. A modifier held down is the local override: it goes to the
 * real browser without asking, because "this one, in my logged-in browser" is a
 * per-click intention and not something to add to an exempt list.
 */
async function activateLink(sessionId: string, url: string, event: MouseEvent): Promise<void> {
  if (event.metaKey || event.ctrlKey || event.shiftKey) {
    openExternally(url);
    return;
  }
  try {
    const answer = await api.ptyOpenUrl(sessionId, url);
    // `pane` is already done: the daemon pushed an `open_url` frame down this
    // session's socket, and the app's frame handler owns the placement.
    if (answer.target === "system") {
      // The daemon knows *which* of several reasons applied — an exempt origin (which
      // may come from a project's veld.json the user has never read), the preference,
      // or no attached window — and this is the one path that can show it. Without
      // this the click just opens somewhere else and the answer lives in a Rust file.
      if (answer.reason) notifyRedirect(`Opened in your browser — ${answer.reason}`);
      openExternally(url);
    }
  } catch (e) {
    // The link still has to work. Report it once and open it the way the user's
    // machine would have anyway.
    notifyError("Could not ask Veld where to open that link", e);
    openExternally(url);
  }
}

/**
 * Open a URL outside Veld.
 *
 * `window.open` in both builds: a browser tab makes it a tab, and the desktop
 * shell's `setWindowOpenHandler` denies the native popup and hands the URL to
 * `shell.openExternal`. `noopener` because the opened page must not be able to
 * reach back into `/ide` through `window.opener`.
 */
export function openExternally(url: string): void {
  window.open(url, "_blank", "noopener,noreferrer");
}

/** Subscribers to `open_url` frames — see `onTerminalOpenUrl`. */
const openUrlListeners = new Set<(event: { sessionId: string; url: string }) => void>();

/** Subscribers to a shell's OSC 0/2 title — see `onTerminalTitleChange`. */
const titleListeners = new Set<(event: { sessionId: string; title: string }) => void>();

/**
 * A URL the daemon routed to this page, and which terminal it came from.
 *
 * The session id *is* the terminal tab's id, so the subscriber can find the dock
 * the terminal sits in and open the page beside it — the same shape the
 * `target=_blank`-in-a-browser-pane path uses (`onBrowserOpenRequest`). Handled
 * here rather than in the React component because a terminal's socket outlives
 * every mount of its pane.
 */
export function onTerminalOpenUrl(
  fn: (event: { sessionId: string; url: string }) => void,
): () => void {
  openUrlListeners.add(fn);
  return () => openUrlListeners.delete(fn);
}

/**
 * A shell set its own tab title with OSC 0/2.
 *
 * The session id *is* the terminal tab's id, so the consumer can find the tab
 * in the layout and adopt the title — but only when the pane is allowed to
 * rename itself (`allowTerminalRenaming`); the host already decided that, and
 * this event only fires for a plain terminal or an opted-in config pane.
 */
export function onTerminalTitleChange(
  fn: (event: { sessionId: string; title: string }) => void,
): () => void {
  titleListeners.add(fn);
  return () => titleListeners.delete(fn);
}

function handleControl(s: Session, raw: string): void {
  let msg: { type?: string; code?: number; resumed?: boolean; url?: string };
  try {
    msg = JSON.parse(raw);
  } catch {
    return;
  }
  switch (msg.type) {
    case "replay_begin":
      s.replaying = true;
      s.replayEnded = false;
      s.replayWrites = 0;
      break;
    case "replay_end":
      s.replayEnded = true;
      endReplayIfDone(s);
      break;
    case "ready":
      // The holder is up, so for a config pane the daemon has *attempted* to
      // record its token by now (`record_pane_launch` runs before the attach
      // completes) — whether it just spawned the command or reattached to a
      // session that had. Best-effort on the daemon side, so this can be true
      // with no row; the cost is bounded, because a resume without a row is
      // refused with "nothing to resume — start it fresh" rather than quietly
      // starting a new conversation.
      if (s.spec !== undefined) s.launched = true;
      setState(s, "live");
      // Deliberately silent on `msg.resumed`. A resumed shell is *live*, and
      // very likely a full-screen program (Claude Code, vim, top) mid-redraw —
      // injecting a line into its screen corrupts it. See writeNotice.
      // The pane may have been resized between minting the ticket and the
      // socket opening; re-assert the size now that the shell can hear it.
      requestFit(s);
      break;
    case "exit":
      // `ended`, not `error`: the shell finished, which is a normal way for a
      // terminal to stop and offers a Restart rather than reading as a fault.
      s.exitCode = msg.code ?? 0;
      // The inbox's fallback producer, for a pane with no shell integration at all — a
      // `oneshot` command pane, or a shell veld has no handoff for. The store discards it
      // where OSC 133 already spoke, so this cannot double-report.
      inbox.report(s.id, s.worktreeId, { type: "exit", code: s.exitCode });
      setState(s, "ended", `exit ${s.exitCode}`);
      if (
        shouldCloseOnExit({
          spec: s.spec,
          closeOnExit: s.closeOnExit,
          exitCode: s.exitCode,
        })
      ) {
        paneCloseHandler?.(s.id, s.worktreeId);
      }
      break;
    case "taken_over":
      // Another view of `/ide` (a second window, or a duplicated tab that
      // inherited this sessionStorage) attached to the same shell. Only one
      // socket owns a session, so this one is finished — say so instead of
      // reporting the close as a lost connection.
      setState(s, "ended", "opened in another window");
      writeNotice(s, "this terminal was opened in another window");
      break;
    case "open_url":
      // The one control frame that is not about this terminal's display. Ignored
      // rather than trusted blindly if it arrives without a URL — the daemon
      // validated it, and this is the cheap re-check at the edge that consumes it.
      if (typeof msg.url === "string" && msg.url !== "") {
        for (const fn of openUrlListeners) fn({ sessionId: s.id, url: msg.url });
      }
      break;
    case "lagged":
      // The daemon dropped output we were too slow to take, so the screen is
      // missing bytes. Reported on the status chip, not written into the
      // terminal: the shell is still live and a full-screen program would have
      // the notice land in the middle of its rendering.
      flash(s, "output dropped — terminal fell behind");
      break;
  }
}

/** Lift the input gate once the replay has been received *and* parsed. */
function endReplayIfDone(s: Session): void {
  if (s.replayEnded && s.replayWrites === 0) s.replaying = false;
}

/**
 * Print a dim, bracketed line into the terminal itself, so the reason a session
 * stopped sits in the scrollback next to what it was doing.
 *
 * Only for terminals whose shell is **gone** (exited, disconnected, taken
 * over). Never narrate at a live shell: it is very likely a full-screen program
 * mid-redraw (Claude Code, vim, top) and an injected line corrupts its display.
 * Live conditions belong on the pane's status chip — use [`flash`].
 *
 * That rule is enforced here rather than only documented, because the natural
 * way to report a new live-shell condition is to add a `writeNotice` call
 * beside the existing ones and the damage is invisible until someone happens to
 * have a TUI open.
 */
function writeNotice(s: Session, text: string): void {
  if (s.state === "live") {
    if (import.meta.env?.DEV) {
      console.warn(`[veld] refusing to write "${text}" into a live terminal; use flash()`);
    }
    return;
  }
  s.term.write(`\r\n\x1b[2m[veld] ${text}\x1b[0m\r\n`);
}

/** Clear a pending auto-reconnect and reset the retry cycle.
 *
 *  Called whenever the user (or a dispose) supersedes the connection: a manual
 *  reconnect, restart or start is a fresh attempt, so the next drop arms with
 *  the full budget rather than continuing a spent cycle — and a pending timer
 *  must not race the action it was superseded by. */
function cancelAutoReconnect(s: Session): void {
  if (s.reconnectTimer !== null) {
    window.clearTimeout(s.reconnectTimer);
    s.reconnectTimer = null;
  }
  s.reconnectLeft = null;
  s.reconnectFiredFirst = false;
}

/**
 * Arm the next auto-reconnect attempt after an abnormal socket close.
 *
 * A dropped socket with the shell still running (the daemon restarted, the
 * machine slept, a proxy timed out) is the common transient, and reattaching is
 * exactly what the manual Reconnect button does — so this does the same thing a
 * few times on the user's behalf. The first attempt is near-immediate (the
 * `reconnectFirstDelaySeconds` setting); the rest wait `reconnectBackoffSeconds`
 * so a still-failing session is not hammering a daemon that is itself coming
 * back. `reconnectTries` of `0` is the off switch and returns immediately.
 *
 * The budget counts per *cycle*: the first drop (from a healthy connection)
 * arms it from the setting, and each failed attempt decrements until it is
 * spent, where the terminal settles in `error` and the manual Reconnect button
 * stays. A cycle resets the moment a connection reaches `ready` (see
 * `ws.onmessage`), and a manual action resets it via [`cancelAutoReconnect`].
 */
function maybeAutoReconnect(s: Session): void {
  const tries = prefs().reconnectTries;
  if (tries <= 0) return;
  if (s.reconnectLeft === null) {
    // First drop in a cycle: arm with the full budget and a near-immediate
    // first attempt.
    s.reconnectLeft = tries;
    s.reconnectFiredFirst = false;
  }
  if (s.reconnectLeft <= 0) return;
  s.reconnectLeft -= 1;
  const first = !s.reconnectFiredFirst;
  s.reconnectFiredFirst = true;
  const delayMs =
    (first
      ? prefs().reconnectFirstDelaySeconds
      : prefs().reconnectBackoffSeconds) * 1000;
  const gen = s.generation;
  s.reconnectTimer = window.setTimeout(() => {
    s.reconnectTimer = null;
    // Superseded by a manual action (they bump the generation) or a dispose
    // since this was armed. If so, this attempt is for a terminal nobody is
    // looking at — leave it to whoever owns it now.
    if (s.generation !== gen) return;
    void connect(s);
  }, delayMs);
}

/**
 * Don't sweep more often than this. A sweep is one attempt per stalled
 * terminal, and the events that trigger it can arrive in bursts — tabbing
 * back and forth, or a daemon that comes up and goes down again.
 */
const RETRY_SWEEP_MS = 3000;
let lastSweep = 0;

/**
 * Try the stalled terminals again, because something changed that makes this
 * attempt worth more than the last one.
 *
 * [`maybeAutoReconnect`]'s budget is spent in about eleven seconds, and the two
 * events that most often break a terminal's socket outlast that by a long way:
 * a laptop that slept, and a daemon restarted by `veld update`. So the budget
 * ran out while the cause was still present, and the pane settled into `error`
 * with a **live shell behind it** — the daemon keeps it, which is the whole
 * point of the holder process — waiting for a click nobody knew to make. The
 * scrollback of a running build sat there looking dead.
 *
 * Called when the page becomes visible again (the laptop woke, the window came
 * forward) and when the control socket reconnects (the daemon is back). Both are
 * evidence, not a timer: a retry with the same conditions as the last failure is
 * how a page ends up hammering a daemon that is not there.
 *
 * Only `error`, and only with no cycle in flight: `ended` is a shell that exited
 * and has an exit code on screen, and reconnecting to one would replay its
 * ending as if it had just happened.
 */
export function retryStalledTerminals(): void {
  const now = Date.now();
  if (now - lastSweep < RETRY_SWEEP_MS) return;
  lastSweep = now;
  for (const s of [...sessions.values()]) {
    if (s.state !== "error" || s.reconnectTimer !== null) continue;
    reconnectTerminal(s.id);
  }
}

// Guarded because this module is imported by unit tests that have no DOM. The
// listener is the page's for its whole life — there is no unmount for a registry
// that outlives React by design.
if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") retryStalledTerminals();
  });
}

/**
 * Reattach to the *same* shell after the socket dropped.
 *
 * The non-destructive counterpart to [`restartTerminal`], and the one that
 * matches what the detach grace exists for: the shell is still running, we just
 * lost the pipe to it (the machine slept, the daemon was restarted mid-`veld
 * update`, a proxy timed out). Offering only Restart there would delete a live
 * session — including the build the grace was protecting.
 */
export function reconnectTerminal(id: string): void {
  const s = sessions.get(id);
  if (!s) return;
  // A manual reconnect is a fresh attempt: cancel any pending auto-reconnect so
  // it cannot race, and re-arm the budget for the drop that follows if it fails.
  cancelAutoReconnect(s);
  s.generation += 1;
  s.ws?.close();
  s.ws = null;
  void connect(s);
}

/**
 * Start a fresh shell in an existing terminal, keeping its scrollback on
 * screen as history.
 *
 * The old daemon session has to be **deleted first**, not just disconnected
 * from. Session ids are the reattach key, so reconnecting with the same id
 * would resume the very session being restarted — and since Restart is offered
 * precisely when the shell has exited, that would replay the exit notice and
 * report the same exit code again instead of giving the user a new shell.
 * Deleting it frees the id for a new one.
 *
 * Bumping the generation is what makes the overlap safe: the previous socket's
 * handlers all return early afterwards, so a late frame from the old session
 * can't be written into the new one.
 */
export function restartTerminal(id: string): void {
  const s = sessions.get(id);
  if (!s) return;
  // A restart reuses the pane id, so the inbox's per-session state is about the *previous*
  // process and none of it carries over — in particular the "this pane's exit is already
  // filed" marker, which otherwise silenced every later exit in the pane.
  inbox.restarted(id);
  // A config-declared pane has no login shell to fall back to, and "restart"
  // for one means the same thing "start fresh" does: run its launch command
  // again under a new identity.
  if (s.spec !== undefined) {
    startTerminal(id, "fresh", "fresh");
    return;
  }
  cancelAutoReconnect(s);
  s.generation += 1;
  s.ws?.close();
  s.ws = null;
  s.restarting = "shell";
  setState(s, "connecting");
  const generation = s.generation;
  void (async () => {
    // A failure here is not fatal: the session may already be gone (reaped, or
    // never created because the spawn failed), which is the state we want.
    await api.closePtySession(id).catch(() => {});
    if (s.generation !== generation) return;
    writeNotice(s, "starting a new shell");
    await connect(s);
  })();
}

/** Mount a session's element into `parent`, creating the session on first
 *  call. Safe to call repeatedly — remounting is the normal path. */
export function mountTerminal(
  id: string,
  worktreeId: number,
  parent: HTMLElement,
  pane?: PaneMount,
): void {
  const start = startPlanFor(id, pane);
  const { session: s, created } = ensure(id, worktreeId, pane);
  if (s.container.parentElement !== parent) parent.appendChild(s.container);

  if (!s.opened) {
    // Now that the container is in the document, xterm can measure the font.
    s.term.open(s.container);
    s.opened = true;
  }

  if (created) {
    // Fit **synchronously** before the first connection, so the pty is created
    // at the pane's real size rather than xterm's 80x24 default. A full-screen
    // program — a pager like `git log`, a TUI — that starts before the resize
    // enters its alternate screen at the wrong size and renders with a blank
    // offset (and a pager whose scrolling looks dead). `requestFit` below is
    // rAF-deferred, so it would run *after* the connection has already minted
    // the pty at 80x24. `fit()` forces layout, so the real size is measurable
    // right here for a container that is in the document; the same `< 2` guard
    // `requestFit` uses keeps a not-yet-laid-out container from collapsing the
    // grid (and the rAF fit and post-`ready` re-fit correct it once it is).
    const rect = s.container.getBoundingClientRect();
    if (rect.width >= 2 && rect.height >= 2) {
      try {
        s.fit.fit();
      } catch {
        // Container not laid out yet (a pane in a hidden dock); the rAF fit
        // below and the post-`ready` re-fit correct the size as soon as it is.
      }
    }
    connect(s, start === "shell" || start === "reattach" ? undefined : start);
  }
  requestFit(s);

  if (!s.observer) {
    s.observer = new ResizeObserver(() => requestFit(s));
    s.observer.observe(s.container);
  }
}

/**
 * Launch a config-declared pane's command because the user asked.
 *
 * `fresh` on a pane that has already launched replaces its identity: the daemon
 * mints a new token, so the tool starts a new conversation rather than
 * reopening the old one. That is what "start fresh" has to mean.
 *
 * `restarting` is a *presentation* flag and nothing else: it says the user is
 * replacing something they were already watching, so the pane can say
 * "Restarting…" instead of the "connecting…" chip a first launch gets. Left
 * `null` by the idle pane's own start buttons, which are not restarting
 * anything.
 */
export function startTerminal(
  id: string,
  mode: PaneLaunchMode,
  restarting: RestartKind | null = null,
): void {
  const s = sessions.get(id);
  if (!s) return;
  // Same reason as `restartTerminal`: the pane id is reused, so everything the inbox knows
  // about this session belongs to the process that just ended.
  inbox.restarted(id);
  cancelAutoReconnect(s);
  s.generation += 1;
  s.ws?.close();
  s.ws = null;
  const generation = s.generation;
  s.restarting = restarting;
  setState(s, "connecting");
  void (async () => {
    // The id is the reattach key, so a dead session under it has to go before a
    // new one can take the name — the same ordering `restartTerminal` needs and
    // for the same reason.
    await api.closePtySession(id).catch(() => {});
    if (s.generation !== generation) return;
    await connect(s, mode);
  })();
}

/** Detach the element without ending the session. */
export function unmountTerminal(id: string): void {
  sessions.get(id)?.container.remove();
}

export function focusTerminal(id: string): void {
  sessions.get(id)?.term.focus();
}

/**
 * Re-fit on the next frame.
 *
 * `fit()` reads the container's box, which is zero while the browser is still
 * laying it out after a mount or a tab switch; fitting then would compute a
 * 1x1 grid. Deferring a frame is also what coalesces a drag's worth of
 * ResizeObserver callbacks into one `TIOCSWINSZ`.
 */
function requestFit(s: Session): void {
  requestAnimationFrame(() => {
    if (!s.opened || !s.container.isConnected) return;
    const { width, height } = s.container.getBoundingClientRect();
    if (width < 2 || height < 2) return;
    try {
      s.fit.fit();
    } catch {
      // fit() throws if the renderer isn't ready yet; the next observer
      // callback will retry.
    }
  });
}

/**
 * End a session for good.
 *
 * Closing the socket is *not* enough: the daemon deliberately keeps a shell
 * running when its socket goes away, which is what makes a reload survivable.
 * Ending it takes an explicit `DELETE`, and skipping that leaks a shell (and
 * one of the daemon's session slots) until the detach grace expires.
 */
export function disposeTerminal(id: string): void {
  // Forget the pane's unseen events, and only here. A *released* terminal is being
  // handed to another window and its news is still the worktree's; a disposed one has
  // no pane left to focus, so an event pointing at it could never be read except by
  // mark-all-read — a badge the user cannot clear by looking is the poisoning the
  // design set out to avoid.
  inbox.forget(id);
  releaseTerminal(id);
  // Fire-and-forget: a failure here (daemon already gone) leaves the session
  // to the daemon's reaper, and there is no UI left to report it to.
  void api.closePtySession(id).catch(() => {});
}

/**
 * Let go of a terminal **without ending its shell.**
 *
 * The handover half of `disposeTerminal`: everything this page owns of a session
 * is torn down — the socket, the xterm, the element — and the shell keeps
 * running, because another window is about to attach to it by id.
 *
 * This exists as its own export because the alternative shape is a trap. Every
 * terminal is collected by `pruneTerminals`, which reads the layouts and ends
 * anything not named in them, so *removing a tab from the layout is what kills a
 * shell* — intent has nothing to do with it. A detach that simply moved the tab
 * record to another window would therefore hang up the very shell it was moving,
 * one commit later. Calling this first is what makes the tab's disappearance a
 * transfer instead: by the time the prune effect runs, the id is not a session
 * this page has, so there is nothing for it to collect.
 *
 * The shell then sits in the daemon's detach grace for the seconds it takes the
 * new window to boot and attach — the same path a reload already takes.
 */
export function releaseTerminal(id: string): void {
  const s = sessions.get(id);
  if (!s) return;
  // Bump first, so the close handler doesn't report "connection lost" for a
  // terminal the user deliberately closed.
  s.generation += 1;
  // A released session must not reconnect on its own — the timer would call
  // `connect` on a session that has been handed to another window.
  cancelAutoReconnect(s);
  s.observer?.disconnect();
  s.ws?.close();
  s.container.remove();
  s.term.dispose();
  s.listeners.clear();
  sessions.delete(id);
}

/**
 * Dispose every session not in `keep`.
 *
 * The layouts are the source of truth for which terminals should exist; this
 * is what collects the ones whose tab, or whose whole worktree, is gone.
 * Without it a shell survives every removal, invisible and holding one of the
 * daemon's session slots.
 */
export function pruneTerminals(keep: Iterable<string>): void {
  const live = new Set(keep);
  for (const id of [...sessions.keys()]) {
    if (!live.has(id)) disposeTerminal(id);
  }
}
