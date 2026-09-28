const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const {
  RELOAD_GRACE_MS,
  STYLES,
  badgeText,
  createDockBadge,
  dockBadgeStyleFrom,
  parseReport,
  registerDockBadgeIpc,
} = require("./dockBadge");

const u = (sessionId, worktreeId) => ({ sessionId, worktreeId });

test("badgeText: a number, a dot, or nothing", () => {
  assert.equal(badgeText(0, "count"), "");
  assert.equal(badgeText(3, "count"), "3");
  assert.equal(badgeText(99, "count"), "99");
  assert.equal(badgeText(100, "count"), "99+");
  assert.equal(badgeText(3, "dot"), "•");
  assert.equal(badgeText(0, "dot"), "");
  assert.equal(badgeText(3, "off"), "");
  // Every style has a rendering — a new one cannot silently fall through to a number.
  for (const style of STYLES) badgeText(1, style);
  assert.throws(() => badgeText(1, "ring"));
});

test("parseReport: anything malformed is nothing unread, nothing held", () => {
  for (const payload of [undefined, null, 3, "x", {}, { unread: "a", held: "b" }]) {
    const r = parseReport(payload);
    assert.equal(r.unread.size, 0);
    assert.equal(r.held.size, 0);
  }
  const r = parseReport({
    unread: [u("a", 1), u(1, 1), u("", 1), null, u("b", -1), u("c", 2), u("x".repeat(201), 1)],
    held: [1, "2", -3, 1.5, 4],
  });
  assert.deepEqual([...r.unread], [["a", 1], ["c", 2]]);
  assert.deepEqual([...r.held], [1, 4]);
});

test("dockBadgeStyleFrom only moves off the fallback for a style it knows", () => {
  assert.equal(dockBadgeStyleFrom({ settings: { "desktop.dockBadge": "dot" } }, "count"), "dot");
  for (const body of [null, {}, { settings: {} }, { settings: { "desktop.dockBadge": "loud" } }]) {
    assert.equal(dockBadgeStyleFrom(body, "off"), "off");
  }
});

/** Two windows that both know one session must show 1, not 2. */
test("the same session in two windows is counted once", () => {
  const badge = createDockBadge(() => {});
  badge.setStyle("count");
  badge.report(1, { unread: [u("a", 7), u("b", 7)], held: [7] });
  badge.report(2, { unread: [u("b", 7)], held: [7] });
  assert.equal(badge.text(), "2");
});

/**
 * The reason `held` exists. Every main window files every relayed agent hook, but
 * only the window holding the worktree can read it — so a copy in another window
 * must not keep the badge lit after the holder read it.
 */
test("for a held worktree only the holder's answer counts", () => {
  const badge = createDockBadge(() => {});
  badge.setStyle("count");
  badge.report(1, { unread: [u("a", 7)], held: [7] });
  badge.report(2, { unread: [u("a", 7)], held: [] });
  assert.equal(badge.text(), "1");
  // Read in the holder: window 2's stale copy does not bring it back.
  badge.report(1, { unread: [], held: [7] });
  assert.equal(badge.text(), "");
});

/** An agent in a project nobody has open this run is news nobody could have read. */
test("for a worktree nobody holds, any window's answer counts", () => {
  const badge = createDockBadge(() => {});
  badge.setStyle("count");
  badge.report(1, { unread: [], held: [7] });
  badge.report(2, { unread: [u("z", 9)], held: [] });
  assert.equal(badge.text(), "1");
  // Somebody opens it: now the holder decides, and it has no copy.
  badge.report(1, { unread: [], held: [7, 9] });
  assert.equal(badge.text(), "");
});

test("a window that goes away takes its report with it", () => {
  const applied = [];
  const badge = createDockBadge((t) => applied.push(t));
  badge.setStyle("count");
  badge.report(1, { unread: [u("a", 1)], held: [1] });
  badge.report(2, { unread: [u("b", 2)], held: [2] });
  badge.forget(1);
  assert.equal(badge.text(), "1");
  assert.deepEqual(applied, ["1", "2", "1"]);
});

