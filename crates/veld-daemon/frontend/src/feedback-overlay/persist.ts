import { getState } from "./store";
import { refs } from "./refs";

/**
 * Tab-scoped persistence of UNSENT overlay state across a page reload.
 *
 * The overlay is re-injected from scratch on every reload — dev-server HMR
 * (browser-sync does a full reload), the agent's hot-reload after a fix, or a
 * plain refresh — which otherwise throws away whatever the reviewer was in the
 * middle of: a half-typed comment, which element it was attached to, the open
 * panel + tab, the panel's scroll position, and half-typed thread replies.
 * This module mirrors that state into sessionStorage so a reload is invisible.
 *
 * Scope, deliberately:
 *  - sessionStorage, not localStorage — the state is tab-local and should die
 *    with the tab (the same call the gateway makes for SHARE_SEEN_KEY, and the
 *    overlay already makes for `veld-feedback-scroll-to-thread` in
 *    navigation.ts). A run maps to its own hostname, so the browser scopes the
 *    key per run automatically.
 *  - keyed by the full path+query+hash — element selectors are page-relative
 *    and a draft belongs to the exact URL it was written on. Including the
 *    query and hash (not just the pathname) keeps hash-routed and
 *    query-distinguished SPA "pages" (which share one pathname) from colliding
 *    onto one key. A reload preserves path+query+hash, so this doesn't weaken
 *    the reload case it's built for. (A live composer that survives a
 *    client-side navigation to a different URL is prevented from writing under
 *    the new URL's key at the save site — see popover.ts.)
 *  - only UNSENT, in-browser state. Threads themselves are persisted
 *    server-side (SQLite); nothing here duplicates or races that.
 *  - the screenshot composer is deliberately NOT persisted: its draft is
 *    anchored to a captured frame (a live MediaStream grab) that cannot be
 *    reconstructed after the page is torn down, so restoring the text alone
 *    would resurrect a composer with no image. Only the element/page comment
 *    composer is restorable.
 *
 * Never throws: merely touching sessionStorage throws in sandboxed /
 * cookie-blocked iframes, so every access is wrapped — a storage failure
 * degrades to "no restore", never a broken overlay boot.
 */

const KEY_PREFIX = "veld-feedback-session:";

/** The URL a draft belongs to: path + query + hash (see the module doc for
 *  why all three). Exposed so the composer save site can detect a client-side
 *  navigation away from the URL a draft was opened on. */
export function pageKey(): string {
  return window.location.pathname + window.location.search + window.location.hash;
}

function storageKey(): string {
  return KEY_PREFIX + pageKey();
}

/** The open new-comment composer (popover), enough to rebuild + re-anchor it
 *  after the DOM is torn down and re-created. */
export interface ComposerDraft {
  text: string;
  /** true = page/global comment (no element); false = element-scoped. */
  isPage: boolean;
  selector: string | null;
  tagInfo: string | null;
  trace: string[] | null;
  elementText: string | null;
  sourceFile: string | null;
  sourceLine: number | null;
  /** Document-relative anchor rect captured at open time (see helpers.docRect).
   *  Used as-is for page comments and as the fallback when the element can no
   *  longer be found; re-derived from the live element when it still exists. */
  rect: { x: number; y: number; width: number; height: number };
}

interface PersistedSession {
  v: 1;
  panelOpen?: boolean;
  panelTab?: "active" | "resolved";
  expandedThreadId?: string | null;
  panelScrollTop?: number;
  composer?: ComposerDraft | null;
  /** threadId -> in-progress reply text. */
  replies?: Record<string, string>;
}

function read(key: string = storageKey()): PersistedSession {
  try {
    const raw = sessionStorage.getItem(key);
    if (raw) {
      const parsed = JSON.parse(raw) as PersistedSession;
      // Ignore anything not written by this schema version — a future bump can
      // then change the shape without a stale blob crashing the restore.
      if (parsed && parsed.v === 1) return parsed;
    }
  } catch (_) { /* ignore */ }
  return { v: 1 };
}

