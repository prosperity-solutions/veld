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
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { api } from "../api";

export type TerminalState = "connecting" | "live" | "ended" | "error";

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

/** ANSI palettes. These are terminal colours, not design tokens — programs
 *  address them by index, so they can't come from the theme variables. The
 *  background/foreground do, so a terminal matches the surrounding panel. */
const ANSI_DARK = {
  black: "#22262c",
  red: "#e05a50",
  green: "#3fbf7f",
  yellow: "#e6b43c",
  blue: "#5aa2e0",
  magenta: "#b98ce0",
  cyan: "#4fbfc0",
  white: "#c8ced5",
  brightBlack: "#666d76",
  brightRed: "#ff7a70",
  brightGreen: "#5fdf9f",
  brightYellow: "#ffd45c",
  brightBlue: "#7ac2ff",
  brightMagenta: "#d9acff",
  brightCyan: "#6fdfe0",
  brightWhite: "#f2f4f6",
};

const ANSI_LIGHT = {
  black: "#2b3138",
  red: "#c33c32",
  green: "#28965f",
  yellow: "#a5761a",
  blue: "#2f6fb5",
  magenta: "#8a54b8",
  cyan: "#1f8a8c",
  white: "#5c636b",
  brightBlack: "#98a0a9",
  brightRed: "#e05a50",
  brightGreen: "#3fbf7f",
  brightYellow: "#c68f20",
  brightBlue: "#3f8ad0",
  brightMagenta: "#a06cd0",
  brightCyan: "#2aa5a7",
  brightWhite: "#171a1d",
};

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
  return {
    ...(dark ? ANSI_DARK : ANSI_LIGHT),
    background: token("--bg", dark ? "#0d0e10" : "#eceef0"),
    foreground: fg,
    cursor: token("--accent", dark ? "#3fbf7f" : "#28965f"),
    cursorAccent: token("--bg", dark ? "#0d0e10" : "#eceef0"),
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

/** Subscribe to a session's connection state. Returns an unsubscribe. */
export function subscribeTerminal(id: string, fn: () => void): () => void {
  const s = sessions.get(id);
  if (!s) return () => {};
  s.listeners.add(fn);
  return () => {
    s.listeners.delete(fn);
  };
}

export function terminalStatus(id: string): { state: TerminalState; detail: string } {
  const s = sessions.get(id);
  return s ? { state: s.state, detail: s.detail } : { state: "connecting", detail: "" };
}

/** Create the session (idempotent) without touching the DOM. */
function ensure(id: string, worktreeId: number): Session {
  const existing = sessions.get(id);
  if (existing) return existing;

  const term = new Terminal({
    allowProposedApi: true,
    cursorBlink: true,
    fontFamily: '"JetBrains Mono Variable", "JetBrains Mono", ui-monospace, monospace',
    fontSize: 12,
    // The shell's own scrollback plus room for a verbose build.
    scrollback: 5000,
    theme: xtermTheme(),
  });
  const fit = new FitAddon();
  term.loadAddon(fit);

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

  /** Whether the terminal may send right now. */
  const canSend = () => s.ws?.readyState === WebSocket.OPEN && !s.replaying;

  // Keystrokes go out as binary frames so a multi-byte character split across
  // sends can't be mangled by UTF-8 validation; the daemon reserves text
  // frames for control messages.
  const encoder = new TextEncoder();
  term.onData((data) => {
    if (canSend()) s.ws!.send(encoder.encode(data));
  });
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
    ticket = (await api.ptyTicket(s.worktreeId, s.id)).ticket;
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

  ws.onmessage = (ev) => {
    if (s.generation !== generation) return;
    if (typeof ev.data === "string") {
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
    // A close with no `exit` frame is an abnormal drop — the daemon stopped,
    // the upgrade was refused, or the network went away.
    if (s.state !== "ended") {
      setState(s, "error", "connection lost");
      writeNotice(s, "connection lost");
    }
  };
}

function handleControl(s: Session, raw: string): void {
  let msg: { type?: string; code?: number; resumed?: boolean };
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
 * Live conditions belong on the pane's status chip instead.
 */
function writeNotice(s: Session, text: string): void {
  s.term.write(`\r\n\x1b[2m[veld] ${text}\x1b[0m\r\n`);
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
  // Fire-and-forget: a failure here (daemon already gone) leaves the session
  // to the daemon's reaper, and there is no UI left to report it to.
  void api.closePtySession(id).catch(() => {});
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
