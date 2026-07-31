// Pure window-registry logic: slot naming, the persisted window list, and which
// suffix the next window gets.
//
// Its own module, Electron-free, for the same reason `validate.js` is one: the
// interesting failures here are arithmetic and parsing (a suffix that collides,
// a stored list that no longer parses), and `node --test src/*.test.js` can only
// reach them without an Electron binary. `windows.js` holds everything that
// needs a `BrowserWindow`.

/**
 * How many windows one app may have open at once.
 *
 * Not a resource limit — it is the backstop on a detach that goes wrong. Detach
 * opens a window from a renderer request, so a loop in the page (or a stuck
 * `dragend`) turns into windows until the machine gives up. Each one also costs
 * a renderer process and its own budget of 16 embedded views, so the ceiling is
 * real either way. Eight is past what a two-monitor desk uses.
 */
const MAX_WINDOWS = 8;

/**
 * Window suffixes, which become the tail of a layout slot and therefore of a
 * `localStorage` key. `w2`… `w99`; the first window has no suffix at all and
 * keeps the bare base (`main` / `dev`), so an app that never opens a second
 * window reads exactly the key it read before slots were per-window — an
 * upgrade must not lose the layout it already had.
 */
const SUFFIX_RE = /^w([2-9]|[1-9][0-9])$/;

/** The kinds of window. `main` is the full `/ide`; `detached` is a bare dock
 *  holding tabs pulled out of another window. */
const WINDOW_KINDS = ["main", "detached"];

/** Minimum a window may be restored at, so a stored 1×1 rect cannot render the
 *  app unusable and unresizable-in-practice. */
const MIN_BOUND_PX = 200;
const MAX_BOUND_PX = 20000;

function isSuffix(value) {
  return typeof value === "string" && SUFFIX_RE.test(value);
}

/**
 * The layout slot a window owns.
 *
 * `base` is the per-*process* namespace (`main` when packaged, `dev` otherwise,
 * or a pid-derived fallback when another live process already holds it — see
 * `claimSlot`), and the suffix is what makes it per-window. Both halves are
 * needed: the base keeps a dev run out of the installed app's terminals, and the
 * suffix keeps two windows of one app out of each other's.
 */
function slotFor(base, suffix) {
  return suffix ? `${base}-${suffix}` : base;
}

/**
 * The lowest suffix nobody is using, or `null` when the app is at its ceiling.
 *
 * *Lowest*, not next-after-the-highest: closing window 2 and opening another
 * should reuse `w2` rather than march the numbering upward forever, because the
 * suffix ends up in a durable storage key. Reuse plus `MAX_WINDOWS` is what
 * bounds `localStorage` here — the set of layout keys one base can ever produce
 * is the eight this can return, so a closed window's abandoned layout is
 * overwritten by the next window rather than accumulating. Nothing prunes those
 * keys, and nothing should: a pruner running in one window would be deleting
 * another window's live layout.
 *
 * @param {Set<string|null>} taken suffixes currently in use; `null` for the
 *   first window's bare base, which is why the search starts at 2.
 */
function nextSuffix(taken) {
  for (let n = 2; n <= 99; n++) {
    const candidate = `w${n}`;
    if (!taken.has(candidate)) return candidate;
  }
  return null;
}

/** Whether another window may be opened. Counts the base window too. */
function canOpenAnother(count) {
  return count < MAX_WINDOWS;
}

function safeBounds(raw) {
  if (typeof raw !== "object" || raw === null) return null;
  const out = {};
  for (const key of ["x", "y", "width", "height"]) {
    const n = Number(raw[key]);
    if (!Number.isFinite(n)) return null;
    out[key] = Math.round(n);
  }
  // Position is allowed to be negative (a display to the left of the primary);
  // size is not allowed to be nonsense. A rect that fails this is dropped whole
  // rather than repaired — half a remembered position is worse than none, since
  // Electron centres a window with no bounds and that is a sane answer.
  if (out.width < MIN_BOUND_PX || out.height < MIN_BOUND_PX) return null;
  if (out.width > MAX_BOUND_PX || out.height > MAX_BOUND_PX) return null;
  if (Math.abs(out.x) > MAX_BOUND_PX || Math.abs(out.y) > MAX_BOUND_PX) return null;
  return out;
}

/**
 * One persisted window, or `null` if the entry is unusable.
 *
 * This parses a file this process wrote, so nothing here is adversarial — but it
 * is written by *older builds* and read on every launch, and a throw in the
 * launch path is an app that does not start. Everything degrades.
 */
