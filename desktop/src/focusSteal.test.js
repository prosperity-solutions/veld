const test = require("node:test");
const assert = require("node:assert");

const { isLoadFocus, LOAD_FOCUS_WINDOW_MS, FOCUS_REQUEST_WINDOW_MS } = require("./focusSteal");

// Mirrors what `browserViews.js` seeds a fresh entry with: nothing has navigated
// and nothing has asked, so both stamps are "has not happened".
const entry = (over) => ({
  focused: false,
  navStartedAt: Number.NEGATIVE_INFINITY,
  requestedAt: Number.NEGATIVE_INFINITY,
  ...over,
});

test("a focus arriving just after a navigation started belongs to the load", () => {
  assert.equal(isLoadFocus(entry({ navStartedAt: 1000 }), 1005), true);
});

test("a pane that already had the keyboard keeps it across its own navigation", () => {
  // Clicking a link in a pane you are typing in. Chromium raises `focus` again
  // with no `blur` between, so `focused` is still true here — measured, and the
  // reason the module says so out loud.
  assert.equal(isLoadFocus(entry({ focused: true, navStartedAt: 1000 }), 1005), false);
});

test("a focus long after the last navigation is somebody asking", () => {
  // Clicking into an idle pane, with no press recorded yet because the ordering
  // put the focus first.
  assert.equal(isLoadFocus(entry({ navStartedAt: 1000 }), 1000 + LOAD_FOCUS_WINDOW_MS + 1), false);
});

test("a pane that has never navigated cannot be blamed on a load", () => {
  assert.equal(isLoadFocus(entry(), 1005), false);
});

test("the deadline is the only navigation state consulted", () => {
  // Deliberately no "is the load still running" half: the steal (2-26 ms after
  // the start) and `did-stop-loading` (5-125 ms after) overlap, so a fast load
  // would have disarmed the guard before the focus it exists to refuse arrived.
  const nav = entry({ navStartedAt: 1000 });
  assert.equal(isLoadFocus(nav, 1000 + LOAD_FOCUS_WINDOW_MS), true);
  assert.equal(isLoadFocus(nav, 1000 + LOAD_FOCUS_WINDOW_MS + 1), false);
});

test("the measured steal is well inside the deadline", () => {
  // Measured on Electron 43: the view takes focus 2-26 ms after
  // `did-start-navigation`. The window is generous on purpose — an unrequested
  // focus in that second is what the load raises.
  assert.ok(LOAD_FOCUS_WINDOW_MS > 26);
});

test("a request for the keyboard outranks a navigation in flight", () => {
  // Clicking into a pane that is still loading, the app's own `focus` command
  // landing during one, and the hand-back that undoes a refused steal — all three
  // are indistinguishable from Chromium's refocus on navigation state alone.
  const asked = entry({ navStartedAt: 1000, requestedAt: 1200 });
  assert.equal(isLoadFocus(asked, 1200 + FOCUS_REQUEST_WINDOW_MS), false);
});

test("a request stops vouching once it is old", () => {
  const stale = entry({ navStartedAt: 1000, requestedAt: 1000 });
  assert.equal(isLoadFocus(stale, 1000 + FOCUS_REQUEST_WINDOW_MS + 1), true);
});

test("the request window is shorter than the navigation deadline", () => {
  // It vouches for a focus raised in the same gesture, not for the pane taking
  // focus at will for as long as a load runs.
  assert.ok(FOCUS_REQUEST_WINDOW_MS < LOAD_FOCUS_WINDOW_MS);
});

test("a zero stamp is not read as 'just now' at the clock's origin", () => {
  // The stamps come from `performance.now()`, whose origin is process start — so
  // a `0` default would put a pane created in the app's first second inside both
  // windows. `-Infinity` is what makes "has not happened" mean it.
  assert.equal(isLoadFocus(entry({ navStartedAt: 0 }), 1), true, "a zero start is a real start");
  assert.equal(isLoadFocus(entry({ navStartedAt: 0, requestedAt: 0 }), 1), false);
  assert.equal(isLoadFocus(entry(), 1), false, "an unset entry is inside no window");
});
