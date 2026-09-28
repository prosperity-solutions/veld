const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const { badgeText, createDockBadge, parseReport } = require("./dockBadge");

test("badgeText: a number, a dot, or nothing", () => {
  assert.equal(badgeText(0, "count"), "");
  assert.equal(badgeText(3, "count"), "3");
  assert.equal(badgeText(99, "count"), "99");
  assert.equal(badgeText(100, "count"), "99+");
  assert.equal(badgeText(3, "dot"), "•");
  assert.equal(badgeText(0, "dot"), "");
  assert.equal(badgeText(3, "off"), "");
});

test("parseReport: anything malformed is nothing unread, counted", () => {
  for (const payload of [undefined, null, 3, "x", {}, { sessions: "a" }]) {
    assert.deepEqual(parseReport(payload), { sessions: [], style: "count" });
  }
  assert.deepEqual(parseReport({ sessions: ["a", 1, "", null, "b"], style: "loud" }), {
    sessions: ["a", "b"],
    style: "count",
  });
  assert.equal(parseReport({ sessions: ["x".repeat(201)] }).sessions.length, 0);
});

/**
 * The reason the page sends ids rather than a count: two windows that both know
 * one waiting session must show 1, not 2.
 */
test("windows are unioned, not added", () => {
  const applied = [];
  const badge = createDockBadge((t) => applied.push(t));
  badge.report(1, { sessions: ["a", "b"], style: "count" });
  badge.report(2, { sessions: ["b", "c"], style: "count" });
  assert.equal(badge.text(), "3");
  badge.forget(1);
  assert.equal(badge.text(), "2");
  badge.report(2, { sessions: [], style: "count" });
  assert.equal(badge.text(), "");
  assert.deepEqual(applied, ["2", "3", "2", ""]);
});

test("the newest report's style wins, and an unchanged answer is not re-applied", () => {
  const applied = [];
  const badge = createDockBadge((t) => applied.push(t));
  badge.report(1, { sessions: ["a"], style: "count" });
  badge.report(1, { sessions: ["a"], style: "count" });
  badge.report(2, { sessions: [], style: "dot" });
  badge.report(2, { sessions: [], style: "off" });
  assert.deepEqual(applied, ["1", "•", ""]);
});

/** The style names live in Rust too, and no compiler sees across the two. */
test("the styles match the Rust allow-list", () => {
  const rust = fs.readFileSync(
    path.join(__dirname, "../../crates/veld-core/src/db/settings_catalog.rs"),
    "utf8",
  );
  const block = rust.match(/DOCK_BADGE_STYLES: &\[Choice\] = &\[([\s\S]*?)\];/);
  assert.ok(block, "DOCK_BADGE_STYLES not found in settings_catalog.rs");
  const values = [...block[1].matchAll(/choice\("([^"]+)"/g)].map((m) => m[1]).sort();
  assert.deepEqual(values, ["count", "dot", "off"]);
  for (const v of values) assert.equal(parseReport({ style: v }).style, v);
});
