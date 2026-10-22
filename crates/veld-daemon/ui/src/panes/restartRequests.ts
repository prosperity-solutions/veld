/**
 * "Restart this agent pane": the daemon asks the one window showing a worktree to
 * relaunch a terminal tab, because the agent inside it ran `veld agent restart`,
 * `veld ide pane restart`, or `veld ide worktree outfit use X --restart-agent`.
 *
 * It arrives on the IDE control socket (`ide/channel.ts`) for the same reason
 * `open_file` does — only that channel can address "the window showing this
 * worktree". The tab id is the PTY session id.
 *
 * Every field is checked: this comes over a socket from a daemon that may be a
 * different version, and a frame that does not parse is dropped rather than
 * guessed at.
 */

export interface RestartPaneRequest {
  worktreeId: number;
  /** The terminal tab's id, which is also its PTY session id. */
  sessionId: string;
  /** `resume` picks the conversation back up; `fresh` starts a new one. */
  mode: "resume" | "fresh";
  /** The agent's first message once it is back. May be empty. */
  prompt: string;
}

const listeners = new Set<(r: RestartPaneRequest) => void>();

export function onRestartPaneRequest(fn: (r: RestartPaneRequest) => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Parse a `restart_pane` frame and hand it to the subscribers. */
export function dispatchRestartPane(msg: Record<string, unknown>): boolean {
  const { worktree_id: worktreeId, session_id: sessionId, mode, prompt } = msg;
  if (typeof worktreeId !== "number" || !Number.isInteger(worktreeId)) return false;
  if (typeof sessionId !== "string" || sessionId === "") return false;
  if (mode !== "resume" && mode !== "fresh") return false;
  const req: RestartPaneRequest = {
    worktreeId,
    sessionId,
    mode,
    prompt: typeof prompt === "string" ? prompt : "",
  };
  for (const fn of listeners) fn(req);
  return true;
}
