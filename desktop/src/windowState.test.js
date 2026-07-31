// Tests for the window registry's arithmetic and parsing. No Electron binary,
// which is the reason this logic lives apart from `windows.js`.
const test = require("node:test");
const assert = require("node:assert/strict");
const {
  MAX_WINDOWS,
  canOpenAnother,
  isSuffix,
  nextSuffix,
  parseWindowList,
  parseWindowRecord,
  safeBounds,
  serializeWindowList,
  slotFor,
} = require("./windowState");

test("the first window keeps the bare base slot", () => {
  // Load-bearing across an upgrade: before slots were per-window the app wrote
  // `veld.panes.slot.main.v1`, and a first window that suddenly wanted
  // `main-w1` would come back from the update with no layout and no terminals.
  assert.equal(slotFor("main", null), "main");
  assert.equal(slotFor("main", "w2"), "main-w2");
  assert.equal(slotFor("dev-4211", "w3"), "dev-4211-w3");
});

test("isSuffix takes w2..w99 and nothing else", () => {
  assert.ok(isSuffix("w2"));
  assert.ok(isSuffix("w99"));
  // `w1` is not a suffix: window one is the bare base, and accepting both
  // spellings would let two windows sit on two keys for one intended slot.
  assert.equal(isSuffix("w1"), false);
  assert.equal(isSuffix("w0"), false);
  assert.equal(isSuffix("w100"), false);
  for (const none of ["", "w", "main", "w2 ", "../w2", 2, null, undefined]) {
    assert.equal(isSuffix(none), false, JSON.stringify(none));
  }
});

test("nextSuffix reuses the lowest free number", () => {
  assert.equal(nextSuffix(new Set([null])), "w2");
  assert.equal(nextSuffix(new Set([null, "w2"])), "w3");
  // Lowest, not highest-plus-one: a suffix ends up in a durable storage key, so
  // marching upward forever leaves an unbounded set of abandoned layouts behind.
  assert.equal(nextSuffix(new Set([null, "w2", "w4"])), "w3");
  const full = new Set([null]);
  for (let n = 2; n <= 99; n++) full.add(`w${n}`);
  assert.equal(nextSuffix(full), null);
});

test("canOpenAnother stops at the ceiling", () => {
  assert.ok(canOpenAnother(0));
  assert.ok(canOpenAnother(MAX_WINDOWS - 1));
  assert.equal(canOpenAnother(MAX_WINDOWS), false);
  assert.equal(canOpenAnother(MAX_WINDOWS + 5), false);
});

test("safeBounds drops a rect whole rather than repairing half of it", () => {
  assert.deepEqual(safeBounds({ x: 10, y: 20, width: 900, height: 700 }), {
    x: 10,
    y: 20,
    width: 900,
    height: 700,
  });
  // Negative position is legal — a display to the left of the primary one.
  assert.deepEqual(safeBounds({ x: -1400, y: 0, width: 800, height: 600 }), {
    x: -1400,
    y: 0,
    width: 800,
    height: 600,
  });
  assert.deepEqual(safeBounds({ x: 0.4, y: 0.6, width: 800.2, height: 600.8 }), {
    x: 0,
    y: 1,
    width: 800,
    height: 601,
  });
  // A stored 1×1 would restore a window nobody can find or grab.
  assert.equal(safeBounds({ x: 0, y: 0, width: 1, height: 600 }), null);
  assert.equal(safeBounds({ x: 0, y: 0, width: 900, height: 99999 }), null);
  assert.equal(safeBounds({ x: 0, y: 0, width: 900 }), null);
  for (const none of [null, undefined, 42, "big", { x: "a", y: 0, width: 900, height: 700 }]) {
    assert.equal(safeBounds(none), null, JSON.stringify(none));
  }
});

test("parseWindowRecord degrades every field it cannot use", () => {
  assert.deepEqual(parseWindowRecord({ suffix: "w2", kind: "detached", origin: null }), {
    suffix: "w2",
    kind: "detached",
    origin: null,
    bounds: null,
  });
  // An unknown kind from a newer build becomes a main window rather than
  // nothing: a window that fails to reopen is a layout, and its shells, lost.
  assert.deepEqual(parseWindowRecord({ kind: "floating", suffix: "nope" }), {
    suffix: null,
    kind: "main",
    origin: null,
    bounds: null,
  });
  assert.equal(parseWindowRecord(null), null);
  assert.equal(parseWindowRecord("main"), null);
});

test("parseWindowList refuses two windows on one slot", () => {
  const raw = JSON.stringify({
    main: [
      { suffix: null, kind: "main" },
      { suffix: "w2", kind: "detached", origin: null },
      // Two windows restoring one slot would restore one layout twice and fight
      // over every terminal in it — the exact collision slots exist to prevent.
      { suffix: "w2", kind: "main" },
    ],
    dev: [{ suffix: null, kind: "main" }],
  });
  assert.deepEqual(
    parseWindowList(raw, "main").map((r) => [r.suffix, r.kind]),
    [
      [null, "main"],
      ["w2", "detached"],
    ],
  );
  // Bases are separate namespaces: a dev run must not reopen the packaged app's
  // windows.
  assert.equal(parseWindowList(raw, "dev").length, 1);
  assert.deepEqual(parseWindowList(raw, "main-991"), []);
});

test("parseWindowList never throws on a file it cannot read", () => {
  for (const junk of ["", "{", "null", "[]", '{"main":42}', '{"main":[null,7,"x"]}']) {
    assert.deepEqual(parseWindowList(junk, "main"), [], JSON.stringify(junk));
  }
});

test("parseWindowList stops at the window ceiling", () => {
  const many = Array.from({ length: MAX_WINDOWS + 5 }, (_, i) => ({
    suffix: i === 0 ? null : `w${i + 1}`,
    kind: "main",
  }));
  assert.equal(parseWindowList(JSON.stringify({ main: many }), "main").length, MAX_WINDOWS);
});

test("serializeWindowList leaves the other base alone", () => {
  // A packaged app and a dev run share one userData. The one quitting must not
  // delete the other's windows.
  const previous = JSON.stringify({ dev: [{ suffix: null, kind: "main", origin: null }] });
  const written = JSON.parse(
    serializeWindowList(previous, "main", [
      { suffix: null, kind: "main", origin: null, bounds: null },
      { suffix: "w2", kind: "detached", origin: null, bounds: { x: 1, y: 2, width: 3, height: 4 } },
    ]),
  );
  assert.equal(written.dev.length, 1);
  assert.equal(written.main.length, 2);
  assert.deepEqual(written.main[1].bounds, { x: 1, y: 2, width: 3, height: 4 });

  // An unreadable previous file starts a fresh one rather than refusing to write.
  const fresh = JSON.parse(serializeWindowList("{oops", "main", []));
  assert.deepEqual(fresh, { main: [] });
});

test("a serialized list round-trips through the parser", () => {
  const records = [
    { suffix: null, kind: "main", origin: null, bounds: { x: 0, y: 0, width: 1280, height: 800 } },
    { suffix: "w2", kind: "detached", origin: null, bounds: null },
  ];
  const parsed = parseWindowList(serializeWindowList("", "main", records), "main");
  assert.deepEqual(parsed, records);
});
