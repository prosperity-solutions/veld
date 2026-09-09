/**
 * The one pure rule behind "a page load must not move the keyboard".
 *
 * A file of its own for one function, for the same reason as `safeArea.js` next
 * door: nothing in `browserViews.js` can be unit-tested, and this rule can.
 *
 * Chromium gives a `WebContentsView` keyboard focus on **every** committed
 * main-frame navigation, including a reload it started itself. Measured on
 * Electron 43 with a page carrying `<meta http-equiv="refresh" content="3">`:
 * the view takes focus 2-26 ms after `did-start-navigation`, once per refresh,
 * and — this is the part that makes it a bug rather than a quirk — it does so
 * while the view is `setVisible(false)`, i.e. while its tab is not even the one
 * on screen. So a background pane previewing an auto-refreshing dev server
 * emptied the keyboard out of whatever the user was actually typing in, every
 * few seconds, with nothing on screen changing to explain it.
 *
 * (The `<iframe>` backend has no such behaviour — measured the same way, the
 * host document's `activeElement` is untouched across an iframe's reloads. This
 * is a native-view problem only, which is why the fix lives in this process.)
 *
 * The rule: **a navigation may keep the keyboard in a pane that already had it,
 * and may never take it into one that did not** — unless the keyboard was
 * explicitly *asked* for there. Clicking into a page, and following a link from
 * a page you are already typing in, both keep working. A reload of a pane you
 * are not in does nothing.
 *
 * The first half of that rests on a measured fact worth writing down, because two
 * review angles independently guessed the other way: **Chromium does not blur a
 * view that already holds focus when its own page reloads.** It raises `focus`
 * again with no `blur` in between, so `entry.focused` is still true when the
 * guard reads it. (Checked against the patched code: a pane focused explicitly,
 * then left to refresh three times, was accepted every time and the host never
 * got the keyboard back.) If that ever changes, this rule silently inverts for
 * the one pane the user is actually typing in — which is why it is stated here
 * rather than left to be re-derived.
 *
 * The first load of a freshly created pane is guarded too, so opening a browser
 * pane leaves the keyboard where it already was in `/ide`. That is deliberate
 * rather than an oversight: the exception would have to read "except when the
 * view has never navigated", which is also the state a crashed-and-reloaded
 * renderer is in, and one rule with no exceptions is the one the next reader can
 * predict. The cost is one click before the keyboard reaches a newly opened page.
 */

/**
 * How long after a navigation starts an incoming focus is attributed to it.
 *
 * **Recency alone, deliberately — not "is the load still running".** The first
 * version also required a `did-start-navigation` with no `did-stop-loading` after
 * it, and that is a race the measurements themselves show: the steal lands 2-26 ms
 * after the navigation starts and the load finishes 5-125 ms after it, so the two
 * overlap, and every observed margin between them was 3-20 ms. A load that
 * finishes inside that margin — a memory-cache reload, a 304 on localhost, a
 * `data:` URL — would have disarmed the guard before the focus it exists to
 * refuse ever arrived. Nothing is lost by dropping the flag: a focus in this
 * window that the user asked for is covered by [`FOCUS_REQUEST_WINDOW_MS`], and
 * the load is the only other thing that raises one.
 *
 * Generously above the 26 ms the steal was measured at, because the cost of being
 * generous is now only that an *unrequested* focus is refused for a second after
 * a navigation — and the load is what those are.
 */
const LOAD_FOCUS_WINDOW_MS = 1000;

/**
 * How long an explicit request for the keyboard vouches for the focus that follows.
 *
 * Three things set it, and they are the ways the keyboard is *asked* to come here
 * rather than arriving on its own: a press on the page, the app's own `focus`
 * command over IPC, and the hand-back that undoes a refused steal.
 *
 * Without it the guard has a real hole: a page that takes a while to load leaves
 * the window above open for as long as it does, and a click landing inside that
 * window is indistinguishable, on navigation state alone, from Chromium's own
 * refocus — so the user's click into a slow-loading pane was refused, and so was
 * an explicit "focus this pane" that happened to land during one.
 *
 * Deliberately short. It vouches for a focus raised in the same gesture, not for
 * the pane taking focus at will for a quarter second afterwards.
 *
 * **Both orderings are covered, because which one Chromium uses is not something
 * this process can rely on.** If the press is reported before the focus, the focus
 * is let through here. If the focus lands first it is refused, and the
 * `input-event` handler that records the press re-focuses the view behind it —
 * which this window is then what accepts. See `browserViews.js`'s `input-event`
 * listener.
 */
const FOCUS_REQUEST_WINDOW_MS = 250;

/**
 * Did this focus arrive because the page loaded, rather than because something
 * asked for the keyboard to be here?
 *
 * `now`, `navStartedAt` and `requestedAt` must all come from the **same monotonic
 * clock** — `performance.now()`, not `Date.now()`; see the callers.
 *
 * @param {{focused: boolean, navStartedAt: number, requestedAt: number}} entry
 * @param {number} now
 */
function isLoadFocus(entry, now) {
  // It already held the keyboard. Whatever the navigation is, it is not taking
  // focus from anyone — this is the click-a-link case.
  if (entry.focused) return false;
  // The keyboard was just asked for here. Nothing a load does looks like that.
  if (now - entry.requestedAt <= FOCUS_REQUEST_WINDOW_MS) return false;
  return now - entry.navStartedAt <= LOAD_FOCUS_WINDOW_MS;
}

module.exports = { isLoadFocus, LOAD_FOCUS_WINDOW_MS, FOCUS_REQUEST_WINDOW_MS };
