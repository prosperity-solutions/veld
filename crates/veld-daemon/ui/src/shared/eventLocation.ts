/**
 * Where an unseen event is, in words: the project, the worktree, and the pane.
 *
 * **One owner, because two surfaces name the same place** — the notification (toast
 * or OS banner) and the Next unread button's tooltip — and they have to agree. They
 * are read in sequence by the same person: a banner says a worktree needs you, you
 * come back, and the button beside the ⋯ menu is what you press to get there. If one
 * called it `feature/x` and the other `veld · main`, the only way to know they meant
 * the same pane would be to click and find out.
 *
 * **Extracted for the reason `projects.ts` and `ide/ownership.ts` were**, and it is
 * that file's own words: a decision over plain values, inline in `App.tsx` — "a
 * component with a socket, a dozen refs and no test" — is a decision nothing can
 * check. This one carries an invariant across two surfaces and had no test at all
 * while every other unit added beside it got one.
 *
 * Nothing here talks to the daemon, the inbox or React. The caller passes the rows
 * in and gets back a string.
 */

import { findTab, paneTabBaseLabel, type PaneLayout } from "../panes/model";
import { worktreeLabel } from "./worktreeName";

/** The least a worktree has to be to be named. */
export interface LocatableWorktree {
  id: number;
  repo_root: string;
  alias: string;
  /** Optional: a daemon older than v13 sends no such key. See [`worktreeLabel`]. */
  display_name?: string;
}

/** The least a project has to be. */
export interface LocatableRepo {
  root: string;
  name: string;
}

/**
 * Name the place an event happened.
 *
 * **The project's name only when it is not the one on screen.** Worktree markers and
 * branch names repeat across repos by design — the assigner probes per repo
 * (`markers_may_repeat_across_repos`) and two projects both checked out on `main` is
 * the default case — so "main" alone names nothing actionable once more than one
 * project is in play. Omitted for the selected project, where it would say nothing
 * and be on every line.
 *
 * **The pane's name only when this window has that worktree's layout.** It may not:
 * an agent hook is relayed to every client, including ones showing something else, so
 * the worktree alone has to be enough on its own.
 *
 * `paneTabBaseLabel`, never `paneTabLabel` — #272's fix, and shell integration makes
 * it matter more rather than less. A pane's *displayed* label can be the title the
 * process set for itself via OSC 0, and a shell's preexec hook writes the running
 * command there: a line reading "· sleep 5 && printf '\033]9;…'" names the noise
 * instead of the pane.
 *
 * Falls back to `"Veld"` for a worktree this client has never heard of, so a caller
 * always has something to title a banner with. The Next unread button cannot reach
 * that arm — it picks its target from the same list this resolves against — but the
 * notification path can, since the daemon relays an event to every client.
 */
export function eventLocation(args: {
  worktreeId: number;
  sessionId: string;
  /** Every project's, never the selected one's — the event is routinely elsewhere. */
  worktrees: readonly LocatableWorktree[];
  repos: readonly LocatableRepo[];
  activeRepoRoot: string | null;
  layouts: Readonly<Record<number, PaneLayout>>;
}): string {
  const wt = args.worktrees.find((w) => w.id === args.worktreeId);
  const project =
    wt && wt.repo_root !== args.activeRepoRoot
      ? (args.repos.find((r) => r.root === wt.repo_root)?.name ?? "")
      : "";
  const label = wt
    ? project
      ? `${project} · ${worktreeLabel(wt)}`
      : worktreeLabel(wt)
    : "Veld";
  const layout = args.layouts[args.worktreeId];
  const tab = layout ? findTab(layout, args.sessionId) : null;
  return tab && layout ? `${label} · ${paneTabBaseLabel(layout, tab)}` : label;
}
