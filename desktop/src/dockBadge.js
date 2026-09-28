// The Dock icon's badge: how many things are unread, across every window.
//
// Its own module for the reason `trayVisibility.js` is: `main.js` and
// `windows.js` cannot be imported outside Electron, so the part worth testing —
// combining windows' reports and turning them into a label — is kept pure of it.
// Electron is only touched through the `apply` callback `registerDockBadgeIpc`
// builds.
//
// **The page decides what is unread; this only combines.** Every window runs
// its own inbox and reports its unread sessions, each with its worktree, plus
// the worktrees it *holds* (see `setBadge` in
// `crates/veld-daemon/ui/src/shell.ts`). Two rules make the combination honest:
//
// - **Ids, not counts.** A main window and a detached one can both know a
//   session, and adding their counts would show one waiting agent as two. The
//   badge counts distinct session ids.
// - **The holder's word wins for a held worktree.** The daemon relays an agent
//   hook to *every* main window, so each one files it — but reading is
//   per-window, and only the window holding that worktree can read it (the
//   others are refused the claim). Taking a plain union therefore kept a session
//   counted forever in a window that could never clear it. So for a worktree
//   some window holds, only that window's answer counts; for a worktree nobody
//   holds — the agent in a project you have not opened this run — anyone's does,
//   since it is news nobody has had the chance to read.
//
// **The style is not the page's to say.** A window re-reads settings only on
// focus, so an unfocused one holds a stale copy; taking its word would bring a
// number back after the user chose `off`. The main process reads
// `desktop.dockBadge` from the daemon on the tray's tick and on every settings
// nudge, as it already does for `desktop.menuBarIcon` — see `main.js`.

/** The most sessions (or held worktrees) one report may carry. A bound, not a design limit. */
const MAX_ENTRIES = 1000;
/** A pane id is a short token; anything longer did not come from `/ide`. */
const MAX_ID_LENGTH = 200;
/** The values `desktop.dockBadge` can take. Mirrors `DOCK_BADGE_STYLES` in Rust —
 *  `dockBadge.test.js` compares the two, and `badgeText` has a case for each. */
const STYLES = new Set(["count", "dot", "off"]);
const DEFAULT_STYLE = "count";
/**
 * How long a reloaded page's previous report is kept while the new page settles.
 * A page's first report can say "I hold nothing" only because its layouts are
 * still being fetched, and dropping the old report at once would count other
 * windows' stale copies for that moment. Long enough for a reload, short enough
 * that a window which navigated to the waiting screen stops counting soon.
 */
const RELOAD_GRACE_MS = 10_000;

const isWorktreeId = (v) => Number.isSafeInteger(v) && v >= 0;

/**
 * A report from a renderer, cleaned up. Anything malformed becomes "nothing
 * unread, nothing held" — the badge's resting state — rather than an exception in
 * the main process.
 *
 * @param {unknown} payload
 * @returns {{ unread: Map<string, number>, held: Set<number>, own: boolean }}
 */
function parseReport(payload) {
  const raw = /** @type {any} */ (payload);
  const unread = new Map();
  if (Array.isArray(raw?.unread)) {
    for (const entry of raw.unread.slice(0, MAX_ENTRIES)) {
      const id = entry?.sessionId;
      const worktreeId = entry?.worktreeId;
      if (typeof id !== "string" || id === "" || id.length > MAX_ID_LENGTH) continue;
      if (!isWorktreeId(worktreeId)) continue;
      unread.set(id, worktreeId);
    }
  }
  const held = new Set(
    Array.isArray(raw?.held) ? raw.held.slice(0, MAX_ENTRIES).filter(isWorktreeId) : [],
  );
  return { unread, held, own: raw?.own === true };
}

/**
 * `desktop.dockBadge` out of a `GET /api/settings` body. `fallback` for every
 * shape that is not a known style — a daemon that is down or older than the key
 * must not change what the user already sees.
 *
 * @param {unknown} body
 * @param {string} fallback
 */
function dockBadgeStyleFrom(body, fallback) {
  const value = /** @type {any} */ (body)?.settings?.["desktop.dockBadge"];
  return STYLES.has(value) ? value : fallback;
}

/**
 * What the Dock should say for `count` unread things in `style`.
 *
 * `""` clears the badge — that is what `app.dock.setBadge` takes for "none".
 * Past 99 the number stops being information and starts being width: macOS
 * draws the whole string, and a four-digit pill over a 128px icon says nothing
 * a two-digit one does not.
 *
 * @param {number} count
 * @param {string} style
 * @returns {string}
 */
