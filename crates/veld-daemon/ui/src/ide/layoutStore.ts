/**
 * The pane layout store, backed by the daemon.
 *
 * Replaces three browser-storage keys (`veld.panes.v1` in `sessionStorage`,
 * `veld.panes.slot.<slot>.v1` and `veld.panes.worktrees.v1` in `localStorage`)
 * with one row per worktree in the daemon's database. The old arrangement made
 * "the panes of worktree 7" a per-client answer: a browser tab and a desktop
 * window showed different sets, and the tab could not re-attach to the app's
 * terminals because their session ids only ever existed in the app's storage.
 *
 * What that deletes, and why none of it is missed: the per-slot store and its
 * `restored` gate, the window seed's role as a layout source of last resort, and
 * the read-through merge — all three existed because several clients wrote one
 * key and none of them could see the others. One row with a version has no such
 * problem.
 *
 * **Writes are versioned, not merged.** One client shows a worktree at a time
 * (the daemon's claim registry), so concurrent edits are prevented rather than
 * resolved. The version exists for the hand-off: the client that yields a
 * worktree can still have a debounced save in flight when the client that
 * claimed it starts editing, and the stale writer must adopt what it is told
 * instead of overwriting real panes.
 */

import { api } from "../api";
import { allTabs, parseLayout, type PaneLayout } from "../panes/model";
import { clientId } from "./channel";

/**
 * Where main-window layouts lived before they were the daemon's.
 *
 * Read once per worktree, only when the database has nothing for it, and the
 * entry is removed as soon as it is adopted. Kept as a literal here rather than
 * imported from `panes/model.ts`, which no longer has any reason to know the
 * key: this is the only code that should ever touch it again.
 */
const LEGACY_WORKTREE_KEY = "veld.panes.worktrees.v1";

/**
 * A worktree's panes from the old browser store, if they are still there.
 *
 * Existing installs have a real layout in here — terminals whose holder
 * processes are very likely still running — so discarding it would greet the
 * update with an empty screen and shells nothing can reach until the detach
 * grace ends them. Adopting it is also safe by construction: it is written with
 * `expected: 0`, so a client that finds the row already created loses the
 * version check and takes what is really there instead.
 */
function takeLegacyLayout(worktreeId: number): PaneLayout | null {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(LEGACY_WORKTREE_KEY);
  } catch {
    // Storage access throws outright in some privacy configurations.
    return null;
  }
  if (!raw) return null;
  let all: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    all = parsed as Record<string, unknown>;
  } catch {
    return null;
  }
  const key = String(worktreeId);
  if (!(key in all)) return null;
  const layout = parseLayout(all[key]);
  // Removed whether or not it parsed: an entry that cannot be read is one this
  // would fail on again on every future visit to the worktree.
  delete all[key];
  try {
    localStorage.setItem(LEGACY_WORKTREE_KEY, JSON.stringify(all));
  } catch {
    // Losing the removal only costs a repeat attempt that the version check
    // refuses; it cannot overwrite anything.
  }
  return layout;
}

/**
 * How long a layout sits before it is written.
 *
 * A drag emits a change per frame; a split-ratio drag emits dozens. Long enough
 * to collapse one gesture into one write, short enough that closing the lid a
 * moment after dropping a tab still records it.
 */
const WRITE_DEBOUNCE_MS = 400;

/** The version this client last saw for a worktree. `0` means "no row". */
const versions = new Map<number, number>();

/** What was last successfully written, so an unchanged layout is not re-sent. */
const written = new Map<number, string>();

/** Pending debounced writes, by worktree. */
const timers = new Map<number, ReturnType<typeof setTimeout>>();

/** The most recent layout for each worktree with a write outstanding. */
const queued = new Map<number, PaneLayout>();

type ExternalChange = (worktreeId: number, layout: PaneLayout | null) => void;

let onExternal: ExternalChange = () => {};

/**
 * Learn about layouts this client did not write.
 *
 * Two sources, both of which have to reach React state or the screen keeps
 * rendering something the database disagrees with: a write that lost a version
 * check, and the daemon's `layout_changed` push.
 */
export function onExternalLayoutChange(fn: ExternalChange): void {
  onExternal = fn;
}

/** Whether a layout has any panes at all. */
function isEmpty(layout: PaneLayout): boolean {
  return allTabs(layout).length === 0;
}

/**
 * Read a worktree's panes.
 *
 * `null` means nobody has arranged this worktree, which is the only case in
 * which the caller seeds a default. A layout that fails to parse — written by a
 * newer build, or hand-edited in `sqlite3` — reads as `null` for the same reason
 * `parseLayouts` discards a malformed entry: a default is recoverable, a throw
 * during the first render white-screens the app.
 */
export async function readLayout(worktreeId: number): Promise<PaneLayout | null> {
  const doc = await api.paneLayout(worktreeId);
  versions.set(worktreeId, doc.version);
  if (doc.layout === null) {
    written.delete(worktreeId);
    // Nothing in the database — so this is either a worktree nobody has arranged
    // or one whose layout is still in the store this replaced. Adopting it here
    // rather than in a migration pass is what keeps it a non-event: it happens
    // the first time the worktree is opened, once, and needs no flag to record
    // that it has already run.
    const legacy = takeLegacyLayout(worktreeId);
    if (legacy) {
      writeLayout(worktreeId, legacy);
      return legacy;
    }
    return null;
  }
  const parsed = parseLayout(doc.layout);
  // Record what the server holds even when it does not parse: without this the
  // next write would present version 0, be refused, and adopt the unreadable
  // document it just rejected.
  written.set(worktreeId, JSON.stringify(doc.layout));
  return parsed;
}

