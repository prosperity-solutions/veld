// Pure join/derivation helpers between the desktop registry (/api/repos) and
// veld run state (/api/environments). Every worktree with a veld.json is its
// own veld project root, so the join key is worktree.path === project_root.

import type { EnvironmentList, RunInfo, Worktree } from "./api";

export type WorktreeStatus = "running" | "partial" | "stopped";

/** The veld runs living in a worktree (its path is the project root). */
export function runsForWorktree(
  envs: EnvironmentList | null,
  worktree: Worktree,
): RunInfo[] {
  if (!envs) return [];
  const project = envs.projects.find(
    (p) => p.project_root === worktree.path,
  );
  return project?.runs ?? [];
}

/**
 * The run the UI binds its controls to: a running one first, then anything
 * in transition. Only live runs qualify — an environment's latest run keeps
 * its status (stopped, failed) as history, and binding to history would show
 * a crashed run as active forever. `null` = nothing to stop/restart (start
 * is always available when there's a veld config).
 */
export function activeRun(runs: RunInfo[]): RunInfo | null {
  const order: Record<string, number> = {
    running: 0,
    starting: 1,
    recovering: 2,
    stopping: 3,
    failed: 4,
    stopped: 5,
  };
  const live = runs
    .filter((r) => r.live)
    .sort((a, b) => (order[a.status] ?? 9) - (order[b.status] ?? 9));
  const best = live[0];
  if (!best) return null;
  return best.status === "stopped" ? null : best;
}

/** Rail status dot: running (green, pulsing), partial (amber), stopped. */
export function worktreeStatus(runs: RunInfo[]): WorktreeStatus {
  const run = activeRun(runs);
  if (!run) return "stopped";
  if (run.status === "running") return "running";
  return "partial";
}

/** URL list of a run, service-name-sorted, as [name, url] pairs. */
export function sortedUrls(run: RunInfo | null): Array<[string, string]> {
  if (!run) return [];
  return Object.entries(run.urls).sort(([a], [b]) => a.localeCompare(b));
}

/** Case-insensitive substring filter over worktree alias + branch. */
export function filterWorktrees(
  worktrees: Worktree[],
  query: string,
): Worktree[] {
  const q = query.trim().toLowerCase();
  if (!q) return worktrees;
  return worktrees.filter(
    (w) =>
      w.alias.toLowerCase().includes(q) || w.branch.toLowerCase().includes(q),
  );
}