function badgeText(count, style) {
  if (count <= 0) return "";
  switch (style) {
    case "off":
      return "";
    case "dot":
      return "•";
    case "count":
      return count > 99 ? "99+" : String(count);
    default:
      throw new Error(`badgeText: no rendering for style ${JSON.stringify(style)}`);
  }
}

/**
 * Combine the windows' reports and push the result to `apply` when it changes.
 *
 * @param {(text: string) => void} apply
 */
function createDockBadge(apply) {
  /** @type {Map<number, { unread: Map<string, number>, held: Set<number>, own: boolean }>} */
  const reports = new Map();
  let style = DEFAULT_STYLE;
  /** Nothing is shown until the style has been read once: a user who chose `off`
   *  must not see a number flash at launch while the first read is in flight. */
  let styleKnown = false;
  let shown = "";

  const count = () => {
    const heldAnywhere = new Set();
    for (const r of reports.values()) for (const w of r.held) heldAnywhere.add(w);
    const counted = new Set();
    for (const r of reports.values()) {
      for (const [id, worktreeId] of r.unread) {
        // `own`: a detached window. It holds no worktree, but everything in its
        // inbox is its own panes' terminal events (it files no relayed hooks), which
        // no other window can see — so nobody else's word can stand in for it.
        if (r.own || r.held.has(worktreeId) || !heldAnywhere.has(worktreeId)) counted.add(id);
      }
    }
    return counted.size;
  };

  const refresh = () => {
    const text = styleKnown ? badgeText(count(), style) : "";
    if (text === shown) return;
    shown = text;
    apply(text);
  };

  return {
    /** A window's current answer, replacing whatever it said before. */
    report(senderId, payload) {
      reports.set(senderId, parseReport(payload));
      refresh();
    },
    /** A window went away, or its page did — its report no longer describes anything. */
    forget(senderId) {
      if (reports.delete(senderId)) refresh();
    },
    /** `desktop.dockBadge`, as the main process last read it from the daemon. */
    setStyle(next) {
      style = STYLES.has(next) ? next : DEFAULT_STYLE;
      styleKnown = true;
      refresh();
    },
    style: () => style,
    /** What the badge currently says. Tests. */
    text: () => shown,
  };
}

/**
 * Wire `veld:app:set-badge` to the Dock, and return the badge so `main.js` can
 * feed it the style it reads.
 *
 * macOS only: `app.dock` is undefined elsewhere, and Linux's `setBadgeCount`
 * works on so few desktops that a setting promising it would mostly be a lie.
 * The handler is registered everywhere anyway, so a page asking on Linux gets a
 * `false` rather than an unhandled-channel error.
 *
 * @param {Pick<import("electron").IpcMain, "handle">} ipcMain
 * @param {{ dock?: { setBadge(text: string): void } }} app
 */
function registerDockBadgeIpc(ipcMain, app) {
  const badge = createDockBadge((text) => app.dock?.setBadge(text));
  /** Senders already being watched, so a reload does not stack listeners. */
  const watched = new Set();
  /** Per sender: the pending forget after a navigation, cancelled by a report. */
  const graces = new Map();
  const cancelGrace = (id) => {
    clearTimeout(graces.get(id));
    graces.delete(id);
  };
  ipcMain.handle("veld:app:set-badge", (event, payload) => {
    if (!app.dock) return false;
    const sender = event.sender;
    // Main frame only, like every other handler here (`senderWindow` in
    // `windows.js`): an iframe inside the page must not be able to paint the Dock.
    if (event.senderFrame !== sender.mainFrame) return false;
    const id = sender.id;
    if (!watched.has(id)) {
      watched.add(id);
      const drop = () => {
        cancelGrace(id);
        badge.forget(id);
      };
      // A crashed renderer keeps its `webContents` (see `browserViews.js`), so its
      // report goes at once.
      sender.on("render-process-gone", drop);
      // A committed page change — a reload, or the waiting screen — keeps the
      // old report for `RELOAD_GRACE_MS` and then drops it unless the new page
      // has reported. `did-navigate` rather than `did-start-navigation`: the
      // latter also fires for an external link that `will-navigate` then cancels
      // (`windows.js`), and the page, still alive, would not re-report.
      // Same-document navigations have their own event and never reach here.
      sender.on("did-navigate", () => {
        cancelGrace(id);
        graces.set(id, setTimeout(drop, RELOAD_GRACE_MS));
      });
      sender.once("destroyed", () => {
        watched.delete(id);
        drop();
      });
    }
    cancelGrace(id);
    badge.report(id, payload);
    return true;
  });
  return badge;
}

module.exports = {
  RELOAD_GRACE_MS,
  STYLES,
  badgeText,
  createDockBadge,
  dockBadgeStyleFrom,
  parseReport,
  registerDockBadgeIpc,
};
