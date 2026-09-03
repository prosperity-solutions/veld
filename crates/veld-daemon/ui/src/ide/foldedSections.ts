/**
 * Which rail sections a project has folded shut.
 *
 * The rail already had one collapse — the narrow/wide toggle — and it answers a
 * different question. That one buys *horizontal* width by dropping every header;
 * this one buys *vertical* room in a repo with more sections than fit, by keeping
 * a header and dropping its rows. They compose rather than override: a folded
 * section is only folded while the rail is wide, because a rail with no headers
 * on screen has nothing left to fold.
 *
 * Three properties are load-bearing:
 *
 * - **Per project, not per window slot.** The selection and the pane layout are
 *   slot-scoped because two windows on one project are deliberately looking at
 *   different things; a fold is a statement about the *rail*, and two windows
 *   showing the same rail with different sections open is the same class of
 *   confusion `railGroups` exists to avoid. One key, plus the `storage` event, is
 *   what makes the two windows agree — see `useFoldedSections` in `App.tsx`.
 * - **The stored value is a set of section keys** ([`RailGroup.key`]) — the lane
 *   name for a real lane, `""` for the ungrouped section, and the NUL-prefixed
 *   sentinels for Detached and the trash. `""` is a legal member, which is why
 *   this is a `Set` and not a delimited string with an "empty means none" reading.
 * - **A lane has no id** (`api.ts`: identified by `(repo root, name)`), so a fold
 *   does not survive a rename on its own — [`renameFoldedSection`] moves it and
 *   [`forgetFoldedSection`] drops it on delete. Without the second one, deleting a
 *   folded lane and later making a new one with the same name hands the user a
 *   section that is mysteriously already shut. The IDE is the only thing that
 *   creates, renames or deletes lanes (there is no `veld lane` subcommand), so
 *   those two calls are the whole hygiene story.
 *
 * One section starts folded — see [`defaultFoldedSections`]. That default lives in
 * this module rather than in the rail because [`toggleFoldedSection`] re-reads
 * before it writes: a default applied by the caller would be invisible to that
 * re-read, so folding any *other* section would write back a set with no trash in
 * it and spring the trash open as a side effect of an unrelated click.
 *
 * Nothing here is persisted by the daemon. A fold is a per-machine view
 * preference like the rail's width, and putting it on the server would make
 * "which sections are open" a thing that travels between machines — the same
 * mistake as syncing a scroll position.
 */

import { TRASH_LANE } from "../model";

/** Just enough of `Storage` to be faked in a test. */
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * The `localStorage` key a project's folds live under.
 *
 * The repo root goes in verbatim, as it does in `lastWorktreeName` — and unlike
 * that one there is no slot suffix to collide with, so the mapping is injective.
 */
export function foldedSectionsKey(repoRoot: string): string {
  return `veld.railFolded.${repoRoot}`;
}

/**
 * What a project has folded before it has said anything of its own: the **trash,
 * shut**.
 *
 * The trash is the one section that is a destination rather than a place you
 * work. Its rows are worktrees on their way off the disk — you do not select one,
 * start one, or open a pane on one — so an open trash spends rail height on a
 * list nobody reads, and spends it at the bottom, where the sections you *are*
 * working in are already competing for the same pixels. Folded it still reports
 * its count, and it still takes a drop: the header is the drop target, and a drag
 * over it paints the whole section (see `.trash-drop` in `styles.css`). That is
 * the whole of what the trash does day to day.
 *
 * **This is a default, not a rule.** The first time the user unfolds the trash,
 * the set is written without it and stays written — the default only ever
 * describes a project that has never touched a fold. Every other section starts
 * open, which is the opposite default and the right one for a section holding
 * worktrees you are working in.
 *
 * A fresh `Set` per call, never a shared constant: callers mutate what
 * [`readFoldedSections`] hands them.
 */
export function defaultFoldedSections(): Set<string> {
  return new Set([TRASH_LANE]);
}

/**
 * What this project has folded — [`defaultFoldedSections`] if it has never been
 * recorded, `∅` if the record is unreadable.
 *
 * **Never-recorded and unreadable are different answers**, and the difference is
 * the point. A project with no stored value has not disagreed with anything, so
 * it gets the defaults. A value that will not parse is a bug or a shape from a
 * future version, and there the rule below wins instead.
 *
 * **Every failure reads as "nothing is folded"** — not as the defaults. A parse
 * error, a value written by a future version with a different shape, a `getItem`
 * that throws in private browsing: all of them show every row, because a fold
 * that failed *open* is a rail with more in it than expected, while one that
 * failed *shut* would hide worktrees with no explanation.
 */
export function readFoldedSections(
  store: KeyValueStore,
  repoRoot: string,
): Set<string> {
  try {
    const raw = store.getItem(foldedSectionsKey(repoRoot));
    if (raw === null) return defaultFoldedSections();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((k): k is string => typeof k === "string"));
  } catch {
    return new Set();
  }
}

/**
 * Persist a set of folds, and hand it back for the caller's state.
 *
 * Returning the set rather than `void` is what keeps the two copies — the stored
 * one and React's — from being written in two places: every mutator below ends in
 * this call, and every caller sets state from what it returns.
 *
 * A storage failure is swallowed for the reason the other persisted view
 * preferences swallow theirs: the fold still applies for this session, and the
 * cost is a rail that comes back open next launch.
 */
export function writeFoldedSections(
  store: KeyValueStore,
  repoRoot: string,
  folded: ReadonlySet<string>,
): Set<string> {
  const next = new Set(folded);
  try {
    store.setItem(foldedSectionsKey(repoRoot), JSON.stringify([...next]));
  } catch {
    // Storage unavailable — see above.
  }
  return next;
}

/**
 * Fold a section shut, or open it again.
 *
 * Reads before it writes rather than transforming a set the caller is holding, so
 * a fold made in another window a moment ago is not clobbered by this one: both
 * windows share the key, and the loser of a race would otherwise write back a
 * snapshot taken before the other's change.
 */
export function toggleFoldedSection(
  store: KeyValueStore,
  repoRoot: string,
  sectionKey: string,
): Set<string> {
  const next = readFoldedSections(store, repoRoot);
  if (!next.delete(sectionKey)) next.add(sectionKey);
  return writeFoldedSections(store, repoRoot, next);
}

/**
 * Follow a lane through a rename, so a folded section stays folded.
 *
 * The alternative — letting the fold fall stale and be pruned — springs the
 * section open the moment it is renamed, which reads as the rename having done
 * something it did not. A rename onto a name that is *itself* folded is not
 * reachable from the UI (the dialog refuses a taken name), and collapses to one
 * entry here rather than being an error.
 */
export function renameFoldedSection(
  store: KeyValueStore,
  repoRoot: string,
  from: string,
  to: string,
): Set<string> {
  const next = readFoldedSections(store, repoRoot);
  if (!next.delete(from)) return next;
  next.add(to);
  return writeFoldedSections(store, repoRoot, next);
}

/**
 * Drop a section's fold, for a lane that no longer exists.
 *
 * Called on delete. Skipping the write when there was nothing to drop keeps the
 * common case (deleting a lane that was open) from touching storage — and from
 * waking every other window's `storage` listener for a change that is not one.
 */
export function forgetFoldedSection(
  store: KeyValueStore,
  repoRoot: string,
  sectionKey: string,
): Set<string> {
  const next = readFoldedSections(store, repoRoot);
  if (!next.delete(sectionKey)) return next;
  return writeFoldedSections(store, repoRoot, next);
}