function parseWindowRecord(value) {
  if (typeof value !== "object" || value === null) return null;
  const kind = WINDOW_KINDS.includes(value.kind) ? value.kind : "main";
  const suffix = isSuffix(value.suffix) ? value.suffix : null;
  // A detached window remembers which window it came from, so closing it hands
  // its tabs back to the same place across an app restart. An origin naming a
  // window that no longer exists is resolved at hand-back time, not here.
  const origin = isSuffix(value.origin) ? value.origin : null;
  return { suffix, kind, origin, bounds: safeBounds(value.bounds) };
}

/**
 * The windows to reopen for `base`, in the order they were created.
 *
 * Duplicate suffixes are dropped rather than merged: two windows on one slot
 * would restore one layout twice and fight over every terminal in it, which is
 * the exact failure slots exist to prevent.
 */
function parseWindowList(raw, base) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return [];
  const list = parsed[base];
  if (!Array.isArray(list)) return [];
  const seen = new Set();
  const out = [];
  for (const entry of list) {
    const record = parseWindowRecord(entry);
    if (!record) continue;
    const key = record.suffix ?? "";
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(record);
    if (out.length >= MAX_WINDOWS) break;
  }
  return out;
}

/**
 * Which live window should receive a closing detached window's tabs.
 *
 * `originId` is the discriminator, and the two cases are exclusive rather than
 * a fallback chain.
 *
 * **It is set** when this process opened the window by detaching from another
 * one, so the id is authoritative: a match is the origin, and *no* match means
 * the origin has closed. Falling through to the suffix there is exactly the bug
 * this function exists to avoid — suffixes are recycled lowest-first, so after
 * `w2` closes and a new window takes the free number, `origin: "w2"` names a
 * window these tabs never came from. A plausible-looking wrong answer is worse
 * than the generic fallback.
 *
 * **It is null** for a window restored from `windows.json`, where only the
 * suffix survived. Every record is new so no id could match, and no suffix has
 * been recycled yet either, which is what makes the persisted one usable.
 *
 * Either way the last resort is any main window, because the alternative is
 * nowhere and these tabs name live shells.
 *
 * Records here are plain `{id, suffix, kind}` — the decision has nothing to do
 * with a `BrowserWindow`, which is what makes it testable.
 */
function handBackTarget(closing, others) {
  if (closing.originId !== null && closing.originId !== undefined) {
    const byId = others.find((r) => r.id === closing.originId);
    if (byId) return byId;
  } else if (closing.origin !== null && closing.origin !== undefined) {
    const bySuffix = others.find((r) => r.suffix === closing.origin);
    if (bySuffix) return bySuffix;
  }
  return others.find((r) => r.kind === "main") ?? null;
}

/**
 * How many stored windows may be reopened.
 *
 * One short of the ceiling **only when the stored set has no main window**, in
 * which case one has to be opened afterwards — an app whose every window is a
 * bare dock has no rail and no way back. Reserving unconditionally was the first
 * version and it silently dropped the last window of a full set, taking its
 * layout and the live shells its terminal ids name with it; the next quit then
 * rewrote the set one shorter, making the loss permanent.
 */
function restoreBudget(stored) {
  return stored.some((e) => e.kind === "main") ? MAX_WINDOWS : MAX_WINDOWS - 1;
}

/**
 * Merge this base's windows into whatever the file already holds.
 *
 * Other bases are carried through untouched: a packaged app and a dev run share
 * one `userData`, and the one quitting must not delete the other's windows.
 */
function serializeWindowList(previousRaw, base, records) {
  let all = {};
  try {
    const parsed = JSON.parse(previousRaw);
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) all = parsed;
  } catch {
    // Unreadable or absent: start a fresh file rather than refusing to write.
  }
  all[base] = records.map((r) => ({
    suffix: r.suffix,
    kind: r.kind,
    origin: r.origin,
    bounds: r.bounds ?? null,
  }));
  return JSON.stringify(all);
}

module.exports = {
  MAX_WINDOWS,
  SUFFIX_RE,
  WINDOW_KINDS,
  isSuffix,
  slotFor,
  nextSuffix,
  canOpenAnother,
  handBackTarget,
  restoreBudget,
  safeBounds,
  parseWindowRecord,
  parseWindowList,
  serializeWindowList,
};
