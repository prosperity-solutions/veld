const test = require("node:test");
const assert = require("node:assert");

const { menuBarIconFrom, serialize } = require("./trayVisibility");

test("an explicit boolean is taken, either way", () => {
  assert.equal(menuBarIconFrom({ settings: { "desktop.menuBarIcon": false } }, true), false);
  assert.equal(menuBarIconFrom({ settings: { "desktop.menuBarIcon": true } }, false), true);
});

/**
 * The direction that matters: nothing but a boolean may remove the icon.
 *
 * Each of these is a real state — a daemon older than the key sends no field, a
 * daemon that is down produces no body, and a truncated read produces neither —
 * and every one of them must keep the icon the user already has.
 */
test("anything that is not a boolean keeps the previous answer", () => {
  for (const body of [
    undefined,
    null,
    {},
    { settings: {} },
    { settings: null },
    { settings: { "desktop.menuBarIcon": undefined } },
    // A value of the wrong type must not be coerced: "false" is truthy and 0 is
    // falsy, so coercion would be wrong in both directions.
    { settings: { "desktop.menuBarIcon": "false" } },
    { settings: { "desktop.menuBarIcon": 0 } },
    { settings: { "desktop.menuBarIcon": 1 } },
    "not an object",
    42,
  ]) {
    assert.equal(menuBarIconFrom(body, true), true, `body: ${JSON.stringify(body)}`);
    assert.equal(menuBarIconFrom(body, false), false, `body: ${JSON.stringify(body)}`);
  }
});

/**
 * The bug this module exists for.
 *
 * Two callers overlapping across the worker's own `await` both saw "no tray yet"
 * and both created one — two menu-bar icons, the first orphaned. The invariant is
 * that no second run starts while one is in flight.
 */
test("serialised calls never overlap", async () => {
  let inFlight = 0;
  let maxInFlight = 0;
  const run = serialize(async () => {
    inFlight += 1;
    maxInFlight = Math.max(maxInFlight, inFlight);
    await new Promise((resolve) => setTimeout(resolve, 5));
    inFlight -= 1;
  });
  await Promise.all([run(), run(), run(), run()]);
  assert.equal(maxInFlight, 1, "two runs were in flight at once");
  assert.equal(inFlight, 0);
});

/** …and each call really runs, rather than being folded into the one in flight:
 *  a nudge means "read it again", and the run already going may have read the
 *  document before the change that prompted the nudge. */
test("every call runs, in order", async () => {
  const seen = [];
  const run = serialize(async (n) => {
    await new Promise((resolve) => setTimeout(resolve, 2));
    seen.push(n);
  });
  await Promise.all([run(1), run(2), run(3)]);
  assert.deepEqual(seen, [1, 2, 3]);
});

/** One failure must not poison the chain — otherwise a single unreachable-daemon
 *  read would stop the tray syncing for the rest of the session. */
test("a rejection does not stop later calls", async () => {
  const seen = [];
  const run = serialize(async (n) => {
    if (n === 1) throw new Error("daemon unreachable");
    seen.push(n);
  });
  await assert.doesNotReject(() => Promise.all([run(1), run(2)]));
  assert.deepEqual(seen, [2]);
});