/**
 * Re-read a worktree the daemon says has moved, and hand it to the app.
 *
 * Only useful for a worktree this client is showing; the caller decides that.
 */
export async function refreshLayout(worktreeId: number): Promise<void> {
  try {
    onExternal(worktreeId, await readLayout(worktreeId));
  } catch {
    // A failed re-read leaves the screen as it was, which is the same state a
    // client that never got the push would be in.
  }
}

/**
 * Queue a worktree's panes for writing.
 *
 * Idempotent per layout: an identical document is dropped rather than written,
 * which matters because the app's save effect runs on every `layouts` change
 * including ones that only touched a different worktree.
 */
export function writeLayout(worktreeId: number, layout: PaneLayout): void {
  const serialized = JSON.stringify(layout);
  if (written.get(worktreeId) === serialized) return;
  queued.set(worktreeId, layout);
  const existing = timers.get(worktreeId);
  if (existing) clearTimeout(existing);
  timers.set(
    worktreeId,
    setTimeout(() => void flush(worktreeId), WRITE_DEBOUNCE_MS),
  );
}

/**
 * Write every worktree whose layout has changed.
 *
 * The whole-object entry point, so the app's existing save effect keeps its
 * shape. **Omission is not deletion** — the same rule the read-through merge
 * had, and for the same reason: a client that yields a worktree drops it from
 * its own state while its panes go on existing for whoever takes it. Only
 * [`dropLayout`] deletes.
 */
export function syncLayouts(layouts: Record<number, PaneLayout>): void {
  for (const [key, layout] of Object.entries(layouts)) {
    const id = Number(key);
    if (Number.isInteger(id)) writeLayout(id, layout);
  }
}

/** Write a queued layout now, if there is one. */
async function flush(worktreeId: number): Promise<void> {
  timers.delete(worktreeId);
  const layout = queued.get(worktreeId);
  if (!layout) return;
  queued.delete(worktreeId);

  const version = versions.get(worktreeId) ?? 0;
  // A worktree whose last pane was closed has no row, so the next client to open
  // it seeds a default rather than restoring an empty screen. Same outcome the
  // old store reached by having `parseLayout` reject a tabless layout on read,
  // spelled as a delete so the database does not accumulate them.
  const payload = isEmpty(layout) ? null : layout;

  let result: Awaited<ReturnType<typeof api.putPaneLayout>>;
  try {
    result = await api.putPaneLayout(worktreeId, version, payload, clientId);
  } catch {
    // The daemon is down, or the worktree was deleted under us. Nothing to tell
    // the user — the layout is on screen and will be written on the next change
    // — but the recorded version must not advance.
    return;
  }

  if (result.ok) {
    versions.set(worktreeId, result.doc.version);
    if (payload === null) written.delete(worktreeId);
    else written.set(worktreeId, JSON.stringify(layout));
    return;
  }

  // **Lost the version check.** Somebody else owns this worktree's panes now.
  // Adopt theirs rather than retrying with ours: a retry is how a stale client
  // wins a race it has already lost, and the panes it would restore name
  // terminal sessions the new owner is attached to.
  versions.set(worktreeId, result.conflict.version);
  if (result.conflict.layout === null) {
    written.delete(worktreeId);
    onExternal(worktreeId, null);
    return;
  }
  written.set(worktreeId, JSON.stringify(result.conflict.layout));
  onExternal(worktreeId, parseLayout(result.conflict.layout));
}

/**
 * Forget a worktree's panes.
 *
 * For a worktree that is *gone*. Letting go of one (a yield) is not this: the
 * panes stay for whoever takes it.
 */
export function dropLayout(worktreeId: number): void {
  const timer = timers.get(worktreeId);
  if (timer) clearTimeout(timer);
  timers.delete(worktreeId);
  queued.delete(worktreeId);
  versions.delete(worktreeId);
  written.delete(worktreeId);
  void api.putPaneLayout(worktreeId, 0, null, clientId).catch(() => {});
}

/**
 * Everything this client has queued but not written, sent before the page goes.
 *
 * A debounce is a window in which a closing tab loses the last thing the user
 * did — moved a tab, resized a split, then hit ⌘W. `keepalive` is what lets the
 * request outlive the document.
 */
export function flushPendingOnUnload(): void {
  for (const [worktreeId, timer] of timers) {
    clearTimeout(timer);
    const layout = queued.get(worktreeId);
    if (!layout) continue;
    const version = versions.get(worktreeId) ?? 0;
    const payload = isEmpty(layout) ? null : layout;
    // Not through `api`, which cannot express `keepalive`. A conflict here is
    // unobservable and that is acceptable: the page is going away, and the
    // client that holds the worktree afterwards re-reads.
    try {
      void fetch(`/api/worktrees/${worktreeId}/layout`, {
        method: "PUT",
        keepalive: true,
        headers: { "Content-Type": "application/json", "X-Veld-Request": "1" },
        body: JSON.stringify({ version, layout: payload, client_id: clientId }),
      });
    } catch {
      // Nothing useful to do while the document is being torn down.
    }
  }
  timers.clear();
  queued.clear();
}

/** Reset every cached version — the daemon restarted, so nothing is current. */
export function forgetVersions(): void {
  versions.clear();
  written.clear();
}
