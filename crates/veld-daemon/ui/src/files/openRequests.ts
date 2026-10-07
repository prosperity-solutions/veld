/**
 * "Show the human this": a file or a page the daemon asks this window to open —
 * `veld ide open`, or the `open` shim handed a text file.
 *
 * **It arrives on the IDE control socket** (`ide/channel.ts`, `ServerMsg::OpenFile`
 * and `OpenUrl` in `veld-daemon/src/ide.rs`), because that is the only place that
 * can address "the window showing this worktree" — which a request from a plain
 * cwd, or from a detached terminal, has no other way to reach. A PTY control frame
 * of the same shape is accepted too (`panes/terminalHost.ts`), so a daemon that
 * routes one through a terminal's own socket lands in the same place; both parse
 * into the one shape here and the app subscribes once.
 *
 * Every field is checked: this arrives over a socket from a daemon that may be a
 * different version, and both wire spellings (`worktree_id` and `worktreeId`) are
 * accepted so the two transports need not agree on case.
 */

export interface OpenFileRequest {
  worktreeId: number;
  /** The terminal the request came from, when there is one — the file opens beside it. */
  sessionId?: string;
  /** The daemon's display path: worktree-relative inside it, absolute outside. */
  path: string;
  line?: number;
  /** Tell the human (inbox event, toast/banner) rather than just open the tab. */
  notify: boolean;
}

export interface OpenUrlRequest {
  worktreeId: number;
  sessionId?: string;
  url: string;
  notify: boolean;
}

const fileListeners = new Set<(r: OpenFileRequest) => void>();
const urlListeners = new Set<(r: OpenUrlRequest) => void>();

export function onOpenFileRequest(fn: (r: OpenFileRequest) => void): () => void {
  fileListeners.add(fn);
  return () => fileListeners.delete(fn);
}

export function onOpenUrlRequest(fn: (r: OpenUrlRequest) => void): () => void {
  urlListeners.add(fn);
  return () => urlListeners.delete(fn);
}

function pick(msg: Record<string, unknown>, snake: string, camel: string): unknown {
  return msg[snake] !== undefined ? msg[snake] : msg[camel];
}

function common(
  msg: Record<string, unknown>,
  fallbackWorktree?: number,
  fallbackSession?: string,
): { worktreeId: number; sessionId?: string; notify: boolean } | null {
  const w = pick(msg, "worktree_id", "worktreeId") ?? fallbackWorktree;
  if (typeof w !== "number" || !Number.isInteger(w)) return null;
  const s = pick(msg, "session_id", "sessionId") ?? fallbackSession;
  return {
    worktreeId: w,
    ...(typeof s === "string" && s !== "" ? { sessionId: s } : {}),
    notify: msg.notify === true,
  };
}

/**
 * Parse an `open_file` frame and hand it to the subscribers. `fallback*` are what
 * the transport already knows — a PTY frame arrives on one terminal's socket, so it
 * need not repeat which.
 */
export function dispatchOpenFile(
  msg: Record<string, unknown>,
  fallbackWorktree?: number,
  fallbackSession?: string,
): boolean {
  const base = common(msg, fallbackWorktree, fallbackSession);
  if (!base || typeof msg.path !== "string" || msg.path === "") return false;
  const line = msg.line;
  const req: OpenFileRequest = {
    ...base,
    path: msg.path,
    ...(typeof line === "number" && Number.isInteger(line) && line > 0 ? { line } : {}),
  };
  for (const fn of fileListeners) fn(req);
  return true;
}

/** The same for an `open_url` the IDE channel carries (`veld ide open <url>`). */
export function dispatchOpenUrl(
  msg: Record<string, unknown>,
  fallbackWorktree?: number,
  fallbackSession?: string,
): boolean {
  const base = common(msg, fallbackWorktree, fallbackSession);
  if (!base || typeof msg.url !== "string" || msg.url === "") return false;
  const req: OpenUrlRequest = { ...base, url: msg.url };
  for (const fn of urlListeners) fn(req);
  return true;
}
