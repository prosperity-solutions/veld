// Pure join/derivation helpers between the desktop registry (/api/repos) and
// veld run state (/api/environments). Every worktree with a veld.json is its
// own veld project root, so the join key is worktree.path === project_root.

import type { EnvironmentList, Lane, RunInfo, Worktree } from "./api";

export type WorktreeStatus = "running" | "partial" | "failed" | "stopped";

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
 * listed. A worktree normally has one environment; when it has several the
 * ordering is the daemon's, which is the honest answer to "which one" without
 * inventing a rule the user cannot see.
 */
export function diagnosticsRun(runs: RunInfo[]): RunInfo | null {
  return activeRun(runs) ?? runs.find((r) => r.live) ?? runs[0] ?? null;
}

/**
 * Rail status dot: running (green, pulsing), partial (amber, in transition),
 * failed (red), stopped (gray).
 */
export function worktreeStatus(runs: RunInfo[]): WorktreeStatus {
  const run = activeRun(runs);
  if (!run) return "stopped";
  if (run.status === "running") return "running";
  if (run.status === "failed") return "failed";
  return "partial";
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
 * Group key for worktrees pending removal.
 *
 * Not a lane name: a lane is user-defined and this is a state, so a repo with a
 * lane literally called "pending removal" must not merge with it. The leading
 * NUL cannot occur in a lane name (the daemon trims and bounds them, and the
 * name comes from a text input) so the two key spaces cannot collide.
 */
export const TRASH_LANE = "\u0000trash";

/**
 * Split a repo's worktrees into rail sections: ungrouped first, then each lane in
 * its own order, then pending removals.
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
      label: "Pending removal",
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
 * Pending removals are excluded from the returned order and cannot be a drop
 * target: they are leaving, so placing one is meaningless.
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
    const rest = g.worktrees.filter((w) => w.path !== path);
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

/** Markers keyed by worktree id. */
export type PendingMap = Record<number, PendingMarker>;

/**
 * Drop markers whose signature has moved, or whose deadline has passed.
 *
 * A moved signature means *something* happened, not necessarily the thing
 * that was fired — this is a change detector, not an acknowledgement. Firing
 * Stop against a run that is still `starting` clears the marker as soon as it
 * reaches `running` on its own, while the stop is still in flight. Correlating
 * properly would need the daemon to hand back an action token.
 *
 * `sigFor` returns the worktree's current [`runSignature`], or `null` when the
 * worktree can't be found at all — a worktree that no longer exists can never
 * report a transition, so its marker is dropped rather than left spinning.
 *
 * Returns the SAME object when nothing changed. That identity is load-bearing:
 * the caller runs this from a `useState` updater inside an effect, and a fresh
 * object every poll would re-trigger the effect forever.
 */
export function prunePending(
  cur: PendingMap,
  now: number,
  sigFor: (worktreeId: number) => string | null,
): PendingMap {
  const ids = Object.keys(cur);
  if (ids.length === 0) return cur;
  const next: PendingMap = {};
  for (const key of ids) {
    const id = Number(key);
    const sig = sigFor(id);
    // An action that 202'd but never produced a transition (the CLI died on
    // startup, say) would otherwise leave the control disabled for the rest
    // of the session. Expiring beats a permanently dead button.
    if (sig !== null && sig === cur[id].sigAtSet && now < cur[id].expiresAt) {
      next[id] = cur[id];
    }
  }
  return Object.keys(next).length === ids.length ? cur : next;
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