test("the style is the main process's, and an unchanged answer is not re-applied", () => {
  const applied = [];
  const badge = createDockBadge((t) => applied.push(t));
  badge.report(1, { unread: [u("a", 1)], held: [1] });
  assert.equal(badge.text(), "", "nothing is shown before the style has been read");
  badge.setStyle("count");
  badge.report(1, { unread: [u("a", 1)], held: [1] });
  badge.setStyle("dot");
  badge.setStyle("off");
  badge.setStyle("nonsense");
  assert.equal(badge.style(), "count");
  assert.deepEqual(applied, ["1", "•", "", "1"]);
});

/** A fake of the three Electron surfaces the handler touches. */
function fakeElectron({ dock = true } = {}) {
  const handlers = new Map();
  const badges = [];
  const ipcMain = { handle: (channel, fn) => handlers.set(channel, fn) };
  const app = dock ? { dock: { setBadge: (t) => badges.push(t) } } : {};
  const sender = (id) => {
    const listeners = new Map();
    const mainFrame = {};
    return {
      id,
      mainFrame,
      on: (name, fn) => listeners.set(name, [...(listeners.get(name) ?? []), fn]),
      once: (name, fn) => listeners.set(name, [...(listeners.get(name) ?? []), fn]),
      emit: (name, ...args) => {
        for (const fn of listeners.get(name) ?? []) fn(...args);
      },
      count: (name) => (listeners.get(name) ?? []).length,
    };
  };
  const call = (s, payload, frame = s.mainFrame) =>
    handlers.get("veld:app:set-badge")({ sender: s, senderFrame: frame }, payload);
  return { ipcMain, app, badges, sender, call };
}

test("the handler: main frame only, one set of listeners, forgets on crash, navigation and destroy", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const f = fakeElectron();
  registerDockBadgeIpc(f.ipcMain, f.app).setStyle("count");
  const s = f.sender(1);
  const report = { unread: [u("a", 1)], held: [1] };

  assert.equal(f.call(s, report, {}), false, "an iframe is refused");
  assert.deepEqual(f.badges, []);

  assert.equal(f.call(s, report), true);
  f.call(s, report);
  assert.equal(s.count("destroyed"), 1, "a second report must not stack listeners");
  assert.deepEqual(f.badges, ["1"]);

  // A reload: the old report survives until the new page reports…
  s.emit("did-navigate");
  t.mock.timers.tick(RELOAD_GRACE_MS - 1);
  f.call(s, report);
  t.mock.timers.tick(RELOAD_GRACE_MS);
  assert.deepEqual(f.badges, ["1"]);
  // …and goes after the grace when it never does.
  s.emit("did-navigate");
  t.mock.timers.tick(RELOAD_GRACE_MS);
  assert.deepEqual(f.badges, ["1", ""]);

  f.call(s, report);
  s.emit("render-process-gone");
  f.call(s, report);
  s.emit("destroyed");
  assert.deepEqual(f.badges, ["1", "", "1", "", "1", ""]);
});

test("the handler answers false where there is no Dock", () => {
  const f = fakeElectron({ dock: false });
  registerDockBadgeIpc(f.ipcMain, f.app);
  assert.equal(f.call(f.sender(1), { unread: [u("a", 1)], held: [1] }), false);
});

/** The style names live in Rust too, and no compiler sees across the two. */
test("the styles match the Rust allow-list and default", () => {
  const read = (p) => fs.readFileSync(path.join(__dirname, "../../crates/veld-core/src/db", p), "utf8");
  const block = read("settings_catalog.rs").match(/DOCK_BADGE_STYLES: &\[Choice\] = &\[([\s\S]*?)\];/);
  assert.ok(block, "DOCK_BADGE_STYLES not found in settings_catalog.rs");
  const values = [...block[1].matchAll(/choice\("([^"]+)"/g)].map((m) => m[1]).sort();
  assert.deepEqual(values, [...STYLES].sort());
  assert.match(read("settings.rs"), /SettingKey::DesktopDockBadge,\s*Value::from\("count"\)/);
  assert.equal(createDockBadge(() => {}).style(), "count");
});
