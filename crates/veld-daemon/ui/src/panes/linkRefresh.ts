/**
 * Makes xterm look for links again on the row under the pointer after that row
 * changed. A workaround for an xterm bug, to be deleted once xterm fixes it.
 *
 * # The bug
 *
 * xterm's `Linkifier` asks the link providers about a row when the pointer moves
 * onto it, and keeps the answer until the pointer moves to *another* row. Nothing
 * clears that answer when the row's content changes, unless a link was already
 * found there. So a row that said "no link" while the pointer was on it keeps
 * saying so after a link is drawn into it: no underline, and a click does nothing
 * until the pointer leaves the row. Coding agents redraw the bottom of the screen
 * in place, so the row with a PR link was usually a spinner a moment ago, and
 * the bug is common.
 *
 * Leaving the terminal does not clear the answer either. Coming back to a row
 * that changed while the pointer was away gets the stale answer, which opens the
 * link that *used* to be there when that row had one.
 *
 * Both reproduce on xterm 5.5.0 (what Veld ships) and 6.0.0.
 *
 * Upstream: issue https://github.com/xtermjs/xterm.js/issues/4323. The open PR
 * https://github.com/xtermjs/xterm.js/pull/6141 fixes the stationary-pointer
 * case; the leave-and-return case also needs `mouseleave` to reset the cached
 * row, proposed on that PR. Once Veld is on an xterm release with both, delete
 * this file and `linkRefresh.test.ts`, and in `terminalHost.ts` the import, the
 * `loadAddon` call that installs it, and the `isLinkRefreshProbe` check in the
 * file-path link's `hover`.
 *
 * # The workaround
 *
 * The cache is keyed by row, so moving the pointer to another row and back is
 * what makes xterm ask again. This sends exactly that, as two synthetic
 * `mousemove`s, after the row under the pointer is redrawn and when the pointer
 * enters the terminal. It relies on DOM events, public API and one class name,
 * `.xterm-screen`; if an xterm upgrade renames that, the addon warns once and
 * does nothing.
 *
 * The events are dispatched on `.xterm-screen` and do not bubble. That element
 * is where the `Linkifier` listens. xterm's mouse *reporting* listens on the
 * outer `.xterm` element, so the program in the terminal never sees them.
 *
 * The first move visits a neighbouring row only to reset the cache, but xterm
 * still calls `hover` on a link there. A provider whose `hover` has effects of
 * its own skips events for which [`isLinkRefreshProbe`] is true.
 */
import type { IDisposable, ITerminalAddon, Terminal } from "@xterm/xterm";

/**
 * How long to gather redraws before asking again. A throttle rather than a
 * debounce: streaming output redraws more often than this, and a debounce would
 * never fire while it does.
 */
export const LINK_REFRESH_MS = 50;

const probes = new WeakSet<Event>();

/** Whether `event` is the addon's move to a neighbouring row, not where the pointer is. */
export function isLinkRefreshProbe(event: Event): boolean {
  return probes.has(event);
}

export class LinkRefreshAddon implements ITerminalAddon {
  private readonly subs: IDisposable[] = [];
  private timer: ReturnType<typeof setTimeout> | undefined;

  activate(term: Terminal): void {
    // Loaded after `term.open()`, so the element exists.
    const screen = term.element?.querySelector<HTMLElement>(".xterm-screen");
    if (!screen) {
      console.warn("LinkRefreshAddon: no .xterm-screen, links will not refresh (xterm.js#4323)");
      return;
    }
    const win = screen.ownerDocument.defaultView ?? window;

    let inside = false;
    let x = 0;
    let y = 0;
    let buttons = 0;
    let nudging = false;
    // A refresh was due while a button was down; owed once it comes up.
    let owed = false;
    // Viewport rows redrawn since the last refresh, as xterm reports them.
    let dirtyStart = Number.POSITIVE_INFINITY;
    let dirtyEnd = Number.NEGATIVE_INFINITY;

    const pointerRow = (rect: DOMRect): number | undefined => {
      const h = rect.height / term.rows;
      if (!(h > 0)) return undefined;
      const row = Math.floor((y - rect.top) / h);
      return row >= 0 && row < term.rows ? row : undefined;
    };

    const nudge = (): void => {
      if (!inside || term.rows < 2) return;
      // A refresh between mousedown and mouseup replaces xterm's link object, and
      // xterm then drops the click; so not while a button is down, but afterwards.
      if (buttons !== 0) {
        owed = true;
        return;
      }
      owed = false;
      const rect = screen.getBoundingClientRect();
      if (pointerRow(rect) === undefined) return;
      const h = rect.height / term.rows;
      const away = new MouseEvent("mousemove", {
        clientX: x,
        clientY: y - h >= rect.top ? y - h : y + h,
        bubbles: false,
      });
      probes.add(away);
      nudging = true;
      try {
        screen.dispatchEvent(away);
        screen.dispatchEvent(new MouseEvent("mousemove", { clientX: x, clientY: y, bubbles: false }));
      } finally {
        nudging = false;
      }
    };

    const track = (e: MouseEvent): void => {
      if (nudging) return;
      inside = true;
      x = e.clientX;
      y = e.clientY;
      buttons = e.buttons;
    };
    const onEnter = (e: MouseEvent): void => {
      track(e);
      nudge();
    };
    const onLeave = (): void => {
      inside = false;
    };
    const onDown = (e: MouseEvent): void => {
      buttons = e.buttons;
    };
    // A terminal taken out of the document gets no `mouseleave`, so `inside` and
    // the position would outlive the pointer. Any real move or release elsewhere in
    // the window settles it. The addon's own moves do not bubble, so never get here.
    const outside = (e: MouseEvent): boolean => !(e.target instanceof Node && screen.contains(e.target));
    // On the window, since the button can come up outside the terminal. Runs after
    // xterm's own mouseup on the screen has activated any link.
    const onUp = (e: MouseEvent): void => {
      buttons = e.buttons;
      if (outside(e)) inside = false;
      if (owed && buttons === 0) nudge();
    };
    const onWindowMove = (e: MouseEvent): void => {
      if (outside(e)) inside = false;
    };
    screen.addEventListener("mousemove", track);
    screen.addEventListener("mouseenter", onEnter);
    screen.addEventListener("mouseleave", onLeave);
    screen.addEventListener("mousedown", onDown);
    win.addEventListener("mouseup", onUp);
    win.addEventListener("mousemove", onWindowMove, { passive: true });
    this.subs.push({
      dispose: () => {
        screen.removeEventListener("mousemove", track);
        screen.removeEventListener("mouseenter", onEnter);
        screen.removeEventListener("mouseleave", onLeave);
        screen.removeEventListener("mousedown", onDown);
        win.removeEventListener("mouseup", onUp);
        win.removeEventListener("mousemove", onWindowMove);
      },
    });

    this.subs.push(
      term.onRender(({ start, end }) => {
        if (!inside) return;
        dirtyStart = Math.min(dirtyStart, start);
        dirtyEnd = Math.max(dirtyEnd, end);
        if (this.timer !== undefined) return;
        this.timer = setTimeout(() => {
          this.timer = undefined;
          const from = dirtyStart;
          const to = dirtyEnd;
          dirtyStart = Number.POSITIVE_INFINITY;
          dirtyEnd = Number.NEGATIVE_INFINITY;
          const row = pointerRow(screen.getBoundingClientRect());
          if (row !== undefined && row >= from && row <= to) nudge();
        }, LINK_REFRESH_MS);
      }),
    );
  }

  dispose(): void {
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
    for (const d of this.subs.splice(0)) d.dispose();
  }
}
