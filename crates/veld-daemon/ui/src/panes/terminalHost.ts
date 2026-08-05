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
import { api } from "../api";
import { ANSI_DARK, ANSI_LIGHT } from "../shared/ansi";
import { notifyError, notifyRedirect } from "../shared/notify";
import { terminalPrefs, type TerminalPrefs } from "../shared/settings";
import { chromeless, layoutSlot, windowSeed } from "../shell";
import { parseLayouts, storedTerminalIds, terminalIds } from "./model";
import { handleKeyEvent } from "./terminalKeys";

/**
 * Terminal ids this page expects to *resume*, captured once at module load.
 *
 * Read here rather than threaded down from the app so the knowledge lives beside
 * the code that needs it. It answers a question the daemon's `resumed: false`
 * cannot: was this a brand-new terminal, or one whose shell we expected to still
 * be there? Without it, a lost shell is silently replaced by an empty prompt.
 *
 * Read from the durable store this window restores from. Reading
 * `sessionStorage` directly meant the set was always empty in the case that
 * matters most: after Veld Desktop restarts there is no `sessionStorage`, so
 * every tab was treated as brand new and a reboot (or an expired grace, or a
 * refused protocol version) handed the user fresh prompts in "restored" tabs
 * without a word.
 */
const EXPECTED_RESUMES: Set<string> = (() => {
  try {
    // Read-only, and deliberately *not* through `loadLayouts`: "which shells
    // might legitimately still be running" is a different question from "which
    // panes does this window own", and a main window owns only what it displays
    // (see `readLayouts`). Answering the first with the second is what let a
    // window stamp its boot snapshot over another window's worktree.
    //
    // `windowSeed` on top, for the same reason it exists: a window opened by
    // detaching a terminal has no store yet, so without it every transferred
    // shell would look brand new — and a transfer that arrived to find its shell
    // gone would say nothing at all, which is the case this set exists to catch.
    return new Set([
      ...storedTerminalIds(layoutSlot, chromeless),
      ...Object.values(parseLayouts(windowSeed)).flatMap(terminalIds),
    ]);
  } catch {
    return new Set<string>();
  }
})();

export type TerminalState = "absent" | "connecting" | "live" | "ended" | "error";

interface Session {
  id: string;
  worktreeId: number;
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
  observer: ResizeObserver | null;
  /** Generation counter: a socket from a superseded connect attempt must not
   *  write into a terminal that has since been restarted. */
  generation: number;
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
export function terminalStatus(id: string): { state: TerminalState; detail: string } {
  const s = sessions.get(id);
  return s ? { state: s.state, detail: s.detail } : { state: "absent", detail: "" };
}

/** Create the session (idempotent) without touching the DOM. */
function ensure(id: string, worktreeId: number): Session {
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
    return existing;
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

  const s: Session = {
    id,
    worktreeId,
    term,
    fit,
    container,
    opened: false,
    ws: null,
    state: "connecting",
    detail: "",
    observer: null,
    generation: 0,
    replaying: false,
    replayWrites: 0,
    replayEnded: false,
    listeners: new Set(),
  };
  sessions.set(id, s);
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
  term.onData(send);
  // Keys that must be answered before xterm sees them: the palette accelerator
  // and Shift+Enter. Sending goes through the same `canSend` gate as ordinary
  // typing, so a replay in progress cannot be interrupted by a keystroke.
  // Read at event time, not at construction: the preference can change while a
  // shell is open and re-attaching a handler per session would be pointless work.
  term.attachCustomKeyEventHandler((e) =>
    handleKeyEvent(e, send, prefs().shiftEnterNewline),
  );
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

  connect(s);
  return s;
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

async function connect(s: Session): Promise<void> {
  const generation = s.generation;
  setState(s, "connecting");
  let ticket: string;
  try {
    // The tab id *is* the daemon session id, so this reattaches to a surviving
    // shell when there is one and starts a fresh one otherwise.
    const minted = await api.ptyTicket(s.worktreeId, s.id);
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
      if (ev.data.includes('"ready"')) everReady = true;
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
      writeNotice(s, "connection lost");
    } else {
      setState(s, "error", "could not open a terminal — see the daemon log");
      writeNotice(s, "could not open a terminal — see the daemon log");
    }
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
      setState(s, "ended", `exit ${msg.code ?? 0}`);
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
  s.generation += 1;
  s.ws?.close();
  s.ws = null;
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
export function mountTerminal(id: string, worktreeId: number, parent: HTMLElement): void {
  const s = ensure(id, worktreeId);
  if (s.container.parentElement !== parent) parent.appendChild(s.container);

  if (!s.opened) {
    // Now that the container is in the document, xterm can measure the font.
    s.term.open(s.container);
    s.opened = true;
  }
  requestFit(s);

  if (!s.observer) {
    s.observer = new ResizeObserver(() => requestFit(s));
    s.observer.observe(s.container);
  }
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
