import { useCallback, useEffect, useRef } from "react";

/**
 * The rail's drag substrate: press, move, release — not HTML5 drag-and-drop.
 *
 * # Why not `draggable`
 *
 * The rail carried three native drags (worktree rows, lane headers, project
 * squares) and paid for each of them in workarounds that are all the same
 * complaint: **HTML5 drag-and-drop hides the pointer from the page**. What it
 * offers instead is a per-element `dragover` protocol, and every question the
 * rail actually had to answer was a question about a *point*.
 *
 * Three costs, each of which was a comment in `App.tsx` before this existed:
 *
 * - **`dragend` fires on the source node.** A source unmounted mid-drag — the
 *   5s poll, a lane renamed in another window — took the only event that ends
 *   the gesture with it, stranding the drag state and leaving the rail with a
 *   live drop zone nobody was aiming at. The rail carried a window-level
 *   `dragend`/`drop` listener purely to survive that. A pointer drag cannot
 *   reach the same state: `pointerup` is delivered to the window whatever
 *   happened to the element it started on.
 * - **There is no way to ask "would anything here take this".** `preventDefault`
 *   is the only way to accept a drop, so the rail adopted *defaultPrevented as a
 *   claim* between its own handlers and mounted a second window listener to
 *   retract a highlight the pointer had left. Resolving from a point makes "over
 *   nothing" the ordinary answer instead of an event that has to be inferred.
 * - **No payload, no drag.** Firefox ignores a native drag that sets no
 *   `dataTransfer` data, so three `setData` calls existed to satisfy a browser
 *   rather than to carry anything: nothing in the app ever read them back.
 *
 * # What this owes the thing it replaces
 *
 * Three behaviours came free with a native drag and are re-implemented here,
 * because losing them silently is how a rewrite regresses:
 *
 * - **Autoscroll.** A native drag scrolls a scrollable container when the
 *   pointer nears its edge. Nothing does that for a pointer drag, and the rail
 *   is a list long enough to scroll — see `scroller`.
 * - **No click after a drag.** Chromium emits no `click` following a native
 *   drop, and the rail's lane header leans on it: the bar that reorders lanes is
 *   also the bar that folds the section. A pointer drag ends in an ordinary
 *   `pointerup` and therefore an ordinary `click`, so this swallows exactly one
 *   — centrally, in the capture phase, rather than asking every handler on three
 *   surfaces to remember a guard it cannot be compiled into remembering.
 * - **A drag image.** The caret and the wash say where the thing will *land*,
 *   which is a different question from what is in your hand — and a drag with no
 *   answer to the second one is a cursor travelling across an unchanged page. So
 *   the source element is cloned and flown under the cursor, at its own size,
 *   which is what the native drag image was. See `mountGhost`.
 *
 * # Native views, and why nothing is done about them
 *
 * Pointer capture does not survive an embedded browser pane. A `WebContentsView`
 * is an OS-level child window rather than a DOM node, so once the cursor is over
 * one it takes the mouse: the renderer stops seeing `pointermove`, the ghost is
 * painted over, and the release goes to the pane instead of here. The drag
 * therefore ends the moment a pane takes focus — `blur` reaches `onAbort`, and
 * the gesture cancels. Which is what a release outside a drop area is supposed
 * to do anyway: every drop target the rail has is *in* the rail, so a pointer
 * out over a pane was never going to commit anything.
 *
 * The alternative was tried and taken back out rather than missed. `PaneArea`'s
 * splitter hides the views for its duration (`pushBrowserSuspend`), and a rail
 * drag can do the same; it keeps the ghost visible out over the panes. It also
 * freezes every browser pane in the window to a still on every row, lane and
 * square drag — a very common gesture — to buy feedback on a path with no
 * destination. Not worth it. If the ghost vanishing at the rail's edge ever
 * reads as broken, that is the lever to pull.
 */

/**
 * How far the pointer must travel before a press counts as a drag.
 *
 * A press that never crosses it is a click and nothing here is told about it —
 * which is what lets a row be *selected* by the same gesture that moves it. Four
 * pixels is the usual figure and it is deliberately small: the rail's rows are
 * 28px, so a threshold generous enough to be felt would be a threshold that eats
 * the first third of a short drag.
 */
export const DRAG_THRESHOLD = 4;

/** The band at a scroller's top and bottom edge where a drag scrolls it. */
export const AUTOSCROLL_BAND = 28;

/** Pixels per frame at the very edge of the band — roughly 840px/s at 60fps. */
export const AUTOSCROLL_MAX = 14;

