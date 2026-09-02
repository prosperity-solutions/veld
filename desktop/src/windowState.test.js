// Tests for the window registry's arithmetic and parsing. No Electron binary,
// which is the reason this logic lives apart from `windows.js`.
const test = require("node:test");
const assert = require("node:assert/strict");
const {
  MAX_WINDOWS,
  canOpenAnother,
  cycleOrder,
  dropDelivery,
  handBackTarget,
  handBackTransfers,
  isSuffix,
  nextInCycle,
  nextListenerState,
  nextSuffix,
  ownsWorktree,
  parseWindowList,
  parseWindowRecord,
  readLastMainBounds,
  releaseClaims,
  zoomFactor,
  cssBoxToDip,
  emulationScale,
  restoreBudget,
  safeBounds,
  serializeWindowList,
  slotFor,
  trafficLightY,
} = require("./windowState");

test("the first window keeps the bare base slot", () => {
  // Load-bearing across an upgrade: before slots were per-window the app wrote
  // `veld.panes.slot.main.v1`, and a first window that suddenly wanted
  // `main-w1` would come back from the update with no layout and no terminals.
  assert.equal(slotFor("main", null), "main");
  assert.equal(slotFor("main", "w2"), "main-w2");
  assert.equal(slotFor("dev-4211", "w3"), "dev-4211-w3");
});

test("trafficLightY centres the light on the zoomed bar", () => {
  const bar = 40;
  const size = 14;
  // At 100% the answer is the pure centred value, (40 - 14) / 2 = 13 — no
  // fudge constant. (An earlier `- 2` nudge up was an artifact of testing at
  // 90% zoom.)
  assert.equal(trafficLightY(bar, size, 1), 13);
  // Zooming scales the bar's DIP height, so the light drops to stay centred on
  // the taller bar: (40*1.5 - 14) / 2 = 23.
  assert.equal(trafficLightY(bar, size, 1.5), 23);
  // Half-integer DIP positions are rounded, like every other rect this module
  // hands to Electron.
  assert.equal(trafficLightY(bar, size, 1.25), 18);
  // Below ~0.35 zoom the bar is shorter than the light and the centred answer
  // goes negative — the light no longer fits, which is a genuine limit of the
  // centring, not something to clamp into a wrong position.
  assert.ok(trafficLightY(bar, size, 0.25) < 0);
});

test("zoomFactor falls back to 1 for anything unusable", () => {
  assert.equal(zoomFactor(1.5), 1.5);
  assert.equal(zoomFactor(0.8), 0.8);
  // A zero or negative factor would collapse every box multiplied by it, which
  // reads as a missing view rather than a bad number.
  assert.equal(zoomFactor(0), 1);
  assert.equal(zoomFactor(-2), 1);
  assert.equal(zoomFactor(NaN), 1);
  assert.equal(zoomFactor(null), 1);
  assert.equal(zoomFactor(undefined), 1);
  assert.equal(zoomFactor("1.25"), 1.25);
});

test("cssBoxToDip scales a CSS box into the view's own pixels", () => {
  const css = { x: 10, y: 20, width: 402, height: 874 };
  // At 100% the two spaces coincide, which is why this was invisible for so long.
  assert.deepEqual(cssBoxToDip(css, 1), css);
  // A zoomed-in page's CSS pixel is worth more than a DIP, so the same box covers
  // more of the window.
  assert.deepEqual(cssBoxToDip(css, 1.5), { x: 15, y: 30, width: 603, height: 1311 });
  assert.deepEqual(cssBoxToDip(css, 0.5), { x: 5, y: 10, width: 201, height: 437 });
  // Rounded, because `setBounds` takes integers.
  assert.deepEqual(cssBoxToDip({ x: 0, y: 0, width: 402, height: 874 }, 1.25), {
    x: 0,
    y: 0,
    width: 503,
    height: 1093,
  });
  // A bad factor is 1, never a collapsed box.
  assert.deepEqual(cssBoxToDip(css, 0), css);
});

