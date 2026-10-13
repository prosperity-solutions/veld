import { pollDue, pollOnShow } from "./pollCadence";

/**
 * `setInterval` that slows to the hidden cadence while the document is hidden and
 * catches up when it is shown again — the main poll's rule (`pollCadence`) for the
 * smaller polls that used to run flat out in a minimised window.
 *
 * The interval keeps ticking at `tickMs` either way; a tick that is not due costs
 * nothing, so the visible cadence returns on its own. Showing the window polls at
 * once when the last run is older than a tick, so a covered window does not show
 * stale data for a further tick on return.
 *
 * Does not run `run` on start: callers already make their own leading call (and
 * some, like the file watch, deliberately delay it). `performance.now`, not
 * `Date.now`, so a stepped wall clock cannot hold polls off.
 *
 * Returns the cleanup.
 */
export function gatedInterval(run: () => void, tickMs: number): () => void {
  let last = performance.now();
  const fire = () => {
    last = performance.now();
    run();
  };
  const timer = window.setInterval(() => {
    if (pollDue(document.hidden, performance.now() - last, tickMs)) fire();
  }, tickMs);
  const onVisible = () => {
    if (!document.hidden && pollOnShow(performance.now() - last, tickMs)) fire();
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => {
    window.clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisible);
  };
}
