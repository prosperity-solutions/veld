/**
 * The app's settings state: a daemon-owned document, mirrored locally so the
 * first paint is not a guess.
 *
 * Settings are server-side because two clients — Veld Desktop and a plain browser
 * tab — talk to one daemon and have to agree. `localStorage` would silently
 * diverge between them, and "my font size reset" would have no diagnosis.
 *
 * Three properties are load-bearing:
 *
 * 1. **No hardcoded defaults here.** `GET /api/settings` returns *effective*
 *    values, so the document is always complete and TypeScript owns no copy that
 *    could drift from the Rust one.
 * 2. **Every write is a patch**, and the response replaces local state. The daemon
 *    clamps out-of-range numbers, so echoing the request back would leave a control
 *    displaying a value that was never stored.
 * 3. **Optimistic, then reconciled.** A toggle must move under the cursor
 *    immediately; the daemon's answer is what survives.
 *
 * The mirror is per-client, so the app and a browser tab hold independent caches
 * and disagree for one frame after a change made in the other. Accepted, and
 * narrowed by re-fetching on window focus rather than by inventing a push channel
 * that nothing else in this daemon has.
 */

import { useCallback, useEffect, useRef, useState } from "react";

import { api, type SettingsDoc } from "../api";
import { notifyError } from "./notify";

/** Where the local mirror lives. Not a store — a cache of the last good read. */
export const SETTINGS_CACHE_KEY = "veld.settings";

function readCache(): SettingsDoc | null {
  try {
    const raw = window.localStorage.getItem(SETTINGS_CACHE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    // A hand-edited or truncated cache must not take the app down; a bad mirror
    // degrades to "not loaded yet", which the callers already handle.
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as SettingsDoc;
  } catch {
    return null;
  }
}

function writeCache(doc: SettingsDoc): void {
  try {
    window.localStorage.setItem(SETTINGS_CACHE_KEY, JSON.stringify(doc));
  } catch {
    // A full or blocked localStorage is not a reason to fail a settings save.
  }
}

export interface SettingsState {
  /**
   * The effective document, or `null` before the first read of *either* the
   * cache or the daemon resolves.
   *
   * Callers that render sized content should prefer non-null over substituting a
   * default. Nothing enforces it, and a terminal mounted before the first read
   * recovers rather than breaking: `applyTerminalPrefs` re-styles and re-fits every
   * live session on the first publish, so the worst case is one frame at the
   * previous release's metrics — not a permanently wrong grid.
   */
  settings: SettingsDoc | null;
  /** Write some keys. Optimistic locally, reconciled from the response. */
  save: (patch: SettingsDoc) => Promise<void>;
  /** True while a save is in flight, for disabling a control mid-write. */
  saving: boolean;
  /** The last save error, cleared by the next successful save. */
  error: string | null;
}

export function useSettings(): SettingsState {
  // Seeded from the mirror, so a returning window paints at the right size on the
  // first frame instead of after a round trip.
  const [settings, setSettings] = useState<SettingsDoc | null>(readCache);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Guards against a slow GET landing after a save and reverting it.
  const generation = useRef(0);

  const load = useCallback(async () => {
    const mine = ++generation.current;
    try {
      const { settings: doc } = await api.settings();
      if (mine !== generation.current) return;
      setSettings(doc);
      writeCache(doc);
    } catch {
      // Unreachable daemon: keep the mirror and stay quiet. The offline banner is
      // already the app's one signal for a daemon that is gone, and a toast per
      // window-focus while it is down would be a stream of them.
    }
  }, []);

  useEffect(() => {
    void load();
    // Re-read on focus rather than on the 5s poll: settings change when a human
    // changes them, which is rare, and a second window's edit only matters once
    // you look at this one. Polling them would be a request per client per five
    // seconds for a document that is usually identical.
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [load]);

  const save = useCallback(async (patch: SettingsDoc) => {
    setSaving(true);
    // Optimistic: the control moves now. `null` means nothing has loaded yet, and
    // merging into `null` would publish a document with only the patched keys —
    // every other setting would read as missing and fall back.
    setSettings((prev) => (prev ? { ...prev, ...patch } : prev));
    const mine = ++generation.current;
    try {
      const { settings: doc } = await api.patchSettings(patch);
      if (mine !== generation.current) return;
      // The daemon's answer wins over the optimistic guess — this is where a
      // clamped number becomes visible.
      setSettings(doc);
      writeCache(doc);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      // Also a toast: a save can fail after the dialog has been closed, and the
      // in-dialog message would then be invisible. Toasts are the app's one error
      // surface (see `notify.ts`), so a silent failed write is the one outcome
      // this must not have — the optimistic value is still on screen.
      notifyError("Could not save settings", e);
      // Re-read rather than trying to invert the optimistic write: the daemon is
      // the only thing that knows what actually landed.
      await load();
      throw e;
    } finally {
      setSaving(false);
    }
  }, [load]);

  return { settings, save, saving, error };
}
