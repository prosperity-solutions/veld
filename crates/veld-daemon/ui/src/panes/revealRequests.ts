/**
 * "Take me to this session": somebody opened a session link (`/session/<id>`,
 * the line `veld agent link` hands an agent to end a pull request with), and the
 * daemon asks one main window to go there.
 *
 * It arrives on the IDE control socket (`ide/channel.ts`), like `restart_pane`.
 * `sessionId` is a tab to bring forward when the terminal is still open; `resume`
 * asks for a new tab of a project pane adopting an earlier conversation, which the
 * daemon sends only after a click on the link's page. Neither, and the worktree is
 * all there is to show.
 *
 * Every field is checked: this comes over a socket from a daemon that may be a
 * different version, and a frame that does not parse is dropped rather than
 * guessed at.
 */

export interface RevealPaneRequest {
  worktreeId: number;
  /** The terminal tab's id, which is also its PTY session id. */
  sessionId?: string;
  /** A project pane `spec` to open, adopting the earlier conversation `session`. */
  resume?: { spec: string; session: string };
}

const listeners = new Set<(r: RevealPaneRequest) => void>();

export function onRevealPaneRequest(fn: (r: RevealPaneRequest) => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Parse a `reveal_pane` frame and hand it to the subscribers. */
export function dispatchRevealPane(msg: Record<string, unknown>): boolean {
  const { worktree_id: worktreeId, session_id: sessionId, resume } = msg;
  if (typeof worktreeId !== "number" || !Number.isInteger(worktreeId)) return false;
  const req: RevealPaneRequest = { worktreeId };
  if (sessionId !== undefined) {
    if (typeof sessionId !== "string" || sessionId === "") return false;
    req.sessionId = sessionId;
  }
  if (resume !== undefined) {
    if (typeof resume !== "object" || resume === null) return false;
    const { spec, session } = resume as Record<string, unknown>;
    if (typeof spec !== "string" || spec === "") return false;
    if (typeof session !== "string" || session === "") return false;
    req.resume = { spec, session };
  }
  for (const fn of listeners) fn(req);
  return true;
}