/** Whether a press that started at the origin has travelled far enough to be a
 *  drag. Distance, not per-axis: a diagonal 3px+3px move is 4.2px of travel and
 *  reads as deliberate, which axis-wise tests would both miss. */
export function beyondThreshold(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) >= DRAG_THRESHOLD;
}

/**
 * How far to scroll a container this frame, for a drag whose pointer is at
 * `clientY`. Negative scrolls up, positive down, `0` leaves it alone.
 *
 * Ramped rather than constant: the speed rises with how far into the band the
 * pointer is, so the edge of the list nudges and the edge of the *screen* moves
 * properly. Past the container entirely the depth clamps, so dragging far above
 * a list scrolls at the top speed rather than at an ever-increasing one.
 *
 * The rounding floor of 1 matters: without it the outermost pixels of the band
 * round to zero and the band has a dead rim exactly where a drag first enters
 * it, which reads as autoscroll being broken rather than as it being gentle.
 *
 * A container shorter than two bands has them overlap; the deeper side wins,
 * which keeps a short list scrollable in both directions instead of always up.
 */
export function autoScrollVelocity(
  top: number,
  bottom: number,
  clientY: number,
): number {
  const up = top + AUTOSCROLL_BAND - clientY;
  const down = clientY - (bottom - AUTOSCROLL_BAND);
  if (up <= 0 && down <= 0) return 0;
  const depth = Math.min(Math.max(up, down), AUTOSCROLL_BAND);
  const speed = Math.max(1, Math.round((AUTOSCROLL_MAX * depth) / AUTOSCROLL_BAND));
  return up > down ? -speed : speed;
}

/**
 * Properties the ghost has to be handed, because it hangs off `<body>` and the
 * cascade that dressed the original does not reach it there.
 *
 * Short on purpose. Everything that makes a row look like a row is matched by a
 * plain class selector (`.wt-row`, `.lane-head`, `.project-sq` — none of them
 * scoped to an ancestor), so those follow the clone wherever it goes. The colour
 * tokens do not, quite: the light theme redefines them on `body[data-theme]`
 * rather than on `:root`, so they reach the clone because it is parented to
 * `<body>` — one more reason `mountGhost` puts it exactly there. What does not
 * follow either way is the *inherited* text properties, which the original gets
 * from somewhere up inside the rail. Setting those on the clone's root covers
 * its whole subtree — that is what inheritance means.
 */
const GHOST_INHERITED = [
  "color",
  "font-family",
  "font-size",
  "font-style",
  "font-weight",
  "letter-spacing",
  "line-height",
  "text-align",
  "text-transform",
] as const;

/**
 * Whether a computed `background-color` lets what is behind it show through.
 *
 * The alpha is the whole question, and it is asked twice because Chromium has
 * two ways of writing one. A legacy colour serialises as `rgb(r, g, b)` when it
 * is opaque and `rgba(r, g, b, a)` when it is not — comma-separated, alpha last.
 * Anything that went through `color-mix()` or a modern colour space does not:
 * measured, `color-mix(in oklab, … , transparent)` computes to
 * `oklab(L a b / 0.09)` and the `in srgb` form to `color(srgb r g b / 0.88)`,
 * both of which omit the slash entirely when opaque.
 *
 * Reading only the first form was the bug this replaces: the stylesheet already
 * uses `color-mix` on a rail selector, and an `oklab(… / 0.09)` matched nothing,
 * so a 9%-alpha background was read as fully opaque — which is the one answer
 * that makes the ghost unreadable rather than merely wrong.
 */
export function seeThrough(color: string): boolean {
  if (color === "transparent" || color === "") return true;
  const legacy =
    /^rgba\(\s*[\d.]+\s*,\s*[\d.]+\s*,\s*[\d.]+\s*,\s*([\d.]+)\s*\)$/.exec(
      color,
    );
  if (legacy) return Number(legacy[1]) < 1;
  // `oklab(… / a)`, `oklch`, `lab`, `lch`, `color(srgb … / a)`, `hsl(… / a)` —
  // every modern form puts the alpha after a slash, as a number or a percentage.
  const slash = /\/\s*([\d.]+)(%?)\s*\)$/.exec(color);
  if (slash) return Number(slash[1]) < (slash[2] ? 100 : 1);
  return false;
}

/**
 * The colour of the surface `el` is sitting on: the nearest ancestor that
 * actually paints something.
 *
 * A rail row's own background is transparent — it borrows the rail's — and the
 * clone leaves the rail behind. Flying over a terminal that is the one thing the
 * native drag image never was: dark text on a dark page. So the ghost is handed
 * the surface it was lifted off, and carries it like a card.
 */
