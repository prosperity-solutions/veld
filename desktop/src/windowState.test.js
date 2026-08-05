// Tests for the window registry's arithmetic and parsing. No Electron binary,
// which is the reason this logic lives apart from `windows.js`.
const test = require("node:test");
const assert = require("node:assert/strict");
const {
  MAX_WINDOWS,
  canOpenAnother,
  dropDelivery,
  forgetWorktrees,
  handBackTarget,
  handBackTransfers,
  isSuffix,
  nextDropListener,
  nextSuffix,
  othersHolding,
  ownsWorktree,
  parseWindowList,
  parseWindowRecord,
  releaseClaims,
  releaseHolds,
  restoreBudget,
  setHolds,
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
  // that is absent reopen identically.
  assert.deepEqual(JSON.parse(serializeWindowList("{oops", "main", [])), {});
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
  assert.deepEqual(Object.keys(written).sort(), ["dev", "main"]);
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
  assert.deepEqual(Object.keys(written), ["dev-41231"]);
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

test("setHolds records exactly what a window holds, and nothing it dropped", () => {
  const holders = new Map();
  setHolds(holders, 1, [7, 9]);
  setHolds(holders, 2, [9]);
  assert.deepEqual([...holders.get(7)], [1]);
  assert.deepEqual([...holders.get(9)].sort(), [1, 2]);

  // A window reports its whole set each time, so a worktree it no longer holds
  // has to disappear — otherwise it would keep being asked to yield panes it
  // gave up long ago, and worse, keep being counted as a place they still are.
  setHolds(holders, 1, [7]);
  assert.deepEqual([...holders.get(9)], [2]);

  // An emptied set is deleted rather than left as an empty husk, so
  // `othersHolding` never has to distinguish "nobody" from "an empty entry".
  setHolds(holders, 2, []);
  assert.equal(holders.has(9), false);
});

test("othersHolding excludes the window that is claiming", () => {
  const holders = new Map();
  setHolds(holders, 1, [7]);
  setHolds(holders, 2, [7]);
  setHolds(holders, 3, [8]);
  // The claimer must not be told to let go of what it is taking.
  assert.deepEqual(othersHolding(holders, 7, 1), [2]);
  assert.deepEqual(othersHolding(holders, 7, 2), [1]);
  // Nobody else holds it, and nobody holds it at all — both are "no yields".
  assert.deepEqual(othersHolding(holders, 8, 3), []);
  assert.deepEqual(othersHolding(holders, 99, 1), []);
});

test("releaseHolds forgets a window entirely", () => {
  const holders = new Map();
  setHolds(holders, 1, [7, 8]);
  setHolds(holders, 2, [8]);
  releaseHolds(holders, 1);
  assert.equal(holders.has(7), false, "a set with only the dead window goes");
  assert.deepEqual([...holders.get(8)], [2], "a shared one keeps the survivor");
});

test("forgetWorktrees drops a deleted worktree from both maps, whoever held it", () => {
  // Worktree rowids are reused (`INTEGER PRIMARY KEY`, no AUTOINCREMENT), so a
  // claim left on a deleted worktree greys out whichever one is created next and
  // focuses a window that is showing something else.
  const claims = new Map([
    [7, 1],
    [8, 2],
  ]);
  const holders = new Map();
  setHolds(holders, 1, [7, 8]);
  setHolds(holders, 2, [8]);

  forgetWorktrees(claims, holders, [8]);
  assert.deepEqual([...claims], [[7, 1]], "the claim goes, whichever window had it");
  assert.equal(holders.has(8), false, "and so does every hold on it");
  assert.deepEqual([...holders.get(7)], [1], "an unrelated worktree is untouched");

  forgetWorktrees(claims, holders, [99]);
  assert.deepEqual([...claims], [[7, 1]], "an unknown worktree changes nothing");
  forgetWorktrees(claims, holders, []);
  assert.deepEqual([...claims], [[7, 1]], "and neither does an empty list");
});

test("ownsWorktree reads the claim for a main window and the field for a detached one", () => {
  const claims = new Map([[7, 1]]);
  const main = { id: 1, kind: "main", worktreeId: 42 };
  const other = { id: 2, kind: "main", worktreeId: 7 };
  const dock = { id: 3, kind: "detached", worktreeId: 8 };

  assert.equal(ownsWorktree(main, 7, claims), true, "the window showing it");
  // The trap: `worktreeId` on a *main* window is what it was opened for, not
  // what it shows now. Reading it there routes a drop at a window that moved on.
  assert.equal(ownsWorktree(main, 42, claims), false);
  assert.equal(ownsWorktree(other, 7, claims), false, "wanting it is not holding it");
  // A detached window never claims — it is a satellite of its origin's claim —
  // so the field is the only thing that says which dock it is. Matching on the
  // claim alone made a detached window impossible to drop onto.
  assert.equal(ownsWorktree(dock, 8, claims), true);
  assert.equal(ownsWorktree(dock, 7, claims), false);
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

test("nextDropListener demotes a live listener only on a real document swap", () => {
  const swap = { isMainFrame: true, isSameDocument: false };
  assert.equal(nextDropListener("ready", swap), "gone");
  // An iframe's load turns the tab spinner and a `pushState` is not a new
  // document; demoting on either would route every later drop into the queue with
  // nothing able to undo it, since the renderer reports `ready` on mount and it is
  // already mounted.
  assert.equal(nextDropListener("ready", { isMainFrame: false, isSameDocument: false }), "ready");
  assert.equal(nextDropListener("ready", { isMainFrame: true, isSameDocument: true }), "ready");
  // `unknown` means "has never reported", which a reload does not change. Demoting
  // it would make an older bundle's every drop take the append path for the rest
  // of the session — the trap this function exists to hold.
  assert.equal(nextDropListener("unknown", swap), "unknown");
  assert.equal(nextDropListener("gone", swap), "gone");
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
