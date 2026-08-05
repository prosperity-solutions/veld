// Pure join/derivation helpers between the desktop registry (/api/repos) and
// veld run state (/api/environments). Every worktree with a veld.json is its
// own veld project root, so the join key is worktree.path === project_root.

import type { EnvironmentList, Lane, RunInfo, Worktree } from "./api";

export type WorktreeStatus =
  | "running"
  | "partial"
  | "recovering"
  | "failed"
  | "stopped";

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

/**
 * The run the *diagnostics* surfaces read — a superset of [`activeRun`].
 *
 * Deliberately not the same predicate: run controls bind to a live run because
 * there is nothing to stop or restart otherwise, but logs and the last node
 * states are exactly what you want **after** a crash, and `/api/logs/{run}`
 * serves an ended run's output. So this falls back to the environment holding the
 * live slot even when its latest run is stopped, and then to the first one
 * listed.
 *
 * **Now the default only, not the answer.** When a worktree holds several
 * environments the user picks one ([`pickRun`]), and this is what a window with no
 * stored choice starts from. It is deliberately still ordering-based: the first
 * frame after opening a directory has nothing else to go on, and the selector
 * shows which run it landed on.
 */
export function diagnosticsRun(runs: RunInfo[]): RunInfo | null {
  return activeRun(runs) ?? runs.find((r) => r.live) ?? runs[0] ?? null;
}

/**
 * Which run the worktree's surfaces are bound to, and whether the user's stored
 * choice still resolves.
 *
 * A worktree can hold several environments at once — routinely, now that a
 * coding agent may start one while a human starts another — and every surface
 * before this bound to whichever one [`activeRun`] sorted first. That rule was
 * invisible: two `running` runs meant the alphabetically-first name won, silently,
 * with the other having no row, no logs and no stop button. So the choice becomes
 * a value the user owns, and this is the one place that resolves it.
 *
 * **`missing` is data, not an error to fix by writing back.** When the stored
 * name no longer resolves, the pick falls back but the caller still renders
 * "`foo` ended" — it must not silently jump to a sibling and present it as the
 * thing you were looking at. Clearing the stored name here (or in an effect) is
 * what makes a run vanish from under a reader mid-glance; let them choose.
 */
export interface RunPick {
  /** The bound run, or `null` when the worktree has no runs at all. */
  run: RunInfo | null;
  /**
   * The stored name that no longer resolves, or `null`. Non-null means `run` is
   * a fallback, so a surface naming the run has to say so.
   */
  missing: string | null;
}

export function pickRun(runs: RunInfo[], stored: string | null): RunPick {
  if (stored) {
    // Matches an ENDED run too, on purpose: `/api/logs` serves an ended run's
    // output, and "what happened to the run I was watching" is the question
    // right after it dies. Only a name with no environment at all is missing.
    const chosen = runs.find((r) => r.name === stored);
    if (chosen) return { run: chosen, missing: null };
    return { run: diagnosticsRun(runs), missing: stored };
  }
  return { run: diagnosticsRun(runs), missing: null };
}

/** Every run of a worktree that occupies its environment's live slot. */
export function liveRuns(runs: RunInfo[]): RunInfo[] {
  return runs.filter((r) => r.live);
}

/**
 * The runs a selector must show as alternatives to `name` — everything else in
 * the worktree, name-ordered as the daemon sent them.
 */
export function siblingRuns(runs: RunInfo[], name: string | undefined): RunInfo[] {
  return runs.filter((r) => r.name !== name);
}

/**
 * What the run selector lists, and how much it is hiding.
 *
 * An entry is an *environment*, not a run — one per name, carrying its latest
 * run's state. So an "ended" entry is an environment whose last run finished, and
 * a directory accumulates those: four names started last week are four rows of
 * noise in a control whose job is "which of the things happening here am I looking
 * at". Run *history* belongs to Runs mode and to the logs/nodes views' own run
 * picker; this is the live picture.
 *
 * Two exceptions keep it from becoming a dead end, and both earn themselves:
 *
 * - **Nothing live → show everything.** The run crashed overnight and its logs
 *   and last node states are exactly why the app is open. An empty selector, or
 *   one that hides the only thing there is, would strand that.
 * - **The bound entry is always listed.** Hiding the environment the control is
 *   *naming* is the vanishing behaviour this whole change exists to remove.
 *
 * `hidden` is the count the caller offers to reveal — a disclosure rather than a
 * setting, so nothing is configured and nothing persists.
 */