function surfaceUnder(el: HTMLElement): string {
  for (let n = el.parentElement; n; n = n.parentElement) {
    const bg = getComputedStyle(n).backgroundColor;
    if (!seeThrough(bg)) return bg;
  }
  return "";
}

/**
 * The copy of the dragged element that flies under the cursor.
 *
 * A clone rather than a rendering the caller describes: three surfaces would each
 * need a second version of themselves kept in step with the first, and this way
 * there is nothing to keep in step — a photograph cannot drift from its subject.
 * It is taken before `onBegin`, so it catches the source *un*faded; the fade is a
 * React class that only lands on the following render.
 *
 * Parked on `<body>` because `position: fixed` is fixed to the viewport only
 * while no ancestor carries a `transform`, `filter` or `will-change`, and a
 * clone left inside the rail would be one refactor away from being positioned
 * against a box that moves. Mounting is this function's job rather than the
 * caller's, because the last decision it makes has to be read back off the live
 * element — see the backing colour below.
 */
function mountGhost(el: HTMLElement, rect: DOMRect): HTMLElement {
  const ghost = el.cloneNode(true) as HTMLElement;
  ghost.classList.add("drag-ghost");
  // Duplicated ids would quietly steal every `aria-labelledby` and `for=` aimed
  // at the original, and there is nothing here for a screen reader to read
  // anyway: the row it was copied from is still in the tree, still labelled.
  ghost.removeAttribute("id");
  ghost.querySelectorAll("[id]").forEach((n) => n.removeAttribute("id"));
  ghost.setAttribute("aria-hidden", "true");
  const cs = getComputedStyle(el);
  for (const prop of GHOST_INHERITED) {
    ghost.style.setProperty(prop, cs.getPropertyValue(prop));
  }
  // Inline rather than in `.drag-ghost`: these have to beat whatever the
  // element's own rules say about where it sits and how big it is, and the class
  // is a single one competing with selectors that may carry more.
  ghost.style.position = "fixed";
  ghost.style.left = "0";
  ghost.style.top = "0";
  ghost.style.margin = "0";
  ghost.style.boxSizing = "border-box";
  ghost.style.width = `${rect.width}px`;
  ghost.style.height = `${rect.height}px`;
  document.body.appendChild(ghost);
  // Asked of the clone and not of the original, and only once it is mounted,
  // because they do not agree: the source is under the pointer and therefore
  // `:hover`, which paints it. The copy is not hovered by anything and never
  // will be, so this is the only way to learn what it will actually render as.
  if (seeThrough(getComputedStyle(ghost).backgroundColor)) {
    ghost.style.backgroundColor = surfaceUnder(el);
  }
  return ghost;
}

/**
 * Move the ghost so its top-left is at `x`,`y`.
 *
 * `transform` and not `left`/`top`: this runs on every `pointermove` and the
 * transform is composited, so the ghost moves without laying the page out again.
 * Rounded because a row is mostly text and a half-pixel offset renders it blurry.
 */
function placeGhost(ghost: HTMLElement, x: number, y: number): void {
  ghost.style.transform = `translate3d(${Math.round(x)}px, ${Math.round(y)}px, 0)`;
}

/**
 * Capture phase for the drag's Escape listener, and window's capture phase runs
 * before anything else in the document. Measured: a project square carries a
 * Mantine `Tooltip`, the tooltip is open exactly when you are dragging the
 * square, and Floating UI's dismiss-on-Escape swallows the keydown at capture —
 * so a bubble-phase listener here never saw it, and Escape silently committed
 * the drag instead of cancelling it. Whatever else wants Escape, a drag in
 * flight wants it more.
 */
const KEY_OPTS = { capture: true } as const;

/** What the caller is told about the drag it asked for. */
export type PointerDragSpec<S> = {
  /** The press became a drag. Fires once, when the threshold is crossed. */
  onBegin: (source: S) => void;
  /** The pointer moved while dragging — or the autoscroll moved the content
   *  under a pointer that did not. */
  onMove: (source: S, x: number, y: number) => void;
  /** Released. The target is the caller's to resolve from the coordinates, the
   *  same way `onMove` did, so what is committed is what was last shown. */
  onDrop: (source: S, x: number, y: number) => void;
  /** Escape, a cancelled pointer, or the window losing focus. Never fires for a
   *  press that stayed under the threshold. */
  onCancel: () => void;
  /** The element to scroll while the pointer sits near its top or bottom edge,
   *  if any — and only while the pointer is horizontally inside it. */
  scroller?: () => HTMLElement | null;
};