test("emulationScale folds the page zoom into the renderer's fit factor", () => {
  // The renderer's number passes through untouched at 100%.
  assert.equal(emulationScale(1, 1), 1);
  assert.equal(emulationScale(0.5, 1), 0.5);
  // The bug this exists for: a phone that fits its pane has `scale` 1, so a /ide
  // at 150% asked Chromium to paint 1 DIP per CSS pixel into a view sized 1.5 —
  // a device frame two thirds full. Above 1 is not magnification here, it is what
  // one of the page's CSS pixels is worth.
  assert.equal(emulationScale(1, 1.5), 1.5);
  // And the inverse: at 80% the page was painted larger than the frame holding it.
  assert.equal(emulationScale(1, 0.8), 0.8);
  // A scaled-down device composes with the page zoom rather than replacing it.
  assert.equal(emulationScale(0.5, 1.5), 0.75);
  // Nonsense on either side reads as "we do not know", never as zero.
  assert.equal(emulationScale(0, 1.5), 1.5);
  assert.equal(emulationScale(NaN, 2), 2);
  assert.equal(emulationScale(0.5, -1), 0.5);
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
    worktreeId: null,
    repoRoot: null,
    bounds: null,
  });
  // An unknown kind from a newer build becomes a main window rather than
  // nothing: a window that fails to reopen is a layout, and its shells, lost.
  assert.deepEqual(parseWindowRecord({ kind: "floating", suffix: "nope" }), {
    suffix: null,
    kind: "main",
    origin: null,
    worktreeId: null,
    repoRoot: null,
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

  // An unreadable previous file starts a fresh one rather than refusing to
  // write. An empty list is not worth a key: a base with no windows and a base
  // that is absent reopen identically. (`lastMainBounds` is null on a first
  // launch, so it is always present but says nothing.)
  assert.deepEqual(JSON.parse(serializeWindowList("{oops", "main", [])), {
    lastMainBounds: null,
  });
});

test("serializeWindowList prunes bases nobody will read again", () => {
  // `claimSlot` mints a `main-<pid>` base whenever a second instance finds the
  // preferred one held, and once that run ends the key is unreachable — so
  // without pruning, every collision leaves one behind and the file grows
  // monotonically across launches.
  const previous = JSON.stringify({
    main: [{ suffix: null, kind: "main", origin: null }],
    "dev-41231": [{ suffix: null, kind: "main", origin: null }],
    "main-9982": [{ suffix: "w2", kind: "detached", origin: null }],
    dev: [{ suffix: null, kind: "main", origin: null }],
    stale: [],
  });
  const dead = () => false;
  const written = JSON.parse(
    serializeWindowList(
      previous,
      "main",
      [{ suffix: null, kind: "main", origin: null, bounds: null }],
      dead,
    ),
  );
  // `dev` is a real base, not a pid-derived one, so it survives whatever its
  // instance is doing; the two dead pid bases and the empty list go.
  assert.deepEqual(Object.keys(written).sort(), ["dev", "lastMainBounds", "main"]);
});

test("serializeWindowList does not delete a live instance's windows", () => {
  // The trap. Two dev instances are a normal thing to want: the first owns
  // `dev`, the second `dev-<pid>`. Pruning on the *shape* of the name meant the
  // first one's next persist wiped the second's window set out from under it
  // while it was still running — and since a quit suppresses persistence, the
  // second would never write it back.
  const alive = (pid) => pid === 41231;
  const previous = JSON.stringify({
    "dev-41231": [{ suffix: null, kind: "main", origin: null }],
    "dev-9982": [{ suffix: null, kind: "main", origin: null }],
  });
  const written = JSON.parse(
    serializeWindowList(previous, "dev", [{ suffix: null, kind: "main", origin: null, bounds: null }], alive),
  );
  assert.ok(written["dev-41231"], "a running second instance keeps its windows");
  assert.equal(written["dev-9982"], undefined, "a dead one is pruned");
});

test("serializeWindowList keeps the pid-derived base it is currently writing", () => {
  // A second dev instance *is* `dev-<pid>` for its whole run, and has to be able
  // to reopen its own windows — even though a liveness check on its own pid
  // would be answering a question about itself.
  const written = JSON.parse(
    serializeWindowList(
      "{}",
      "dev-41231",
      [{ suffix: null, kind: "main", origin: null, bounds: null }],
      () => false,
    ),
  );
  assert.deepEqual(Object.keys(written), ["dev-41231", "lastMainBounds"]);
});

test("serializeWindowList writes the last main window's bounds", () => {
  // The recalled size/position of a fresh main window, kept apart from the
  // window set so it survives a macOS close that empties the set.
  const written = JSON.parse(
    serializeWindowList("", "main", [], () => false, { x: 40, y: 33, width: 1500, height: 887 }),
  );
  assert.deepEqual(written.lastMainBounds, { x: 40, y: 33, width: 1500, height: 887 });
  // No remembered bounds yet (first launch): the key is present but null.
  assert.equal(JSON.parse(serializeWindowList("", "main", [])).lastMainBounds, null);
});

