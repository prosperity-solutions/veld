/**
 * Keeping the worktree inbox across a page reload.
 *
 * A reload is precisely when you were not looking, so losing the inbox to one is the
 * feature failing at its own job. It kept happening because the store lives in memory —
 * see `inbox.ts` on why it lives in the browser at all.
 *
 * **`sessionStorage`, not `localStorage`**: the inbox belongs to this window. Two windows
 * showing different worktrees have different news, and `localStorage` is shared across
 * every tab on the origin — restoring another window's events here would badge panes this
 * window does not have. `sessionStorage` is per tab, which is the same scope the store
 * already has. It is also cleared when the tab closes, which is correct: a closed window's
 * unread events are not news for the next one.
 *
 * Split from `inbox.ts` so the store stays pure. The UI's tests run with
 * `environment: "node"` — no DOM, no `sessionStorage` — so the store's own tests exercise
 * `snapshot`/`restore` as functions while this module's storage access stays out of them.
 */

import { inbox } from "./inbox";
import { layoutSlot } from "../shell";

/**
 * Per window, keyed exactly as the pane layouts are.
 *
 * `layoutSlot` is what distinguishes a detached window from the main one, so two windows
 * in the same tab-storage scope keep their own inboxes rather than fighting over one key.
 */
function storageKey(): string {
  return `veld.inbox.${layoutSlot}`;
}

/**
 * How long to wait before writing.
 *
 * Events arrive in bursts — a build finishing marks a pane and re-renders the rail — and a
 * `JSON.stringify` plus a synchronous storage write per event is work on the frame that is
 * already busy. Short enough that a reload a moment later still finds it.
 */
const WRITE_DEBOUNCE_MS = 250;

let timer: number | null = null;

function write(): void {
  try {
    sessionStorage.setItem(storageKey(), JSON.stringify(inbox.snapshot()));
  } catch {
    // Private browsing, a full quota, a storage-less embedder. The inbox still works;
    // it just will not survive the next reload, which is where it was before this.
  }
}

/**
 * Restore the inbox from this window's last paint, then keep it saved.
 *
 * Call once, at boot, before the panes mount. Returns an unsubscribe for symmetry;
 * nothing calls it, because the store outlives every component.
 *
 * The restore is a **merge** (see `WorktreeInbox.restore`), so ordering against a pane
 * that reports immediately is safe either way.
 */
export function persistInbox(): () => void {
  try {
    const raw = sessionStorage.getItem(storageKey());
    if (raw) inbox.restore(JSON.parse(raw));
  } catch {
    // Unreadable or not JSON — an older build, or a hand-edited value. Starting empty is
    // the pre-feature behaviour and strictly better than throwing during boot.
  }
  const stop = inbox.subscribe(() => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      write();
    }, WRITE_DEBOUNCE_MS) as unknown as number;
  });
  // A reload can happen inside the debounce window, which would drop the very events this
  // module exists to keep. `pagehide` rather than `unload`: it fires for a reload *and*
  // when the page goes into the back/forward cache, and it is the one the browsers have
  // not deprecated.
  const flush = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    write();
  };
  window.addEventListener("pagehide", flush);
  return () => {
    stop();
    window.removeEventListener("pagehide", flush);
  };
}