type Session<S> = {
  pointerId: number;
  source: S;
  startX: number;
  startY: number;
  /** The element the press landed on, and the one that takes pointer capture
   *  once this is a drag. Held rather than read back off the event: React
   *  clears `currentTarget` as soon as the handler returns. */
  el: HTMLElement;
  /** Whether the threshold has been crossed. Until it has, this is still a
   *  click and the caller has been told nothing. */
  active: boolean;
  /** Where the pointer was last seen, for the autoscroll frame loop. */
  x: number;
  y: number;
  frame: number | null;
  /** The flying copy of `el`, once this is a drag. */
  ghost: HTMLElement | null;
  /** Where inside `el` the press landed, so the ghost keeps the grab point: the
   *  cursor stays on the part of the row it took hold of, and the copy lifts off
   *  the original instead of jumping to meet the pointer. */
  offX: number;
  offY: number;
};

/**
 * A press-move-release drag on any number of elements.
 *
 * `start` goes on the source's `onPointerDown` along with what is being carried;
 * everything after that is on the window, so the drag survives the pointer
 * leaving the element, the element unmounting, and the list reflowing under it.
 *
 * Capture is taken **at the threshold, not at the press**. That ordering is what
 * lets a row full of nested buttons be draggable at all: capture retargets the
 * `pointerup` and with it the `click`, so capturing on `pointerdown` would break
 * every ▶ and ⋮ inside the things being dragged. By the time capture is taken
 * the click is being swallowed anyway.
 */