test("lastMainBounds survives a close that empties the window set", () => {
  // The macOS red-X path: the only window closes, the base is pruned to an
  // empty array, but the recalled bounds must still be there for the next
  // fresh window. This is the regression the fix exists for.
  const bounds = { x: 12, y: 33, width: 1500, height: 887 };
  const written = JSON.parse(serializeWindowList("", "main", [], () => false, bounds));
  assert.deepEqual(Object.keys(written), ["lastMainBounds"]);
  assert.deepEqual(readLastMainBounds(JSON.stringify(written)), bounds);
});

test("readLastMainBounds degrades to null", () => {
  // Nothing remembered, unreadable, or corrupt all mean "no recall", which is
  // the first-launch fallback to the default size.
  assert.equal(readLastMainBounds(""), null);
  assert.equal(readLastMainBounds("{oops"), null);
  assert.equal(readLastMainBounds(JSON.stringify({})), null);
  assert.equal(readLastMainBounds(JSON.stringify({ lastMainBounds: { width: 1, height: 1 } })), null);
});

test("handBackTarget prefers the window the tabs actually came from", () => {
  const main = { id: 1, suffix: null, kind: "main" };
  const w2 = { id: 2, suffix: "w2", kind: "detached" };
  const closing = { id: 3, suffix: "w3", kind: "detached", origin: "w2", originId: 2 };
  assert.equal(handBackTarget(closing, [main, w2]), w2);
});

test("handBackTarget does not deliver to a window that inherited the number", () => {
  // The trap suffix reuse sets. `w2` opens a detached `w3`, then `w2` closes and
  // a *new* window is allocated the free `w2`. Matching on the suffix would hand
  // w3's tabs to a window they never came from — a plausible-looking wrong
  // answer, which is worse than the fallback.
  const main = { id: 1, suffix: null, kind: "main" };
  const recycled = { id: 9, suffix: "w2", kind: "main" };
  const closing = { id: 3, suffix: "w3", kind: "detached", origin: "w2", originId: 2 };
  const target = handBackTarget(closing, [main, recycled]);
  assert.notEqual(target, recycled);
  assert.equal(target, main);
});

test("handBackTarget uses the suffix only for a restored window", () => {
  // A restored window carries no `originId` — only the suffix survived into
  // `windows.json`. Every record is new so no id could match, and no suffix has
  // been recycled yet either, which is what makes the persisted one usable here
  // and unusable in the test above.
  const restoredOrigin = { id: 41, suffix: "w2", kind: "detached" };
  const main = { id: 40, suffix: null, kind: "main" };
  const closing = { id: 42, suffix: "w3", kind: "detached", origin: "w2", originId: null };
  assert.equal(handBackTarget(closing, [main, restoredOrigin]), restoredOrigin);
});

test("handBackTarget ends at any main window, then at nothing", () => {
  const main = { id: 1, suffix: null, kind: "main" };
  const orphan = { id: 5, suffix: "w4", kind: "detached", origin: null, originId: null };
  // Anywhere beats nowhere: these tabs name live shells, and a shell nobody
  // adopts is hung up by the detach grace.
  assert.equal(handBackTarget(orphan, [main]), main);
  assert.equal(handBackTarget(orphan, []), null);
  // Only detached windows left: nothing to hand to rather than a wrong guess.
  assert.equal(handBackTarget(orphan, [{ id: 6, suffix: "w2", kind: "detached" }]), null);
});

test("restoreBudget reserves a window only when one will be needed", () => {
  const main = { kind: "main" };
  const detached = { kind: "detached" };
  // A stored set with a main window reopens whole — reserving unconditionally
  // dropped the last window of a full set, and with it a layout naming live
  // shells; the next quit then rewrote the set one shorter and made it permanent.
  assert.equal(restoreBudget([main, detached]), MAX_WINDOWS);
  assert.equal(restoreBudget(Array(MAX_WINDOWS).fill(main)), MAX_WINDOWS);
  // No main stored: one has to be opened afterwards, because an app whose every
  // window is a bare dock has no rail and no way back.
  assert.equal(restoreBudget([detached, detached]), MAX_WINDOWS - 1);
  assert.equal(restoreBudget([]), MAX_WINDOWS - 1);
});

test("a serialized list round-trips through the parser", () => {
  const records = [
    {
      suffix: null,
      kind: "main",
      origin: null,
      worktreeId: null,
      repoRoot: null,
      bounds: { x: 0, y: 0, width: 1280, height: 800 },
    },
    {
      // A detached window is a dock *for* a worktree and has no rail to
      // re-resolve one, so this has to survive a restart or it reopens blank.
      suffix: "w2",
      kind: "detached",
      origin: null,
      worktreeId: 12,
      repoRoot: "/Users/x/code/veld",
      bounds: null,
    },
  ];
  const parsed = parseWindowList(serializeWindowList("", "main", records), "main");
  assert.deepEqual(parsed, records);
});

