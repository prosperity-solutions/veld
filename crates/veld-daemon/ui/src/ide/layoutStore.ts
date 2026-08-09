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
import { channel } from "./channel";

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

/**
 * Drop a queued write for a worktree, because what it was based on is gone.
 *
 * **The version is read at flush time, not at queue time**, so a write that
 * survives an intervening read would be sent against a version it never saw —
 * and be *accepted*, silently replacing whatever that read had just adopted.
 * That is the write the version exists to refuse, arriving through the one gap
 * the check cannot see. Every path that replaces this client's picture of a
 * worktree therefore cancels the write that belonged to the old one.
 */
export function cancelPendingWrite(worktreeId: number): void {
  const timer = timers.get(worktreeId);
  if (timer) clearTimeout(timer);
  timers.delete(worktreeId);
  queued.delete(worktreeId);
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
  // The boot sweep takes an entry out of the old store before it pushes it, so
  // reading past a sweep in flight would find nothing and seed a default over
  // the layout being moved.
  if (adopting) await adopting.catch(() => {});
  const doc = await api.paneLayout(worktreeId);
  // Anything queued was composed against the layout this read replaces — see
  // `cancelPendingWrite`. Cancelled *before* the version moves, so there is no
  // window in which a stale write could pick up the fresh one.
  cancelPendingWrite(worktreeId);
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
  // **Keyed on the parsed form, which is what `writeLayout` compares against.**
  // Keying on the server's document instead meant the two strings never matched
  // — `serde_json` re-serialises a `Value` with its keys sorted, so the daemon
  // returns `docks, focused, ratio` where the model builds `docks, ratio,
  // focused` — and the dedupe silently never fired on this path: every worktree
  // *open* wrote a new version and broadcast a change to every other client.
  // A document that does not parse records nothing, so the next write goes out
  // and replaces it; the version was recorded above either way, which is what
  // stops that write presenting 0 and adopting the unreadable row it rejected.
  if (parsed) written.set(worktreeId, JSON.stringify(parsed));
  else written.delete(worktreeId);
  return parsed;
}

/**
 * Move every layout still in the old browser store into the daemon, once, at
 * boot.
 *
 * The lazy per-worktree adoption in [`readLayout`] is not enough on its own, and
 * the gap is the one this whole change creates: **the first client to open a
 * worktree after the update creates its row**, and a browser tab is a different
 * origin from Veld Desktop (`https://veld.localhost` versus
 * `http://127.0.0.1:19899`), so it has no old store to adopt from. It would seed
 * a default, and the app — finding a row — would never look in its own
 * `localStorage` again. The user's panes, naming shells still running under the
 * detach grace, would be stranded permanently.
 *
 * So the client that *has* the old layouts pushes all of them as soon as it
 * starts, rather than waiting to be asked for each one.
 *
 * Each entry is removed whatever the outcome. Accepted is obvious; refused means
 * a row already exists, and that row is the one in use — keeping the old copy
 * would only leave it to resurrect dead session ids at some later boot.
 */
let adopting: Promise<void> | null = null;

/**
 * Start the boot sweep, once, and expose it for anything that must not race it.
 *
 * [`readLayout`] awaits it: the sweep removes an entry from the old store
 * *before* pushing it, so a read that interleaved would find the entry already
 * taken, return `null`, and let the app seed a default over the layout being
 * adopted.
 */
export function adoptLegacyLayouts(): Promise<void> {
  adopting ??= sweepLegacyLayouts();
  return adopting;
}

async function sweepLegacyLayouts(): Promise<void> {
  let ids: number[];
  try {
    const raw = localStorage.getItem(LEGACY_WORKTREE_KEY);
    if (!raw) return;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return;
    ids = Object.keys(parsed as Record<string, unknown>)
      .map(Number)
      .filter(Number.isInteger);
  } catch {
    return;
  }
  for (const id of ids) {
    const layout = takeLegacyLayout(id);
    if (!layout) continue;
    try {
      // Straight to the daemon rather than through `writeLayout`: this must not
      // sit in the debounce, and it must not disturb the version bookkeeping of
      // a worktree this client may be opening at the same moment.
      await api.putPaneLayout(id, 0, layout, channel.identity);
    } catch {
      // A worktree that no longer exists (the foreign key refuses it) or an
      // unreachable daemon. The entry is gone either way — a layout for a
      // deleted checkout has nowhere to go, and one that missed its window will
      // be seeded fresh, which is recoverable where a resurrected session id is
      // not.
    }
  }
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
    result = await api.putPaneLayout(worktreeId, version, payload, channel.identity);
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
  // Keyed on the parsed form for the same reason the read path is: the daemon's
  // key order differs, and `parseLayout` normalises besides — so keying on the
  // server's document meant the loser immediately echoed the winner's layout
  // back at the winner's version, taking the worktree from them, and the
  // winner's next real edit was then the one refused.
  const adopted = parseLayout(result.conflict.layout);
  if (adopted) written.set(worktreeId, JSON.stringify(adopted));
  else written.delete(worktreeId);
  onExternal(worktreeId, adopted);
}

/**
 * Forget a worktree's panes.
 *
 * For a worktree that is *gone*. Letting go of one (a yield) is not this: the
 * panes stay for whoever takes it.
 */
export function dropLayout(worktreeId: number): void {
  cancelPendingWrite(worktreeId);
  // Read before clearing: the delete is versioned like every other write, so
  // presenting 0 for a worktree that has a row is a guaranteed refusal — the
  // call did nothing and the whole behaviour rested on the foreign key.
  const version = versions.get(worktreeId) ?? 0;
  versions.delete(worktreeId);
  written.delete(worktreeId);
  void api.putPaneLayout(worktreeId, version, null, channel.identity).catch(() => {});
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
    // **The version is not advanced, and that is deliberate.** Assuming the
    // write landed was the first version of this and it recreated the very
    // defect the version exists to catch: a keepalive PUT that 409s is
    // unobservable, so guessing `version + 1` can land on exactly what the
    // *winner* wrote — after which this client's next save is accepted and
    // replaces panes another client is attached to. Left where it is, that save
    // loses its version check and adopts, which is the correct outcome. The cost
    // is one wasted round trip on a page restored from the bfcache; the
    // alternative was a silent clobber.
    //
    // `written` *is* cleared, and that is a separate question from the version:
    // it records what the server is known to hold, and after a request whose
    // outcome nobody sees it is no longer known. Left in place, a page restored
    // from the bfcache whose user returns the layout to exactly its pre-unload
    // shape has that write deduped away, and the two diverge silently.
    written.delete(worktreeId);
    //
    // Not through `api`, which cannot express `keepalive`. Two things this
    // cannot promise, both stated rather than papered over: a conflict is
    // unobservable (the page is going away, and whoever holds the worktree next
    // re-reads), and the Fetch spec caps *all* in-flight keepalive bodies at
    // 64 KiB — well past any real layout, but a pathological one is dropped
    // here rather than truncated.
    try {
      void fetch(`/api/worktrees/${worktreeId}/layout`, {
        method: "PUT",
        keepalive: true,
        headers: { "Content-Type": "application/json", "X-Veld-Request": "1" },
        body: JSON.stringify({ version, layout: payload, client_id: channel.identity }),
      });
    } catch {
      // Nothing useful to do while the document is being torn down.
    }
  }
  timers.clear();
  queued.clear();
}