export function usePointerDrag<S>(spec: PointerDragSpec<S>): {
  start: (e: React.PointerEvent, source: S) => void;
} {
  // Read through a ref, so a drag in flight sees this render's handlers — they
  // close over `groups`, `props.lanes` and the rest, which change under it.
  const specRef = useRef(spec);
  specRef.current = spec;
  const session = useRef<Session<S> | null>(null);
  // Forward reference: the listeners below end the drag, and `end` removes the
  // listeners. Every function here is built once (`useCallback` with no
  // dependencies) and reads its moving parts out of refs — which is required
  // rather than tidy, because `removeEventListener` has to be handed the same
  // function object `addEventListener` was.
  const endRef = useRef<(commit: boolean, stillDown?: boolean) => void>(
    () => {},
  );

  const frameStep = useCallback(() => {
    const s = session.current;
    if (!s || !s.active) return;
    const scroller = specRef.current.scroller?.();
    if (scroller) {
      const box = scroller.getBoundingClientRect();
      // Horizontally inside, or not at all. Vertically the velocity deliberately
      // survives overshooting the edge — that is how you keep scrolling once the
      // pointer has run past the last row — but a drag carried sideways out of
      // the column entirely is not asking it to scroll, and a list that ran to
      // its end while the pointer was somewhere else would have moved every
      // target out from under the gesture that came back.
      const v =
        s.x < box.left || s.x > box.right
          ? 0
          : autoScrollVelocity(box.top, box.bottom, s.y);
      if (v !== 0) {
        const before = scroller.scrollTop;
        scroller.scrollTop = before + v;
        // Only when the content actually moved. At either end of the scroll
        // range the velocity is still non-zero, and re-resolving there would be
        // a target recomputed sixty times a second for a list that is not
        // moving.
        if (scroller.scrollTop !== before) {
          specRef.current.onMove(s.source, s.x, s.y);
        }
      }
    }
    s.frame = requestAnimationFrame(frameStep);
  }, []);

  const onMove = useCallback(
    (e: PointerEvent) => {
      const s = session.current;
      if (!s || e.pointerId !== s.pointerId) return;
      s.x = e.clientX;
      s.y = e.clientY;
      if (!s.active) {
        if (!beyondThreshold(e.clientX - s.startX, e.clientY - s.startY)) return;
        s.active = true;
        // Capture keeps the stream coming once the pointer leaves the source,
        // which it does immediately — the whole gesture is about aiming
        // somewhere else. Guarded on `isConnected` because a refresh between
        // the press and the first move can have unmounted the row already, and
        // capturing on a detached element throws.
        if (s.el.isConnected) s.el.setPointerCapture(s.pointerId);
        // Photograph before `onBegin` fades the source: the ghost is what the
        // row looked like when it was picked up, not what it looks like now that
        // it is a hole in the list.
        const rect = s.el.getBoundingClientRect();
        s.offX = s.startX - rect.left;
        s.offY = s.startY - rect.top;
        s.ghost = mountGhost(s.el, rect);
        document.body.classList.add("pointer-dragging");
        specRef.current.onBegin(s.source);
        s.frame = requestAnimationFrame(frameStep);
      }
      // Placed here rather than at creation so there is one line that does it.
      // The ghost is appended untranslated a few statements above, which is
      // invisible: nothing can paint between there and here.
      if (s.ghost) placeGhost(s.ghost, e.clientX - s.offX, e.clientY - s.offY);
      specRef.current.onMove(s.source, e.clientX, e.clientY);
    },
    [frameStep],
  );

  const onUp = useCallback((e: PointerEvent) => {
    const s = session.current;
    if (!s || e.pointerId !== s.pointerId) return;
    s.x = e.clientX;
    s.y = e.clientY;
    endRef.current(true);
  }, []);

  const onAbort = useCallback(() => endRef.current(false), []);

  const onKey = useCallback((e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    // Only once this is a drag. Below the threshold there is nothing to cancel,
    // and Escape belongs to whatever else is listening for it.
    if (!session.current?.active) return;
    e.preventDefault();
    e.stopPropagation();
    // The button is still held — Escape is a key, not a release — which is what
    // the second argument is for. See the click swallower in `end`.
    endRef.current(false, true);
  }, []);

  const end = useCallback(
    (commit: boolean, stillDown = false) => {
      const s = session.current;
      if (!s) return;
      session.current = null;
      if (s.frame !== null) cancelAnimationFrame(s.frame);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onAbort);
      window.removeEventListener("keydown", onKey, KEY_OPTS);
      window.removeEventListener("blur", onAbort);
      // A press that never crossed the threshold was a click. Nothing was told
      // a drag existed, so nothing is told it ended.
      if (!s.active) return;
      document.body.classList.remove("pointer-dragging");
      s.ghost?.remove();
      if (s.el.hasPointerCapture(s.pointerId)) {
        s.el.releasePointerCapture(s.pointerId);
      }
      // The click this release produces belongs to the drag, not to whatever is
      // under the pointer — a lane header is also its section's fold toggle, and
      // a row is its own selection. Capture phase, so it never reaches a
      // handler, and one-shot, because a drag released over nothing clickable
      // produces no click at all and the listener must not outlive the gesture.
      const swallow = (ev: MouseEvent) => {
        ev.stopPropagation();
        ev.preventDefault();
        disarm();
      };
      const disarm = () => {
        window.removeEventListener("click", swallow, true);
        window.removeEventListener("pointerup", released, true);
        window.removeEventListener("pointercancel", disarm, true);
      };
      // `pointerup` runs before `click`, so the disarm waits a turn — otherwise
      // it removes the swallower just in time for the click to get through.
      const released = () => setTimeout(disarm, 0);
      window.addEventListener("click", swallow, true);
      if (stillDown) {
        // Escape, with the button still held. The click that must not happen has
        // not been produced yet: it arrives whenever the user lets go, which is
        // arbitrarily later than a timer would have fired. Measured — Escape and
        // then a release over the source row selected that worktree and folded
        // its lane, which is exactly what cancelling was supposed to prevent.
        window.addEventListener("pointerup", released, true);
        // A release is not guaranteed even so: the pointer can be cancelled, or
        // the window can take the gesture away. Either way no click follows, and
        // a swallower left armed would eat an unrelated one later.
        window.addEventListener("pointercancel", disarm, true);
      } else {
        // The release already happened — on a drop it is what got us here, and
        // on a `blur` or a cancelled pointer it went somewhere this window will
        // never hear about. Either way the click, if there is one, is the very
        // next thing to run.
        released();
      }
      if (commit) specRef.current.onDrop(s.source, s.x, s.y);
      else specRef.current.onCancel();
    },
    [onMove, onUp, onAbort, onKey],
  );
  endRef.current = end;

  // A component unmounted mid-drag would otherwise leave the window listeners,
  // the frame loop and the body class behind.
  useEffect(() => () => endRef.current(false), []);

  const start = useCallback(
    (e: React.PointerEvent, source: S) => {
      // Primary button only. The secondary one opens the context menu the rail's
      // sections and rows already have, and a drag armed underneath it would be
      // in flight while the user reads the menu.
      if (e.button !== 0 || session.current) return;
      session.current = {
        pointerId: e.pointerId,
        source,
        startX: e.clientX,
        startY: e.clientY,
        el: e.currentTarget as HTMLElement,
        active: false,
        x: e.clientX,
        y: e.clientY,
        frame: null,
        ghost: null,
        offX: 0,
        offY: 0,
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onAbort);
      window.addEventListener("keydown", onKey, KEY_OPTS);
      // A window that loses focus mid-drag stops being told where the pointer
      // is, so the honest end is here rather than on a target the user can no
      // longer see themselves aiming at.
      window.addEventListener("blur", onAbort);
    },
    [onMove, onUp, onAbort, onKey],
  );

  return { start };
}
