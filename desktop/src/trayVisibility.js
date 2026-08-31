// Whether the menu-bar icon is wanted, and how to act on that answer safely.
//
// Its own module for the reason `updatePolicy.js` and `windowState.js` are:
// `main.js` cannot be imported outside Electron, so logic that stays in there is
// logic no test can reach. Both exports below are pure of Electron — one parses a
// settings document, the other serialises callers — which is exactly the part that
// was subtly wrong before it had a test (see `serialize`).

/**
 * Read `desktop.menuBarIcon` out of a `GET /api/settings` body.
 *
 * `fallback` is returned for **every** shape that is not a boolean, and that is
 * the load-bearing rule rather than defensive habit: a daemon older than the key
 * sends no such field, a daemon that is down sends nothing at all, and neither
 * may take the icon away from a user who never asked to lose it. So only an
 * explicit `false` from a daemon that answered turns it off.
 *
 * @param {unknown} body Parsed JSON body, or anything at all.
 * @param {boolean} fallback The answer to keep when the body does not carry one.
 * @returns {boolean}
 */
function menuBarIconFrom(body, fallback) {
  const value = /** @type {any} */ (body)?.settings?.["desktop.menuBarIcon"];
  return typeof value === "boolean" ? value : fallback;
}

/**
 * Wrap an async function so calls run one after another, never overlapping.
 *
 * The tray sync reads the setting and *then* creates or destroys the `Tray`, with
 * an `await` in the middle. Two overlapping runs both observed "no tray yet"
 * across that await and both created one — two icons in the menu bar, the first
 * orphaned beyond the reach of the variable that tracks it. That is reachable
 * with no unusual timing at all: the 10s tick and a window's settings nudge, or
 * two windows nudging at once.
 *
 * **Chained, deliberately, rather than coalesced.** Returning the in-flight run
 * to a second caller would be cheaper and is wrong here: a nudge means "the
 * document just changed", and the run already in flight may have read it before
 * that change landed — so the toggle would appear to do nothing until the next
 * tick. Chaining guarantees every nudge is followed by a *fresh* read.
 *
 * A rejection is swallowed so one failure cannot poison the chain and stop every
 * later sync; the worker is expected to handle its own errors.
 *
 * @template {(...args: any[]) => Promise<any>} F
 * @param {F} fn
 * @returns {(...args: Parameters<F>) => Promise<void>}
 */
function serialize(fn) {
  /** @type {Promise<void>} */
  let queue = Promise.resolve();
  return (...args) => {
    queue = queue.then(() => fn(...args)).then(
      () => undefined,
      () => undefined,
    );
    return queue;
  };
}

module.exports = { menuBarIconFrom, serialize };