export interface SelectorRuns {
  runs: RunInfo[];
  hidden: number;
}

export function selectorRuns(
  runs: RunInfo[],
  boundName: string | undefined,
  showEnded = false,
): SelectorRuns {
  const live = liveRuns(runs);
  if (showEnded || live.length === 0) return { runs, hidden: 0 };
  const shown = runs.filter((r) => r.live || r.name === boundName);
  return { runs: shown, hidden: runs.length - shown.length };
}

/**
 * The environment name a fresh start should use, given what is already live.
 *
 * `▶` used to send no name at all, leaving the daemon to default to the
 * worktree's alias — so pressing it while an agent's `foo` was live minted a
 * *third* environment named after the alias, and pressing it while the alias
 * itself was live replaced that run (a start takes over its own name). Neither
 * was asked for. This suffixes instead, and the caller shows the name **before**
 * the start, because a run appearing under a name nobody typed is the confusion
 * this whole change exists to remove.
 *
 * Only *live* names are avoided. A stopped environment is the same environment
 * started again, which is what its history is for.
 */
export function proposeRunName(alias: string, runs: RunInfo[]): string {
  const taken = new Set(liveRuns(runs).map((r) => r.name));
  if (!taken.has(alias)) return alias;
  for (let n = 2; n < 100; n += 1) {
    const candidate = `${alias}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
  // 98 live environments in one directory is not a state worth a branch of its
  // own; the daemon rejects the duplicate and the toast says so.
  return `${alias}-${runs.length + 1}`;
}

/**
 * A name **no environment in this worktree has ever used** — what an explicit
 * "start another run" must create.
 *
 * Distinct from [`proposeRunName`], which avoids only *live* names because ▶ means
 * "run this environment again" and reusing a stopped environment's name is the
 * normal way to do that. An action labelled *another* cannot do the same: offering
 * "Start another run named `dev`" while a stopped `dev` sits in the list above it
 * is naming an environment that already exists, so the label is simply false.
 *
 * History counts as used. Two environments cannot share a name — `veld start`
 * would take over the existing one — so a name in the list, live or ended, is not
 * available for a new one.
 */
export function freshRunName(alias: string, runs: RunInfo[]): string {
  const taken = new Set(runs.map((r) => r.name));
  if (!taken.has(alias)) return alias;
  for (let n = 2; n < 100; n += 1) {
    const candidate = `${alias}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
  return `${alias}-${runs.length + 1}`;
}

/**
 * The environment name ▶ starts, given what the user is currently looking at.
 *
 * Two different intents share one button. With an **ended** run selected, ▶ means
 * "run that environment again" — its name, so the run lands in the history the
 * user is reading and its logs continue the same environment. Otherwise ▶ means
 * "another environment alongside the live ones", which is [`proposeRunName`].
 *
 * Without this the first case took the suffixed name too, and selecting last
 * night's crashed `dev` and pressing ▶ produced `dev-2` — a new environment with
 * none of the history the user was looking at.
 */
export function startRunName(
  alias: string,
  runs: RunInfo[],
  selected: RunInfo | null,
): string {
  if (selected && !selected.live) return selected.name;
  return proposeRunName(alias, runs);
}

/**
 * A worktree's run state, reduced to what a surface has to render.
 *
 * `partial` means **exactly** `starting` or `stopping` — a transition that is
 * expected to end on its own, which is what a run control draws a spinner for.
 * `recovering` is deliberately *not* folded into it: that is the health monitor
 * restarting a node which keeps failing its liveness probe, so it has no
 * expected end and a spinner would read as "perpetually starting". It routes to
 * the same attention affordance `failed` does — "something is wrong, look at the
 * nodes".
 *
 * What it got before was **not** nothing, and issue #214's own text is wrong
 * about this: folded into `partial` it rendered `.dot.partial`, a static amber
 * dot identical to an ordinary `starting`/`stopping` row. So the defect was that
 * an unbounded restart loop was *indistinguishable from progress*, which is worse
 * than an absent signal — a wrong signal is acted on.
 *
 * The pairing is load-bearing: [`transitionAction`] answers *which way* a
 * `partial` run is moving, and the two must stay in agreement, so
 * `partial` ⇔ `transitionAction() !== null` is asserted in the tests.
 */
export function worktreeStatus(runs: RunInfo[]): WorktreeStatus {
  return runStatus(activeRun(runs));
}

/**
 * One run's status in the render vocabulary — the reduction
 * [`worktreeStatus`] applies, extracted so a surface bound to a *chosen* run
 * (the run selector) reduces it the same way rather than growing a second
 * mapping that can disagree about `recovering`.
 *
 * `null` reduces to `stopped`: no run and a stopped run render identically, and
 * every caller here has "nothing to stop" as the same state.
 */
export function runStatus(run: RunInfo | null): WorktreeStatus {
  if (!run) return "stopped";
  if (run.status === "running") return "running";
  if (run.status === "failed") return "failed";
  if (run.status === "recovering") return "recovering";
  return "partial";
}

/**
 * Attention-first severity: which of several runs a single glyph must report.
 *
 * Deliberately not the same order as [`activeRun`]'s. That one answers "which
 * run do the controls act on", where a `running` run outranks a `failed` one;
 * this one answers "does anything here need looking at", where the opposite is
 * true. A sibling badge that reported the healthiest run would hide exactly the
 * case it exists for — an agent's run that died while the selected one is fine.
 */
const STATUS_SEVERITY: Record<WorktreeStatus, number> = {
  failed: 0,
  recovering: 1,
  partial: 2,
  running: 3,
  stopped: 4,
};

/** The most attention-worthy status among `runs` (`stopped` when empty). */
export function worstStatus(runs: RunInfo[]): WorktreeStatus {
  return runs
    .map((r) => runStatus(r))
    .reduce(
      (worst, s) => (STATUS_SEVERITY[s] < STATUS_SEVERITY[worst] ? s : worst),
      "stopped" as WorktreeStatus,
    );
}

/**
 * Which direction an observed transition is moving in, or `null` if there is no
 * transition to report.
 *
 * A run control's spinner used to appear only for an action *this window* fired
 * (`PendingMarker`), which meant a run started from the CLI, from another
 * window, or one already coming up when the window opened showed a plain ▶ or ■
 * while it was mid-transition. This is what lets the spinner be driven by
 * observed state, demoting the optimistic marker to a latency optimisation
 * rather than the only source of truth.
 *
 * A [`PendingAction`] rather than a direction of its own so the caller can hand
 * it straight to `actionColor`: a row that is stopping has to read as stopping
 * and not as starting, and that property was previously only available for a
 * locally-fired action. `restart` is never returned — a restart is observed as
 * `stopping` then `starting`, and only the local marker knows the two were one
 * action.
 */
export function transitionAction(run: RunInfo | null): PendingAction | null {
  if (run?.status === "starting") return "start";
  if (run?.status === "stopping") return "stop";
  return null;
}

/**
 * Which spinner a run control shows, and in which action's colour. `null` means
 * a plain ▶/■.
 *
 * **One function for every run control**, which is the point: while only the rail
 * row combined these two sources, the same worktree could spin in the rail and
 * show a static glyph in the top bar for the whole of an externally-started
 * transition. Two surfaces deriving "is this moving?" from different inputs is
 * the defect, not the styling.
 *
 * The local marker wins over the observed status, and that ordering is
 * load-bearing: it is the only thing that knows a **restart** was one action
 * rather than a stop followed by a start, and the top bar puts the spinner on
 * the button that was pressed. An *externally* fired restart is indistinguishable
 * from a stop-then-start over the wire, so it legitimately reads as one.
 */
export function spinnerAction(
  pending: PendingAction | null,
  run: RunInfo | null,
): PendingAction | null {
  return pending ?? transitionAction(run);
}

/**
 * Whether a worktree's run state is asking to be looked at rather than waited on.
 *
 * `failed` has given up; `recovering` is the health monitor restarting a node
 * that keeps failing its liveness probe, which has no expected end. Both mean
 * "open the nodes view", which is why one predicate serves both — and why
 * `recovering` must not reach the spinner, where an unbounded loop reads as
 * progress.
 *
 * Derived from the observed status only, never from a pending marker. No
 * *observed* state both spins and alerts, but the two **do** coexist once a local
 * action is in flight — stopping a failed run has always been offered — and that
 * is intended: the alert reports the run's state and the spinner reports your
 * action on it. Both halves are pinned in `model.test.ts`, the second one
 * precisely because the first reads as forbidding it.
 */
export function needsAttention(status: WorktreeStatus): boolean {
  return status === "failed" || status === "recovering";
}

/**
 * One rendered section of the rail.
 *
 * `lane` is `""` for the ungrouped section and the sentinel [`TRASH_LANE`] for
 * pending removals — neither is a real lane, and both are deliberately not
 * absences: the rail has to render a header for trash and none for ungrouped, so
 * the distinction lives in the group rather than in a caller's conditional.
 */
export interface RailGroup {
  /**
   * Unique identity of this section, and what a drop target is keyed on.
   *
   * Distinct from [`lane`] because two sections can write the same lane: the main
   * checkout gets a section of its own while still being ungrouped. Keying drops
   * on `lane` made both of them the same target.
   */
  key: string;
  /** The value to write to `worktrees.lane` for a worktree dropped here. */
  lane: string;
  /** Header text, or `null` for a section that has none (main, ungrouped). */
  label: string | null;
  /**
   * Fixed position — not draggable, not a drop target.
   *
   * True for the main checkout, which always leads the rail, and for pending
   * removals, which are leaving. A pinned section still renders and is still
   * separated by a divider; it just takes no part in ordering.
   */
  pinned: boolean;
  worktrees: Worktree[];
}

/**
 * Group key for the trash.
 *
 * Not a lane name: a lane is user-defined and this is a state, so a repo with a lane
 * literally called "Trash" must not merge with it. A leading NUL cannot occur in a
 * lane name — `valid_lane_name` rejects control characters — so the two key spaces
 * cannot collide.
 */
export const TRASH_LANE = "\u0000trash";

/**
 * Split a repo's worktrees into rail sections: ungrouped first, then each lane in
 * its own order, then the trash.
 *
 * The daemon already sorts the worktrees into this order (`WT_ORDER`), so this
 * only *segments* the list — it must not re-sort, or the manual order the user
 * dragged would be silently re-derived here from a different rule.
 *
 * Empty lanes are kept, because a lane you just created and have not filled yet
 * still needs somewhere to drop a worktree.
 */
export function railGroups(worktrees: Worktree[], lanes: Lane[]): RailGroup[] {
  const live = worktrees.filter((w) => !w.trashed_at);
  const trashed = worktrees.filter((w) => w.trashed_at);
  const known = new Set(lanes.map((l) => l.name));
  // A worktree whose lane no longer exists counts as ungrouped rather than
  // vanishing. `delete_lane` clears assignments in the same transaction, so this
  // should not arise — but a row the client cannot place is a row the user cannot
  // reach, and that is the worse failure.
  const ungrouped = live.filter((w) => !w.lane || !known.has(w.lane));
  // The main checkout gets a section of its own — it is the repository, not one of
  // the branches you are juggling, and a divider under it says so. Only while it is
  // ungrouped: assigned to a lane it belongs in that lane, because the user put it
  // there on purpose.
  const main = ungrouped.filter((w) => w.is_main);
  const groups: RailGroup[] = [];
  if (main.length > 0) {
    groups.push({ key: "main", lane: "", label: null, pinned: true, worktrees: main });
  }
  groups.push({
    key: "",
    lane: "",
    label: null,
    pinned: false,
    worktrees: ungrouped.filter((w) => !w.is_main),
  });
  for (const l of lanes) {
    groups.push({
      key: l.name,
      lane: l.name,
      label: l.name,
      pinned: false,
      worktrees: live.filter((w) => w.lane === l.name),
    });
  }
  if (trashed.length > 0) {
    groups.push({
      key: TRASH_LANE,
      lane: TRASH_LANE,
      label: "Trash",
      pinned: true,
      worktrees: trashed,
    });
  }
  return groups;
}

/**
 * The rail order after dragging `path` to index `toIndex` of lane `toLane`.
 *
 * Returns the full path order for the repo plus the lane the worktree lands in,
 * because the write is two halves: the lane goes on the worktree row and the
 * order is a full-list rewrite. Full list, not a delta, so the write is
 * idempotent — and **paths, not ids**, because `worktrees.id` is a rowid SQLite
 * reuses.
 *
 * Trashed worktrees are excluded from the returned order and cannot be a drop
 * target: they are on their way out, so placing one is meaningless.
 */
export function moveWorktree(
  groups: RailGroup[],
  path: string,
  toKey: string,
  toIndex: number,
): { order: string[]; lane: string } | null {
  const target = groups.find((g) => g.key === toKey);
  if (!target || target.pinned) return null;
  // Pinned sections take no part in ordering: the main checkout always leads
  // (`is_main DESC` in the daemon's sort, so it needs no position) and pending
  // removals would be given a position and then deleted.
  const placed = groups.filter((g) => !g.pinned);
  const moved = placed.flatMap((g) => g.worktrees).find((w) => w.path === path);
  if (!moved) return null;
  const order: string[] = [];
  for (const g of placed) {
    // The main checkout never gets a position, in any group. `WT_ORDER` sorts
    // `is_main DESC` before `sort_position`, so one would be silently ignored — and
    // writing a value the daemon overrules is how an order starts disagreeing with
    // what the rail shows.
    const rest = g.worktrees.filter((w) => w.path !== path && !w.is_main);
    if (g.key === toKey) {
      // Clamped rather than trusted: the index comes from a drop position, and a
      // stale render can hand over one past the end of a list that just shrank.
      const at = Math.max(0, Math.min(toIndex, rest.length));
      rest.splice(at, 0, moved);
    }
    order.push(...rest.map((w) => w.path));
  }
  return { order, lane: target.lane };
}

/**
 * A value that changes whenever a fired action has visibly landed — what the
 * optimistic pending markers watch.
 *
 * Status alone is not enough: `veld restart` tears the run down and starts a
 * fresh one, so a fast restart goes `running` → `running` and a status-only
 * check would never observe it (the spinner then runs until its timeout).
 * The run id covers that case, because a restart mints a new one
 * (`RunState::new`); the status covers start/stop, where the id is absent on
 * one side.
 */
export function runSignature(runs: RunInfo[]): string {
  const run = activeRun(runs);
  return run ? `${run.status}:${run.run_id}` : "none";
}

/**
 * [`runSignature`] for **one named environment** of a worktree.
 *
 * The per-worktree signature cannot serve a per-run marker: it reports whichever
 * run `activeRun` picks, so stopping `foo` while `bar` was still starting had the
 * marker cleared by `bar`'s transition — the spinner vanished with the stop still
 * in flight — and an action on a run `activeRun` doesn't pick could never observe
 * its own landing at all.
 *
 * `"none"` for a name with no environment: a start fires before the run exists,
 * so this is the legitimate starting value, and the marker clears when the name
 * appears.
 */
export function runSignatureFor(runs: RunInfo[], name: string): string {
  const run = runs.find((r) => r.name === name);
  return run ? `${run.status}:${run.run_id}` : "none";
}

/** URL list of a run, service-name-sorted, as [name, url] pairs. */
export function sortedUrls(run: RunInfo | null): Array<[string, string]> {
  if (!run) return [];
  return Object.entries(run.urls).sort(([a], [b]) => a.localeCompare(b));
}

// ---------------------------------------------------------------------------
// Optimistic pending action markers
// ---------------------------------------------------------------------------

/**
 * Actions that move [`runSignature`], and so can be tracked as pending.
 *
 * Only add a member if the action actually changes the run's status or id —
 * a marker for anything else (a share, say) never clears and leaves its
 * control disabled until the TTL.
 *
 * Widening this breaks the build in `actionColor` (App.tsx), which is
 * deliberately exhaustive. Two other sites enumerate members by literal and
 * will NOT fail to compile — fix them in the same change:
 * `TopBar`'s play/stop `loading` and its restart `loading`.
 */
export type PendingAction = "start" | "stop" | "restart";

export interface PendingMarker {
  label: PendingAction;
  /** `runSignature` at the moment the action was fired. */
  sigAtSet: string;
  /** Epoch ms after which the marker is abandoned. */
  expiresAt: number;
}

/**
 * Markers keyed by [`pendingKey`] — worktree **and** environment name.
 *
 * One slot per worktree was wrong once a directory could hold two live runs:
 * stopping one and starting the other overwrote each other's marker, so one of
 * the two controls lost its spinner immediately.
 */
export type PendingMap = Record<string, PendingMarker>;

/**
 * Identity of a pending action: the worktree it was fired from and the
 * environment it targets.
 *
 * A space separator, split at the FIRST one, which is what makes the key
 * unambiguous for any environment name at all: the left side is a decimal id,
 * so it can never contain the separator, and the name keeps whatever it holds.
 *
 * Deliberately NOT a NUL byte, which is the obvious "can't collide" choice: a
 * source file containing one is classified as binary and SILENTLY skipped by
 * ripgrep and grep, which is why `App.tsx` needs `rg -a` (AGENTS.md). A key
 * separator is not worth making this file unsearchable.
 */
export function pendingKey(worktreeId: number, runName: string): string {
  return `${worktreeId} ${runName}`;
}

/** Split a [`pendingKey`] back into its parts. */
export function parsePendingKey(key: string): { worktreeId: number; runName: string } | null {
  const at = key.indexOf(" ");
  if (at < 0) return null;
  const worktreeId = Number(key.slice(0, at));
  if (!Number.isSafeInteger(worktreeId)) return null;
  return { worktreeId, runName: key.slice(at + 1) };
}

/**
 * Drop markers whose signature has moved, or whose deadline has passed.
 *
 * A moved signature means *something* happened, not necessarily the thing
 * that was fired — this is a change detector, not an acknowledgement. Firing
 * Stop against a run that is still `starting` clears the marker as soon as it
 * reaches `running` on its own, while the stop is still in flight. Correlating
 * properly would need the daemon to hand back an action token.
 *
 * `sigFor` returns the marked environment's current [`runSignatureFor`], or
 * `null` when the worktree can't be found at all — a worktree that no longer
 * exists can never report a transition, so its marker is dropped rather than
 * left spinning. A worktree that exists but has no run under that name yet is
 * `"none"`, not `null`: that is a start still in flight.
 *
 * Returns the SAME object when nothing changed. That identity is load-bearing:
 * the caller runs this from a `useState` updater inside an effect, and a fresh
 * object every poll would re-trigger the effect forever.
 */
export function prunePending(
  cur: PendingMap,
  now: number,
  sigFor: (key: string) => string | null,
): PendingMap {
  const keys = Object.keys(cur);
  if (keys.length === 0) return cur;
  const next: PendingMap = {};
  for (const key of keys) {
    const sig = sigFor(key);
    // An action that 202'd but never produced a transition (the CLI died on
    // startup, say) would otherwise leave the control disabled for the rest
    // of the session. Expiring beats a permanently dead button.
    if (sig !== null && sig === cur[key].sigAtSet && now < cur[key].expiresAt) {
      next[key] = cur[key];
    }
  }
  return Object.keys(next).length === keys.length ? cur : next;
}

// ---------------------------------------------------------------------------
// Fuzzy matching (command palette + worktree search)
// ---------------------------------------------------------------------------

export interface FuzzyMatch {
  score: number;
  /** Indices in the haystack that the query matched, for highlighting. */
  positions: number[];
}

/**
 * Score `query` as a subsequence of `text`, or `null` when it isn't one.
 *
 * Two left-to-right scans rather than an optimal alignment — the haystacks
 * here are worktree aliases and command labels, a few dozen short strings, so
 * full backtracking buys nothing a user would notice:
 *
 * 1. **Plain leftmost**, the completeness guarantee: if the query is a
 *    subsequence at all, this finds it.
 * 2. **Boundary-anchored**, starting the first character at the earliest word
 *    start. Without it, plain greedy takes the `w` in "s(w)itch" and ranks
 *    "Switch to Runs" above "New worktree…" for `wt` — the acronym-ish shape
 *    most palette queries have.
 *
 * The higher-scoring of the two wins. Anchoring must stay an *alternative*,
 * never a replacement: jumping the first character forward can strand the
 * rest of the query ("switch to veld-web" + `wt` anchors `w` to "-(w)eb",
 * after which there is no `t`), and an item that fails to match vanishes from
 * ⌘K entirely — strictly worse than being mis-ranked.
 *
 * Bonuses do the rest of the ranking:
 *
 * - consecutive characters, so `chk` beats `c…h…k` scattered across a name
 * - matches at a word boundary, so `cv` ranks `checkout-v2` above a mid-word
 *   hit (a plain subsequence scan finds it either way)
 * - a small penalty for a late first match and for long haystacks, which
 *   breaks ties toward the tighter, shorter candidate
 *
 * An empty query matches everything with score 0 (callers keep input order).
 */
export function fuzzyMatch(text: string, query: string): FuzzyMatch | null {
  // Spaces are separators the user types between words, not characters to
  // find — "new wt" should match "New worktree…".
  const q = Array.from(query.trim().toLowerCase()).filter((c) => c !== " ");
  if (q.length === 0) return { score: 0, positions: [] };
  const t = text.toLowerCase();
  const atBoundary = (i: number) => i === 0 || !/[a-z0-9]/.test(t[i - 1]);

  /** One left-to-right pass; `firstAt` fixes where the first char matches. */
  const scan = (firstAt: number): FuzzyMatch | null => {
    const positions: number[] = [];
    let score = 0;
    let prev = -2;
    let from = firstAt;
    for (let qi = 0; qi < q.length; qi++) {
      const at = qi === 0 ? firstAt : t.indexOf(q[qi], from);
      if (at === -1) return null;
      positions.push(at);
      score += 1;
      if (at === prev + 1) score += 4;
      if (atBoundary(at)) score += 3;
      prev = at;
      from = at + 1;
    }
    return { score: score - positions[0] * 0.1 - t.length * 0.01, positions };
  };

  const leftmost = t.indexOf(q[0]);
  if (leftmost === -1) return null;

  // The plain leftmost scan is the completeness guarantee: if the query is a
  // subsequence at all, this finds it. Never return null while it succeeds.
  const plain = scan(leftmost);

  // The anchored scan ranks better when it works, but it can strand the rest
  // of the query past the boundary it jumped to — "switch to veld-web" + `wt`
  // anchors `w` to "-(w)eb", after which there is no `t`. So it's an
  // *alternative*, not a replacement: take whichever scores higher.
  let anchored: FuzzyMatch | null = null;
  for (let p = leftmost; p !== -1; p = t.indexOf(q[0], p + 1)) {
    if (atBoundary(p)) {
      anchored = p === leftmost ? plain : scan(p);
      break;
    }
  }

  // A failed plain scan means no subsequence exists at all, and the anchored
  // scan starts no earlier — so it cannot rescue one. Returning `anchored`
  // here would read as if it could, inverting the invariant above.
  if (!plain) return null;
  if (!anchored) return plain;
  // Ties keep the anchored positions: same score, better highlight.
  return anchored.score >= plain.score ? anchored : plain;
}

/** Best score across several haystacks (e.g. a worktree's alias and branch). */
export function bestFuzzyMatch(
  texts: string[],
  query: string,
): FuzzyMatch | null {
  let best: FuzzyMatch | null = null;
  for (const text of texts) {
    const m = fuzzyMatch(text, query);
    if (m && (!best || m.score > best.score)) best = m;
  }
  return best;
}
