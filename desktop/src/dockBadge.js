// The Dock icon's badge: how many things are unread, across every window.
//
// Its own module for the reason `trayVisibility.js` is: `main.js` and
// `windows.js` cannot be imported outside Electron, so the part worth testing —
// combining windows' reports and turning them into a label — is kept pure of it.
// Electron is only touched through the `apply` callback `registerDockBadgeIpc`
// builds.
//
// **The page decides what is unread; this only combines.** Every window runs
// its own inbox and reports the session ids it holds an unread event for (see
// `setBadge` in `crates/veld-daemon/ui/src/shell.ts`). A pane lives in one
// window's layout, but a main window and a detached one can both know a
// session, so the badge counts the *union* of ids rather than adding counts —
// adding would show one waiting agent as two.

/** The most session ids one report may carry. A bound, not a design limit. */
const MAX_SESSIONS = 1000;
/** A pane id is a short token; anything longer did not come from `/ide`. */
const MAX_ID_LENGTH = 200;
/** The values `desktop.dockBadge` can take. Mirrors `DOCK_BADGE_STYLES` in Rust. */
const STYLES = new Set(["count", "dot", "off"]);

/**
 * A report from a renderer, cleaned up. Anything malformed becomes "nothing
 * unread, counted" — the badge's resting state — rather than an exception in the
 * main process.
 *
 * @param {unknown} payload
 * @returns {{ sessions: string[], style: "count" | "dot" | "off" }}
 */
function parseReport(payload) {
  const raw = /** @type {any} */ (payload);
  const style = STYLES.has(raw?.style) ? raw.style : "count";
  const sessions = Array.isArray(raw?.sessions)
    ? raw.sessions
        .filter((id) => typeof id === "string" && id !== "" && id.length <= MAX_ID_LENGTH)
        .slice(0, MAX_SESSIONS)
    : [];
  return { sessions, style };
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
 * @param {"count" | "dot" | "off"} style
 * @returns {string}
 */
function badgeText(count, style) {
  if (style === "off" || count <= 0) return "";
  if (style === "dot") return "•";
  return count > 99 ? "99+" : String(count);
}

/**
 * Combine the windows' reports and push the result to `apply` when it changes.
 *
 * `style` is taken from the **newest** report. Every window reads the same
 * settings document and re-reports when it changes, so the windows disagree for
 * at most a frame, and "the one that spoke last" is the one that has seen the
 * change.
 *
 * @param {(text: string) => void} apply
 */
function createDockBadge(apply) {
  /** @type {Map<number, string[]>} */
  const reports = new Map();
  let style = /** @type {"count" | "dot" | "off"} */ ("count");
  let shown = "";

  const refresh = () => {
    const union = new Set();
    for (const sessions of reports.values()) for (const id of sessions) union.add(id);
    const text = badgeText(union.size, style);
    if (text === shown) return;
    shown = text;
    apply(text);
  };

  return {
    /** A window's current answer, replacing whatever it said before. */
    report(senderId, payload) {
      const parsed = parseReport(payload);
      reports.set(senderId, parsed.sessions);
      style = parsed.style;
      refresh();
    },
    /** A window went away — its unread events are no longer anybody's to see here. */
    forget(senderId) {
      if (reports.delete(senderId)) refresh();
    },
    /** What the badge currently says. Tests. */
    text: () => shown,
  };
}

/**
 * Wire `veld:app:set-badge` to the Dock.
 *
 * macOS only: `app.dock` is undefined elsewhere, and Linux's `setBadgeCount`
 * works on so few desktops that a setting promising it would mostly be a lie.
 * The handler is registered everywhere anyway, so a page asking on Linux gets a
 * `false` rather than an unhandled-channel error.
 *
 * @param {import("electron").IpcMain} ipcMain
 * @param {import("electron").App} app
 */
function registerDockBadgeIpc(ipcMain, app) {
  const badge = createDockBadge((text) => app.dock?.setBadge(text));
  /** Senders already being watched for `destroyed`, so a reload does not stack listeners. */
  const watched = new Set();
  ipcMain.handle("veld:app:set-badge", (event, payload) => {
    if (!app.dock) return false;
    const sender = event.sender;
    // Main frame only, like every other handler here (`senderWindow` in
    // `windows.js`): an iframe inside the page must not be able to paint the Dock.
    if (event.senderFrame !== sender.mainFrame) return false;
    const id = sender.id;
    if (!watched.has(id)) {
      watched.add(id);
      sender.once("destroyed", () => {
        watched.delete(id);
        badge.forget(id);
      });
    }
    badge.report(id, payload);
    return true;
  });
}

module.exports = { badgeText, createDockBadge, parseReport, registerDockBadgeIpc };
