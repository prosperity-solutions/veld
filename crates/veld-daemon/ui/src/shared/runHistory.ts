/**
 * The run-history horizon: which ended runs the views show.
 *
 * `runs.historyDays` hides ended runs older than N days, with `0` meaning "show
 * everything". Applied here, once, to the payload both modes poll — rather than at
 * each of the four surfaces that render history (runs mode's History tab and its
 * card picker, the IDE nodes-view picker, the logs panel's run picker), which would
 * be four filters to keep in agreement.
 *
 * Three properties are deliberate:
 *
 * 1. **A live run is never hidden.** The horizon is about history, and a run that is
 *    still going is not history no matter when it started. This also keeps the
 *    Active tab identical under every setting.
 * 2. **A worktree's latest run survives the filter in IDE mode** — see
 *    `pruneRunHistory`, which prunes the `history` *list* but never the run the view
 *    is pointed at. Hiding it would take the logs and node states of a stopped run
 *    away from the pane whose whole job is showing them.
 * 3. **Hiding is counted, not silent.** `hiddenByHorizon` gives the caller the
 *    number it needs to say "3 older runs hidden", because a list that quietly
 *    omits things is indistinguishable from data loss — which is what the GC would
 *    look like.
 *
 * Pure and dependency-free so `vitest` (`environment: "node"`) can test it: the
 * clock is a parameter, never `Date.now()` read inside.
 */

import type { EnvironmentList, HistoryEntry, ProjectInfo, RunInfo } from "../api";

/**
 * The oldest timestamp still shown, as milliseconds since the epoch, or `null` when
 * the horizon is off.
 *
 * `days <= 0` is off rather than "hide everything" — that is the daemon's
 * off-switch value, and the two must agree.
 */
export function horizonCutoff(days: number, now: Date): number | null {
  if (!Number.isFinite(days) || days <= 0) return null;
  return now.getTime() - days * 24 * 60 * 60 * 1000;
}

/**
 * When a run ended, in epoch milliseconds, or `null` if that cannot be determined.
 *
 * `ended_at` is the honest field for an ended run; `created_at` is the fallback for a
 * history entry mid-teardown, whose `ended_at` is not written yet. An unparseable or
 * absent timestamp returns `null` and is **kept** by every filter here — a run with
 * no clock is not evidence that it is old.
 */
export function runEndedAt(run: {
  ended_at?: string | null;
  created_at?: string;
}): number | null {
  const raw = run.ended_at || run.created_at;
  if (!raw) return null;
  const t = Date.parse(raw);
  return Number.isNaN(t) ? null : t;
}

function withinCutoff(
  run: { ended_at?: string | null; created_at?: string },
  cutoff: number,
): boolean {
  const t = runEndedAt(run);
  return t === null || t >= cutoff;
}

/**
 * Drop history entries older than the horizon, leaving every run itself in place.
 *
 * The `history` array feeds the "which past run" pickers. Pruning it is always safe:
 * the run currently being viewed is `RunInfo` itself, never an entry in this list.
 */
export function pruneRunHistory(
  envs: EnvironmentList,
  days: number,
  now: Date,
): EnvironmentList {
  const cutoff = horizonCutoff(days, now);
  if (cutoff === null) return envs;
  const projects: ProjectInfo[] = envs.projects.map((p) => ({
    ...p,
    runs: p.runs.map((r) =>
      r.history === undefined
        ? r
        : { ...r, history: r.history.filter((h) => withinCutoff(h, cutoff)) },
    ),
  }));
  return { ...envs, projects };
}

/**
 * Whether an ended run is old enough for the History tab to hide it.
 *
 * Runs-mode-only, because it hides a whole environment row: IDE mode points at a
 * worktree's run and must keep showing it (see the module header).
 */
export function hiddenByHorizon(run: RunInfo, days: number, now: Date): boolean {
  const cutoff = horizonCutoff(days, now);
  if (cutoff === null || run.live) return false;
  return !withinCutoff(run, cutoff);
}

/** How many of `entries` the horizon hides — for the "N hidden" line. */
export function countHidden(
  entries: (RunInfo | HistoryEntry)[],
  days: number,
  now: Date,
): number {
  const cutoff = horizonCutoff(days, now);
  if (cutoff === null) return 0;
  return entries.filter((e) => {
    if ("live" in e && e.live) return false;
    return !withinCutoff(e, cutoff);
  }).length;
}