// ---------------------------------------------------------------------------
// Worktree ownership
// ---------------------------------------------------------------------------

test("ownsWorktree reads the display map for a main window and the field for a detached one", () => {
  const showing = new Map([[7, 1]]);
  const main = { id: 1, kind: "main", worktreeId: 42 };
  const other = { id: 2, kind: "main", worktreeId: 7 };
  const dock = { id: 3, kind: "detached", worktreeId: 8 };

  assert.equal(ownsWorktree(main, 7, showing), true, "the window displaying it");
  // The trap: `worktreeId` on a *main* window is what it was opened for, not
  // what it shows now. Reading it there routes a drop at a window that moved on.
  assert.equal(ownsWorktree(main, 42, showing), false);
  assert.equal(ownsWorktree(other, 7, showing), false, "wanting it is not showing it");
  // A detached window never reports what it displays — it is a satellite of its
  // origin — so the field is the only thing that says which dock it is. Matching
  // on the map alone made a detached window impossible to drop onto.
  assert.equal(ownsWorktree(dock, 8, showing), true);
  assert.equal(ownsWorktree(dock, 7, showing), false);
});

/** A window record as `cycleOrder` reads one — id, and the tab strip its
 *  renderer last reported. Nothing else in a `WindowRecord` is looked at. */
function win(id, worktreeId, ids, activeId = null) {
  return { id, tabs: worktreeId === null ? null : { worktreeId, ids, activeId } };
}

test("cycleOrder is windows by record id, then each window's strip as drawn", () => {
  // Deliberately out of id order in the input: the caller's list is whatever
  // `allRecords()` iterates, and a Map's insertion order is not creation order
  // once a window has been closed and another opened.
  const order = cycleOrder([win(3, 7, ["c1"]), win(1, 7, ["a1", "a2"]), win(2, 7, ["b1"])], 7);
  assert.deepEqual(
    order.map((e) => e.tabId),
    ["a1", "a2", "b1", "c1"],
  );
  // The record id travels with each entry — the caller needs to know whether the
  // answer is its own tab (activate it here) or somebody else's (raise them).
  assert.deepEqual(
    order.map((e) => e.recordId),
    [1, 1, 2, 3],
  );
});

test("cycleOrder takes only the windows showing this worktree", () => {
  const records = [
    win(1, 7, ["a1"]),
    win(2, 9, ["b1"]), // another worktree's dock — never in this cycle
    win(3, null, []), // reported nothing yet: mid-load, or a settings window
    win(4, 7, []), // showing it, but with an empty strip
    win(5, 7, ["e1"]),
  ];
  assert.deepEqual(
    cycleOrder(records, 7).map((e) => e.tabId),
    ["a1", "e1"],
  );
  assert.deepEqual(cycleOrder(records, 100), [], "a worktree no window is showing");
});

test("nextInCycle steps and wraps across windows from the caller's own position", () => {
  const order = cycleOrder([win(1, 7, ["a1", "a2"]), win(2, 7, ["b1"])], 7);

  assert.deepEqual(nextInCycle(order, 1, "a1", 1), { recordId: 1, tabId: "a2" });
  // Off the end of this window's own tabs and into the next window's.
  assert.deepEqual(nextInCycle(order, 1, "a2", 1), { recordId: 2, tabId: "b1" });
  // …and around, which is what makes a small fixed strip feel like a strip.
  assert.deepEqual(nextInCycle(order, 2, "b1", 1), { recordId: 1, tabId: "a1" });
  assert.deepEqual(nextInCycle(order, 1, "a1", -1), { recordId: 2, tabId: "b1" });
});

test("nextInCycle does not ping-pong between a tab and another window", () => {
  // The failure that pulled #315. There, the position was remembered in the
  // window the cycle started in, which had no way to represent "parked on a
  // window whose tabs I cannot read" — so a second press recomputed from the
  // same stale docked tab and landed on the same detached window again.
  //
  // Here the position is `(sender, activeId)`, and after the first press the
  // *other* window has focus, so the second press arrives from it. Stepping on
  // is then ordinary arithmetic, and the caller cannot pass a position it does
  // not actually have.
  const order = cycleOrder([win(1, 7, ["a1", "a2"]), win(2, 7, ["b1", "b2"])], 7);
  assert.deepEqual(nextInCycle(order, 1, "a2", 1), { recordId: 2, tabId: "b1" });
  assert.deepEqual(nextInCycle(order, 2, "b1", 1), { recordId: 2, tabId: "b2" });
  assert.deepEqual(nextInCycle(order, 2, "b2", 1), { recordId: 1, tabId: "a1" });
});

