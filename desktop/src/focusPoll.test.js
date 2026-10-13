// Tests for the focus-gated poll: no Electron binary, a fake window and fake timers.
const test = require("node:test");
const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const { focusGatedInterval } = require("./focusPoll");

/** A window that is just focus state plus an emitter. */
function fakeWindow(focused) {
  const win = new EventEmitter();
  win.focused = focused;
  win.isFocused = () => win.focused;
  win.isDestroyed = () => false;
  win.focus = () => {
    win.focused = true;
    win.emit("focus");
  };
  win.blur = () => {
    win.focused = false;
    win.emit("blur");
  };
  return win;
}

/** Timers driven by hand: `tick()` fires every live interval once. */
function fakeTimers() {
  let next = 1;
  const live = new Map();
  return {
    setInterval: (fn, ms) => {
      const id = next++;
      live.set(id, { fn, ms });
      return id;
    },
    clearInterval: (id) => live.delete(id),
    tick: () => {
      for (const { fn } of [...live.values()]) fn();
    },
    count: () => live.size,
    msOf: () => [...live.values()].map((t) => t.ms),
  };
}

test("runs at the given cadence while focused", () => {
  const win = fakeWindow(true);
  const timers = fakeTimers();
  let calls = 0;
  focusGatedInterval(win, () => calls++, 100, timers);
  assert.deepEqual(timers.msOf(), [100]);
  timers.tick();
  timers.tick();
  assert.equal(calls, 2);
});

test("does not call fn on start when already focused (the caller syncs on load)", () => {
  const win = fakeWindow(true);
  let calls = 0;
  focusGatedInterval(win, () => calls++, 100, fakeTimers());
  assert.equal(calls, 0);
});

test("stops on blur: no timer, no ticks", () => {
  const win = fakeWindow(true);
  const timers = fakeTimers();
  let calls = 0;
  focusGatedInterval(win, () => calls++, 100, timers);
  win.blur();
  assert.equal(timers.count(), 0);
  timers.tick();
  assert.equal(calls, 0);
});

test("restarts on focus and syncs immediately", () => {
  const win = fakeWindow(true);
  const timers = fakeTimers();
  let calls = 0;
  focusGatedInterval(win, () => calls++, 100, timers);
  win.blur();
  win.focus();
  assert.equal(calls, 1, "fn ran at once on focus");
  assert.equal(timers.count(), 1);
  timers.tick();
  assert.equal(calls, 2);
});

test("a window created in the background waits for its first focus", () => {
  const win = fakeWindow(false);
  const timers = fakeTimers();
  let calls = 0;
  focusGatedInterval(win, () => calls++, 100, timers);
  assert.equal(timers.count(), 0);
  win.focus();
  assert.equal(calls, 1);
  assert.equal(timers.count(), 1);
});

test("repeated focus does not stack timers", () => {
  const win = fakeWindow(true);
  const timers = fakeTimers();
  focusGatedInterval(win, () => {}, 100, timers);
  win.emit("focus");
  win.emit("focus");
  assert.equal(timers.count(), 1);
});

test("windows are independent: one blurring leaves the other polling", () => {
  const a = fakeWindow(true);
  const b = fakeWindow(true);
  const timers = fakeTimers();
  const calls = { a: 0, b: 0 };
  focusGatedInterval(a, () => calls.a++, 100, timers);
  focusGatedInterval(b, () => calls.b++, 100, timers);
  a.blur();
  timers.tick();
  assert.deepEqual(calls, { a: 0, b: 1 });
  b.blur();
  assert.equal(timers.count(), 0);
  a.focus();
  assert.deepEqual(calls, { a: 1, b: 1 });
  assert.equal(timers.count(), 1);
});

test("stop clears the timer, removes the listeners, and is idempotent", () => {
  const win = fakeWindow(true);
  const timers = fakeTimers();
  let calls = 0;
  const stop = focusGatedInterval(win, () => calls++, 100, timers);
  stop();
  stop();
  assert.equal(timers.count(), 0);
  assert.equal(win.listenerCount("focus"), 0);
  assert.equal(win.listenerCount("blur"), 0);
  win.focus();
  assert.equal(calls, 0);
});

test("stop on an already destroyed window does not touch it", () => {
  const win = fakeWindow(true);
  const timers = fakeTimers();
  const stop = focusGatedInterval(win, () => {}, 100, timers);
  win.isDestroyed = () => true;
  win.removeListener = () => {
    throw new Error("destroyed");
  };
  stop();
  assert.equal(timers.count(), 0);
});
