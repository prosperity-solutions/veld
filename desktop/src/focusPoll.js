// A poll that only runs while its window has focus.
//
// Its own module, Electron-free, so `node --test src/*.test.js` can drive it with
// a fake window and fake timers; `windows.js` holds everything that needs a real
// `BrowserWindow`.

/**
 * Run `fn` every `ms` while `win` is focused, and not at all while it is not.
 *
 * On focus the poll restarts and `fn` runs **at once**, so whatever changed while
 * the window was in the background (a zoom set by wheel, a restored zoom) is
 * caught up immediately rather than a tick later. On blur the timer is cleared,
 * which is the point: a window nobody is interacting with has nothing to follow
 * at 10 Hz.
 *
 * Per window — each call owns its own timer and listeners, so one window losing
 * focus never stops another's poll.
 *
 * If the window is focused when this is called the poll starts straight away
 * (without the immediate `fn`: the caller syncs on load itself); otherwise it
 * waits for the first focus.
 *
 * Returns a `stop` that clears the timer and removes the listeners. It is called
 * on `closed`, and is safe to call twice.
 *
 * @param {{ on: Function, removeListener: Function, isFocused: () => boolean,
 *           isDestroyed?: () => boolean }} win
 * @param {() => void} fn
 * @param {number} ms
 * @param {{ setInterval: Function, clearInterval: Function }} [timers]
 * @returns {() => void}
 */
function focusGatedInterval(win, fn, ms, timers = { setInterval, clearInterval }) {
  let timer = null;
  const start = () => {
    if (timer !== null) return;
    timer = timers.setInterval(fn, ms);
  };
  const halt = () => {
    if (timer === null) return;
    timers.clearInterval(timer);
    timer = null;
  };
  const onFocus = () => {
    // `start` first, so a `fn` that throws still leaves the poll running.
    start();
    fn();
  };
  const onBlur = () => halt();

  win.on("focus", onFocus);
  win.on("blur", onBlur);
  if (win.isFocused()) start();

  let stopped = false;
  return () => {
    if (stopped) return;
    stopped = true;
    halt();
    // A destroyed BrowserWindow throws on any method call; `closed` is the usual
    // caller, by which point the listeners are gone with the window anyway.
    if (win.isDestroyed?.()) return;
    win.removeListener("focus", onFocus);
    win.removeListener("blur", onBlur);
  };
}

module.exports = { focusGatedInterval };