test("nextInCycle starts at an end when there is no position", () => {
  const order = cycleOrder([win(1, 7, ["a1", "a2"]), win(2, 7, ["b1"])], 7);
  assert.deepEqual(nextInCycle(order, 1, null, 1), { recordId: 1, tabId: "a1" });
  // The one-sided bug: `(-1 - 1) % 3` is second-from-last, not last. Backward
  // from nothing has to be the *last* entry, so it is special-cased rather than
  // folded into the modulo.
  assert.deepEqual(nextInCycle(order, 1, null, -1), { recordId: 2, tabId: "b1" });
  // An id that is real but belongs to another window is "not found" too — the
  // match is on the pair, so one window cannot claim another's position.
  assert.deepEqual(nextInCycle(order, 1, "b1", 1), { recordId: 1, tabId: "a1" });
  assert.equal(nextInCycle([], 1, null, 1), null, "nothing to step to");
});

test("dropDelivery queues only for a window that said its listener is gone", () => {
  // A claim is recorded when a window *asks* for a worktree; the listener that
  // answers a drop arrives once `/ide` has mounted. Pushing into that gap went
  // nowhere and reported a refusal two seconds later.
  assert.equal(dropDelivery("gone"), "queue");
  assert.equal(dropDelivery("ready"), "send");
  // "Never reported" is not "gone". It means *loaded and has not reported* — an
  // older /ide bundle that never will, or a current one between its load and
  // `PaneArea` mounting. Sending is right for both: the older bundle answers a
  // `drop-here` perfectly well, and for the newer one the drop ack's own timeout
  // falls back to the queue. Note what this case is NOT: a page that has not
  // finished loading is queued for by the *caller*, which asks the window itself
  // (`webContents.isLoading()`) rather than waiting for the page to report — that
  // is the longer gap and it is the shell's own knowledge.
  assert.equal(dropDelivery("unknown"), "send");
  assert.equal(dropDelivery(undefined), "send");
});

test("nextListenerState demotes a live listener only on a real document swap", () => {
  const swap = { isMainFrame: true, isSameDocument: false };
  assert.equal(nextListenerState("ready", swap), "gone");
  // An iframe's load turns the tab spinner and a `pushState` is not a new
  // document; demoting on either would strand the window in "gone" with nothing
  // able to undo it, since the renderer reports `ready` on mount and it is already
  // mounted.
  assert.equal(nextListenerState("ready", { isMainFrame: false, isSameDocument: false }), "ready");
  assert.equal(nextListenerState("ready", { isMainFrame: true, isSameDocument: true }), "ready");
  // `unknown` means "has never reported", which a reload does not change. Demoting
  // it would hold an older bundle in the degraded path for the rest of the session
  // — the trap this function exists to hold.
  assert.equal(nextListenerState("unknown", swap), "unknown");
  assert.equal(nextListenerState("gone", swap), "gone");
});


test("handBackTransfers carries a queue on from any window, its own tabs only from a dock", () => {
  const carried = [{ worktreeId: 7, tabs: [{ id: "t1" }] }];
  const own = { worktreeId: 8, tabs: [{ id: "t2" }] };

  // The branch this change added. A drop routed at a loading window parks in its
  // queue and the *source has already let go* on the strength of that, so closing
  // the window before it drained would end exactly the shells the ack protocol
  // protects. A queue is a resting place, never a grave.
  assert.deepEqual(handBackTransfers("main", carried, own), carried, "no snapshot from a main window");
  assert.deepEqual(handBackTransfers("main", [], own), []);

  // A detached window hands both on, queue first, so tabs arrive in the order they
  // were sent.
  assert.deepEqual(handBackTransfers("detached", carried, own), [...carried, own]);
  assert.deepEqual(handBackTransfers("detached", [], own), [own]);
  // An empty snapshot travels as nothing rather than as an empty transfer.
  assert.deepEqual(handBackTransfers("detached", [], { worktreeId: 8, tabs: [] }), []);
  assert.deepEqual(handBackTransfers("detached", [], null), []);
});

test("releaseClaims drops every claim a window held", () => {
  // A window shows one worktree at a time, so taking a claim releases the old
  // one through here — and closing releases all of them. A claim outliving its
  // window would refuse every other window forever.
  const claims = new Map([
    [7, 1],
    [8, 2],
    [9, 1],
  ]);
  releaseClaims(claims, 1);
  assert.deepEqual([...claims], [[8, 2]]);
  releaseClaims(claims, 99);
  assert.deepEqual([...claims], [[8, 2]], "an unknown window changes nothing");
});