function write(s: PersistedSession, key: string = storageKey()): void {
  try {
    sessionStorage.setItem(key, JSON.stringify(s));
  } catch (_) { /* ignore */ }
}

/** Read-modify-write one field group without clobbering the rest. Defaults to
 *  the current page's key; a specific key targets another page's blob. */
function mutate(fn: (s: PersistedSession) => void, key: string = storageKey()): void {
  const s = read(key);
  fn(s);
  write(s, key);
}

/** Snapshot the panel's open/tab/expanded/scroll — the "where was I" bits. */
export function savePanelState(): void {
  mutate((s) => {
    const st = getState();
    s.panelOpen = st.panelOpen;
    s.panelTab = st.panelTab;
    s.expandedThreadId = st.expandedThreadId;
    // panelBody may not be built yet during very early boot; guard it.
    s.panelScrollTop = refs.panelBody ? refs.panelBody.scrollTop : 0;
  });
}

/** rAF-coalesced panel save for high-frequency events (scroll). */
let saveScheduled = false;
export function schedulePanelSave(): void {
  if (saveScheduled) return;
  saveScheduled = true;
  requestAnimationFrame(() => {
    saveScheduled = false;
    savePanelState();
  });
}

export function saveComposerDraft(draft: ComposerDraft): void {
  mutate((s) => { s.composer = draft; });
}

/** Clear the composer draft. `originPageKey` (from `pageKey()` captured when the
 *  composer opened) targets the page the composer belongs to — the overlay lets
 *  a composer outlive a client-side navigation, so a dismiss must clear the
 *  draft's origin page, not whatever page the app happens to be showing now. */
export function clearComposerDraft(originPageKey?: string): void {
  const key = originPageKey ? KEY_PREFIX + originPageKey : storageKey();
  mutate((s) => { s.composer = null; }, key);
}

export function getComposerDraft(): ComposerDraft | null {
  return read().composer ?? null;
}

export function saveReplyDraft(threadId: string, text: string): void {
  mutate((s) => {
    const replies = s.replies || (s.replies = {});
    // Drop empties so cleared boxes don't linger and re-open on reload.
    if (text) replies[threadId] = text;
    else delete replies[threadId];
  });
}

export function clearReplyDraft(threadId: string): void {
  mutate((s) => { if (s.replies) delete s.replies[threadId]; });
}

/** Drop reply drafts whose thread is no longer restorable (resolved or deleted
 *  out-of-band): those threads render no reply box, so their draft would never
 *  otherwise be cleared until the session ends. Keeps the blob bounded. */
export function pruneReplyDrafts(keepThreadIds: Iterable<string>): void {
  const keep = new Set(keepThreadIds);
  mutate((s) => {
    if (!s.replies) return;
    for (const id of Object.keys(s.replies)) {
      if (!keep.has(id)) delete s.replies[id];
    }
  });
}

export function getReplyDraft(threadId: string): string {
  return read().replies?.[threadId] ?? "";
}

export interface PanelSnapshot {
  open: boolean;
  tab: "active" | "resolved";
  expandedThreadId: string | null;
  scrollTop: number;
}

export function getPanelState(): PanelSnapshot {
  const s = read();
  return {
    open: !!s.panelOpen,
    tab: s.panelTab === "resolved" ? "resolved" : "active",
    expandedThreadId: s.expandedThreadId ?? null,
    scrollTop: typeof s.panelScrollTop === "number" ? s.panelScrollTop : 0,
  };
}

/** Wipe the whole tab-local feedback session across every page — the reviewer
 *  clicked Done, which ends the session globally, so no unsent state on any
 *  page (this key or a draft left on another route) is worth restoring. */
export function clearSession(): void {
  try {
    const keys: string[] = [];
    for (let i = 0; i < sessionStorage.length; i++) {
      const k = sessionStorage.key(i);
      if (k && k.startsWith(KEY_PREFIX)) keys.push(k);
    }
    keys.forEach((k) => sessionStorage.removeItem(k));
  } catch (_) { /* ignore */ }
}
