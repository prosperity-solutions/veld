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
 * The vertical position of a macOS traffic light, centred against the top bar.
 *
 * The OS draws the traffic lights at a fixed size while the top bar is CSS and
 * scales with the page's zoom factor, so a light centred on the *unzoomed* bar
 * drifts off the bar's own controls as the page zooms. `zoom` scales the bar's
 * height: a `topbarHeight`-pixel CSS bar renders `topbarHeight × zoom`
 * device-independent pixels tall, and the light stays centred on that.
 *
 * `topbarHeight` and `trafficLightSize` mirror `TOPBAR_HEIGHT` and
 * `TRAFFIC_LIGHT_SIZE` in `desktop/src/main.js`, and at `zoom = 1` the answer
 * is the pure centred value — deliberately no fudge constant, since the
 * centred 100% position is the correct one (an earlier `- 2` nudge up only
 * compensated for testing at 90% zoom).
 *
 * This is the one piece of the feature that is arithmetic over plain values,
 * so it lives here rather than in `windows.js` and has tests.
 */
function trafficLightY(topbarHeight, trafficLightSize, zoom) {
  return Math.round((topbarHeight * zoom - trafficLightSize) / 2);
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
  // …and which worktree it is a dock *for*. A detached window has no rail, so
  // it cannot resolve a selection the way a main window does; without this it
  // reopened against whatever the main window last selected, rendered blank
  // (its real tabs sitting unread in its slot's layout), and handed back
  // nothing when closed. A main window needs none of this — it has the rail,
  // and its own slot-scoped selection key.
  const worktreeId =
    Number.isSafeInteger(value.worktreeId) && value.worktreeId > 0 ? value.worktreeId : null;
  const repoRoot =
    typeof value.repoRoot === "string" && value.repoRoot !== "" && value.repoRoot.length <= 4096
      ? value.repoRoot
      : null;
  return { suffix, kind, origin, worktreeId, repoRoot, bounds: safeBounds(value.bounds) };
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
 * Everything a closing window has to hand on, queue first.
 *
 * Two sources and they are not the same thing. `carried` is what was handed *to*
 * this window and never collected — a drop routed here while it was loading, whose
 * source has already let go on the strength of the shell taking custody — and it
 * travels on whatever kind of window this is: **a queue is a resting place, never a
 * grave.** `own` is the window's own tabs, which only a detached window hands back,
 * because a main window's tabs are its own and closing it is not a transfer.
 *
 * Queue first so the order tabs arrive in is the order they were sent, and an
 * `own` with no tabs is dropped rather than travelling as an empty transfer.
 */
function handBackTransfers(kind, carried, own) {
  const mine = kind === "detached" && own && own.tabs.length > 0 ? [own] : [];
  return [...carried, ...mine];
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

// ---------------------------------------------------------------------------
// Which window is displaying what
// ---------------------------------------------------------------------------
//
// One map: worktree → the window showing it, reported by the renderer once the
// *daemon* has granted it (`veld:window:shows`). **Not ownership** — who may
// show a worktree, who has to let go, and what a click on a taken one does are
// the daemon's, because `/ide` also runs in a browser tab this process cannot
// see. What is left here is routing a cross-window tab drop at a window those
// tabs belong in, which is a question about this process's own windows.
//
// Pure bookkeeping, kept here rather than in `windows.js`, because the failure
// is set arithmetic — an entry that outlives its window — and `node --test` can
// reach it here.

/**
 * Whether `record` is a window this worktree's panes belong in.
 *
 * The one question about worktrees this process still answers, and only because
 * it is about *its own windows*: when tabs are dropped onto a window, do they
 * belong there? Who may show a worktree moved to the daemon, which is the only
 * party a browser tab also talks to.
 *
 * Two ways to qualify, and the second is not a special case. A **main** window
 * qualifies by displaying the worktree — its own `worktreeId` records what it
 * was opened for, which is not what it shows now, so that field must never be
 * read for one. A **detached** window never reports what it displays (it is a
 * satellite of its origin), so the field is the only thing that says which
 * worktree its dock is for — and matching on the map alone made a detached dock
 * impossible to drop onto.
 *
 * Liveness and "is this the window the drag started in" are the caller's, since
 * neither is set arithmetic.
 */
function ownsWorktree(record, worktreeId, showing) {
  if (showing.get(worktreeId) === record.id) return true;
  return record.kind === "detached" && record.worktreeId === worktreeId;
}

/**
 * Whether a cross-window drop may be pushed at a window's drop listener, or has
 * to be queued for it.
 *
 * A window reports what it displays only once the daemon has granted it *and*
 * React has committed — which is after `PaneArea` registers its drop handler, so
 * for a current bundle the two now arrive in the safe order. The gap that
 * remains is a page that is **loading or reloading**: it is still in the map
 * from before, its handler is gone, and pushing there means `webContents.send`
 * goes nowhere, the ack times out, and the gesture reports `refused` after two
 * seconds of
 * looking like a hang.
 *
 * **This is only half the question, and the smaller half.** A page that has not
 * finished *loading* has no listener either, and that is the longer gap — the
 * waiting page through a daemon restart, the bundle load, a reload. The caller
 * asks the window itself about that (`webContents.isLoading()`), because it is the
 * shell's own knowledge and does not depend on the UI's version. What is left for
 * this function is a page that has loaded and still has no handler.
 *
 * `"unknown"` therefore means *loaded, and has not reported* — either an older
 * `/ide` bundle that never will (version skew makes that reachable) or a current
 * one in the window between its load and `PaneArea` mounting, which is `/api/repos`
 * resolving. It **sends**, because for the older bundle send-and-answer is the only
 * thing that works at all, and for the newer one the drop ack's own timeout is the
 * safety net — a queue, not a refusal. Only a window that has reported, and has
 * since said its listener is gone, is queued for outright.
 */
function dropDelivery(dropListener) {
  return dropListener === "gone" ? "queue" : "send";
}

/**
 * What one of a window's listener states becomes when its page navigates.
 *
 * One listener now (the drop listener); it was two, and the yield listener went
 * with the claim protocol to the daemon. Kept general because the question is
 * about a page's listeners, not about which one.
 *
 * Only a main-frame, cross-document navigation counts. `did-start-loading` is the
 * tab spinner and an iframe's load turns it too; a same-document navigation
 * (`pushState`, a fragment) does not replace a listener at all. Demoting on either
 * would strand the window in `"gone"` with nothing able to undo it, since the
 * renderer reports `ready` when its effect mounts and it is already mounted.
 *
 * `"unknown"` is left alone rather than demoted: it means "has never reported",
 * which a reload does not change, and turning it into `"gone"` would hold an older
 * bundle in the degraded path for the rest of the session.
 */
function nextListenerState(current, { isMainFrame, isSameDocument }) {
  if (!isMainFrame || isSameDocument) return current;
  return current === "ready" ? "gone" : current;
}

/** Drop every entry pointing at `recordId`. A window displays one worktree at a
 *  time, so reporting a new one clears the old through here too. */
function releaseClaims(claims, recordId) {
  for (const [worktreeId, id] of [...claims]) {
    if (id === recordId) claims.delete(worktreeId);
  }
}

/**
 * Merge this base's windows into whatever the file already holds.
 *
 * Other bases are carried through untouched: a packaged app and a dev run share
 * one `userData`, and the one quitting must not delete the other's windows.
 */
/**
 * Whether a pid is still running. Signal 0 is the existence check only — the
 * same question `claimSlot` asks of the same pids. Injectable so the pruning
 * below is testable without spawning processes.
 *
 * It answers "is *a* process alive", not "is *that instance* alive": a pid
 * recycled after a reboot reads as live and keeps its base one launch longer,
 * and `EPERM` (alive, another owner) reads as dead. Both are harmless for a
 * per-user `userData`, and the pruning only has to converge, not be exact.
 */
function livePid(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function serializeWindowList(previousRaw, base, records, isPidAlive = livePid, lastMainBounds = null) {
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
    worktreeId: r.worktreeId ?? null,
    repoRoot: r.repoRoot ?? null,
    bounds: r.bounds ?? null,
  }));
  // Drop the bases nobody will ever read again. `claimSlot` mints a
  // `main-<pid>` / `dev-<pid>` base whenever a second instance finds the
  // preferred one held, and once that run ends the key is unreachable — so
  // without pruning, every such collision leaves one behind and the file grows
  // monotonically across launches.
  //
  // **Liveness, not just the shape of the name.** Pruning every pid-derived base
  // but our own deletes the windows of a second instance that is *still
  // running*: two dev instances are a normal thing to want, the first owns
  // `dev` and the second `dev-<pid>`, and the first one's next persist would
  // wipe the second's set out from under it. `process.kill(pid, 0)` is the same
  // question `claimSlot` asks of the same pids, and the OS answers it exactly.
  for (const [key, list] of Object.entries(all)) {
    if (Array.isArray(list) && list.length === 0) {
      delete all[key];
      continue;
    }
    if (key === base) continue;
    const pid = key.match(/^(?:main|dev)-(\d+)$/);
    if (pid && !isPidAlive(Number(pid[1]))) delete all[key];
  }
  // The last main window's bounds, kept apart from the window set: closing the
  // last window on macOS empties the set, and without this the next fresh main
  // window would open at the default size. It is not a base — it is never
  // reopened, only recalled as the size/position a new main window starts at.
  all.lastMainBounds = lastMainBounds ? safeBounds(lastMainBounds) : null;
  return JSON.stringify(all);
}

/**
 * The bounds a fresh main window should start at, or `null` when nothing has
 * ever been remembered (a first launch, or an unreadable file).
 *
 * `safeBounds` guards the same way it does for a window record: a corrupted
 * value must not render the app unresizable-in-practice.
 */
function readLastMainBounds(raw) {
  try {
    const parsed = JSON.parse(raw);
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
      return safeBounds(parsed.lastMainBounds);
    }
  } catch {
    // Unreadable or absent: nothing remembered, which is the first-launch case.
  }
  return null;
}

module.exports = {
  MAX_WINDOWS,
  SUFFIX_RE,
  WINDOW_KINDS,
  isSuffix,
  slotFor,
  nextSuffix,
  canOpenAnother,
  dropDelivery,
  handBackTarget,
  handBackTransfers,
  nextListenerState,
  ownsWorktree,
  releaseClaims,
  restoreBudget,
  safeBounds,
  trafficLightY,
  parseWindowRecord,
  parseWindowList,
  readLastMainBounds,
  serializeWindowList,
};
