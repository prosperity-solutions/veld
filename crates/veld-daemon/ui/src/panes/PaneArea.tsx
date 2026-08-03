/**
 * The dock region of IDE mode: two tab hosts side by side with a draggable
 * split. See `model.ts` for why this replaced the fixed columns.
 *
 * Adding a content type means (the embedded browser and the run's diagnostics both
 * landed this way, as tabs here rather than as new columns):
 *
 * 1. `PANE_KINDS` in `model.ts` — also what validates a restored layout, so a
 *    kind missing from it works until the first reload and then vanishes.
 * 2. `paneTabLabel` in `model.ts` — returns `string`, so a missing case is a
 *    compile error. This is the one the compiler catches for you.
 * 3. `tabIcon` below — its `default` exists to make a missing case a compile
 *    error too, because `React.ReactNode` includes `undefined` and tsc would
 *    otherwise let a new kind render with no glyph.
 * 4. The `active?.kind === …` branches in `DockView`'s body.
 * 5. The `+` menu beside them, if the kind should be openable.
 * 6. `tabMenu` in this file — right-click actions branch on `tab.kind`, a
 *    different variable from (4), so grepping for one pattern misses the other.
 * 7. The ⌘K palette in `App.tsx` — `focused?.kind === …` gates the per-kind
 *    commands. Additive, so skipping it breaks nothing; it just leaves the kind
 *    unreachable from the keyboard.
 * 8. **Where its data comes from**, if it needs any. A pane renders inside the
 *    dock and has no access to the app's state, so anything server-backed arrives
 *    through `RunPaneContext` (`panes/RunPanes.tsx`), which `App.tsx` builds as
 *    `runCtx` and threads down. `logs` and `nodes` are the two examples: both are
 *    thin wrappers whose whole job is to resolve that context, and adding a field
 *    to it means touching both ends. A kind that needs *different* data
 *    (environment variables, say) adds it there rather than fetching on its own —
 *    a pane that polls is a pane that keeps polling while nobody is looking at it.
 * 9. `DiagKind` in `model.ts`, if it is a run view — `diagTab` and the chooser's
 *    `onDiag` are typed by it, so a kind that is one has to be in that subset.
 * 10. **`PANE_KINDS` in `desktop/src/validate.js`** — a second, hand-maintained
 *    copy, because a tab now crosses into the Electron main process when a pane
 *    is detached into its own window. A kind missing there works everywhere
 *    except detach, which refuses with "the desktop shell refused the request"
 *    and no hint where to look. Guarded by a drift-gate test in
 *    `desktop/src/validate.test.js` ("PANE_KINDS agrees with the renderer's"),
 *    which is why this one is enforced despite living in another language, in
 *    another package, with no shared type between them.
 * 11. `releaseForTransfer` below, **if the kind owns anything outside the
 *    layout** — a live registry the way `terminal` and `browser` do. Removing a
 *    tab from a layout is what destroys its resource (`pruneTerminals` /
 *    `pruneBrowsers` collect against the layouts), so a detach has to let go
 *    first or it kills the thing it is moving. Its `switch` is exhaustive, so
 *    this one is a compile error rather than a checklist item you can miss.
 *
 * Only 1-3, 9, 10 and 11 are enforced. Note what is *not* a kind: the run's
 * URLs, which are a launcher shown inside a pane rather than a pane of their own
 * (`VeldLinks.tsx`).
 */

import { ActionIcon, Menu } from "@mantine/core";
import {
  IconActivityHeartbeat,
  IconArrowsExchange,
  IconExternalLink,
  IconLayoutColumns,
  IconLogs,
  IconPlus,
  IconRefresh,
  IconTerminal2,
  IconWorld,
  IconX,
} from "@tabler/icons-react";
import { useContextMenu } from "mantine-contextmenu";
import { Fragment, useCallback, useEffect, useReducer, useRef, useState } from "react";
import { BrowserPane, browserTabDot } from "./BrowserPane";
import { LogsPane, NodesPane, type RunPaneContext } from "./RunPanes";
import { VeldLinks } from "./VeldLinks";
import { popBrowserSuspend, pushBrowserSuspend, reloadBrowser } from "./browserHost";
import {
  type BrowserProfile,
  DEFAULT_RATIO,
  type DiagKind,
  type DockIndex,
  MAX_RATIO,
  MIN_RATIO,
  type PaneLayout,
  type PaneLayoutUpdate,
  type PaneTab,
  activateTab,
  activeTab,
  addTab,
  addTabToFocused,
  browserTab,
  closeTab,
  diagTab,
  dockOf,
  dockVisible,
  focusDock,
  insertTab,
  moveTab,
  moveTabToOtherDock,
  newPaneTab,
  newTabId,
  paneTabLabel,
  parseTransferTabs,
  replaceTab,
  setRatio,
  splitWithTab,
  updateTab,
} from "./model";
import { notifyError } from "../shared/notify";
import { desktopWindow } from "../shell";
import { type DropZone, sameZone, zoneAt } from "./dropModel";

/**
 * Drag payload type for a pane tab.
 *
 * A custom MIME type, not `text/plain`: it keeps a tab drag from being accepted
 * by unrelated drop targets (and a dragged file or selection from being
 * accepted here), because `dragover` can only inspect types, never data.
 */
const TAB_MIME = "application/x-veld-pane-tab";
import {
  focusTerminal,
  mountTerminal,
  reconnectTerminal,
  releaseTerminal,
  restartTerminal,
  subscribeTerminal,
  terminalStatus,
  unmountTerminal,
} from "./terminalHost";

/**
 * One embedded-browser suspend for the whole tab-drag gesture.
 *
 * Under Electron a browser pane is a native view that owns every event inside
 * its own rect, so a drop target *behind* one never sees `dragover` — the pane
 * body is exactly where the new edge zones live, and without this dropping a tab
 * onto a preview would silently do nothing. The splitter drag has the same
 * problem and the same fix (`onSplitterDown`); this is that pattern for a
 * gesture whose start and end are on different elements.
 *
 * Module scope, not component state, because a drag *moves* tabs between docks:
 * the element that started it can be unmounted by the drop's own re-render
 * before `dragend` reaches it. Every path that ends a drag — our drops, the
 * detach, `dragend` — calls `endTabDrag`, and it is idempotent so the first one
 * to arrive wins.
 */
let tabDragSuspended = false;
function beginTabDrag(): void {
  if (tabDragSuspended) return;
  tabDragSuspended = true;
  pushBrowserSuspend();
}
function endTabDrag(): void {
  if (!tabDragSuspended) return;
  tabDragSuspended = false;
  popBrowserSuspend();
}

/**
 * The same gesture, announced to the shell so it reaches the *other* windows.
 *
 * They have no drag events of their own — a drag never leaves the document it
 * started in — so without this they keep their native views painting over any
 * overlay, and never learn the pointer is above them. The shell broadcasts the
 * start and then carries the cursor to whichever window it is over.
 */
function beginTabDragEverywhere(): void {
  beginTabDrag();
  void desktopWindow?.dragBegin().catch(() => {});
}
function endTabDragEverywhere(): void {
  endTabDrag();
  void desktopWindow?.dragEnd().catch(() => {});
}

/**
 * Let go of whatever a tab owns outside the layout, **without destroying it**,
 * because it is about to exist in another window instead.
 *
 * A `switch` with an exhaustive `default` rather than
 * `if (kind === "terminal")`, and that is the whole point of the function. The
 * rule it enforces is the trap in this file: `pruneTerminals`/`pruneBrowsers`
 * collect against the layouts, so *removing a tab is what destroys its
 * resource* — a future pane kind with a live registry of its own (see the
 * `PaneKind` docs in `model.ts`, which invite exactly that) would have its
 * resource killed by a detach, silently, by a code path that never mentions it.
 * Written this way, adding a kind is a compile error here instead.
 */
function releaseForTransfer(tab: PaneTab): void {
  switch (tab.kind) {
    case "terminal":
      // The shell is the daemon's and outlives this page; the new window
      // re-attaches by id. `disposeTerminal` would `DELETE` the session.
      releaseTerminal(tab.id);
      return;
    case "browser":
      // Deliberately nothing. A `WebContentsView` belongs to a window and cannot
      // be re-parented, so a detach *is* a destroy-and-recreate: letting
      // `pruneBrowsers` destroy this one is the intended half of it, and the tab
      // record carries everything the new window rebuilds from.
      return;
    case "logs":
    case "nodes":
    case "new":
      // Pure React; they own nothing outside the layout.
      return;
    default:
      return unhandledKind(tab.kind);
  }
}

export function PaneArea(props: {
  layout: PaneLayout;
  onLayout: (next: PaneLayoutUpdate) => void;
  /** Which worktree's terminals these are. */
  worktreeId: number;
  /** The worktree's repository root — what a detached window needs in its
   *  `?repo=` so it resolves the same selection this one is showing. */
  repoRoot: string;
  /** The run's live URLs, offered by every pane that has nothing in it yet. */
  serviceUrls: Array<[string, string]>;
  /** Why there are none — only the app knows (no run, or no veld.json). */
  urlsEmptyHint: string;
  /** Browser sessions: the set that exists for this worktree, and how to add or
   *  remove one. */
  sessions: BrowserProfile[];
  onAddSession: ((tabId: string) => void) | undefined;
  onRemoveSession: (profile: BrowserProfile) => void;
  /** The selected worktree's run, for the `logs` and `nodes` panes. */
  runCtx: RunPaneContext;
}) {
  const { layout, onLayout } = props;
  const areaRef = useRef<HTMLDivElement>(null);
  const bothVisible = dockVisible(layout, 0) && dockVisible(layout, 1);
  /** Where the tab currently being dragged would land, or `null`. */
  const [localDropZone, setDropZone] = useState<DropZone | null>(null);
  /**
   * Whether the pointer has left the window with a tab in hand.
   *
   * Once it is outside, no `dragover` fires and the split preview would simply
   * freeze wherever it was last — showing a confident "this pane will split
   * here" while the actual outcome is a *new window*. The two destinations need
   * two different pictures, and this is the only signal that the drag has left:
   * `dragleave` with no `relatedTarget`.
   */
  const [dragOutside, setDragOutside] = useState(false);
  /**
   * The same fact, readable from `dragend`.
   *
   * `dragend` fires from a handler closed over an older render, so the state
   * above is not reliable there — and this is the *only* trustworthy answer to
   * "did the pointer leave this window", which is what decides whether a
   * released tab is going anywhere. Geometry cannot answer it: two Veld windows
   * overlap, so a point can be inside both, and `dragend`'s own coordinates are
   * not the release point (see the `veld:window:drop-out` handler). Drag events
   * are routed by the OS, which is the only party that knows the stacking order
   * — so `dragleave` is the answer, and this carries it.
   */
  const dragOutsideRef = useRef(false);
  const setOutside = (v: boolean) => {
    dragOutsideRef.current = v;
    setDragOutside(v);
  };

  /**
   * Where a tab being dragged **from another window** would land here.
   *
   * A drag never leaves the document it began in, so this window sees no
   * `dragover` at all — the shell forwards the cursor instead
   * (`onDragOver`), and this is that position resolved into the same answer a
   * local drag would have produced. Rendered by the same indicators and applied
   * by the same functions, so a drop from another window is not a second
   * behaviour to learn.
   */
  const [remote, setRemote] = useState<
    { at: { dock: DockIndex; tabId: string; after: boolean } } | { zone: DropZone } | null
  >(null);
  const remoteRef = useRef(remote);
  remoteRef.current = remote;

  /**
   * Resolve a forwarded cursor into a drop target, by asking the document what
   * is under it.
   *
   * `elementFromPoint` rather than arithmetic over the layout: the tab strip
   * scrolls, tabs are variable width, and the docks are sized by a ratio the
   * splitter moves. The DOM already knows all of that, and a second model of it
   * here would be a second thing to keep in step.
   */
  const resolveRemote = (x: number, y: number) => {
    const area = areaRef.current;
    const el = document.elementFromPoint(x, y);
    if (!area || !el || !area.contains(el)) {
      setRemote(null);
      return;
    }
    const dockEl = el.closest<HTMLElement>("[data-dock]");
    const dock = (dockEl ? Number(dockEl.dataset.dock) : 0) as DockIndex;

    const tabEl = el.closest<HTMLElement>("[data-tab-id]");
    if (tabEl?.dataset.tabId) {
      const box = tabEl.getBoundingClientRect();
      setRemote({
        at: { dock, tabId: tabEl.dataset.tabId, after: x > box.left + box.width / 2 },
      });
      return;
    }
    // The strip itself, past the last tab: append to that dock. Same rule the
    // local drop already uses, and the reason the strip is a target at all.
    if (el.closest(".pane-tabs")) {
      setRemote({ zone: { where: "into", dock } });
      return;
    }
    setRemote({ zone: zoneAt(area.getBoundingClientRect(), x, dock) });
  };

  // Same backstop as the per-dock indicator below: a committed layout retires
  // every preview, however the gesture that produced it ended. **Including the
  // outside-the-window hint** — a drag-out that *works* removes the tab, which
  // unmounts the element `dragend` would have fired on, so a successful move
  // was exactly the case that left the hint on screen for good.
  // biome-ignore lint/correctness/useExhaustiveDependencies: see `dropAt`.
  useEffect(() => {
    setDropZone(null);
    setOutside(false);
  }, [layout]);

  /**
   * A tab drag anywhere in the app.
   *
   * **Every** window freezes its embedded browser views for the duration, not
   * just the one the drag started in. A `WebContentsView` is a native sibling of
   * the page and paints over all DOM regardless of z-index, so a drop overlay
   * under one is simply invisible — and the window being dragged *onto* is
   * exactly the one that needs to show an overlay and has no drag events of its
   * own to trigger the freeze. Same mechanism `onSplitterDown` uses locally.
   */
  useEffect(() => {
    const shell = desktopWindow;
    if (!shell) return;
    const offBegin = shell.onDragBegin(() => beginTabDrag());
    const offEnd = shell.onDragEnd(() => {
      endTabDrag();
      setRemote(null);
    });
    const offOver = shell.onDragOver(({ x, y }) => resolveRemote(x, y));
    const offOut = shell.onDragOut(() => setRemote(null));
    return () => {
      offBegin();
      offEnd();
      offOver();
      offOut();
      endTabDrag();
    };
  }, []);

  /**
   * Tabs dropped here from another window — placed where the preview said.
   *
   * The same three outcomes a local drop has, applied by the same functions:
   * into a dock's strip at a caret, appended to a dock, or split to an edge.
   * Split reuses `splitWithTab` by inserting first and moving second, so the
   * one-dock case (where "make this the left pane" has no dock index) stays in
   * one place.
   */
  useEffect(() => {
    const shell = desktopWindow;
    if (!shell) return;
    return shell.onDropHere(({ tabs }) => {
      const parsed = parseTransferTabs(tabs);
      if (parsed.length === 0) return;
      const where = remoteRef.current;
      setRemote(null);
      onLayout((prev) => {
        let next = prev;
        for (const tab of parsed) {
          if (where && "at" in where) {
            const dock = where.at.dock;
            const idx = next.docks[dock].tabs.findIndex((t) => t.id === where.at.tabId);
            next = insertTab(next, dock, tab, idx < 0 ? undefined : idx + (where.at.after ? 1 : 0));
          } else if (where && where.zone.where === "into") {
            next = insertTab(next, where.zone.dock, tab);
          } else if (where) {
            next = insertTab(next, 0, tab);
            next = splitWithTab(next, tab.id, where.zone.where === "left" ? 0 : 1);
          } else {
            next = addTabToFocused(next, tab);
          }
        }
        return next;
      });
    });
  }, [onLayout]);

  /** Re-entering the window puts the split preview back in charge. */
  const onAreaDragOver = (e: React.DragEvent) => {
    if (!e.dataTransfer.types.includes(TAB_MIME)) return;
    if (dragOutsideRef.current) setOutside(false);
  };

  /**
   * `relatedTarget === null` is the drag crossing the window's own edge —
   * moving between two elements inside it always names the one being entered.
   */
  const onAreaDragLeave = (e: React.DragEvent) => {
    if (!e.dataTransfer.types.includes(TAB_MIME)) return;
    if (e.relatedTarget !== null) return;
    setOutside(true);
    setDropZone(null);
  };

  /**
   * Move tabs into a window of their own.
   *
   * Order is the whole correctness argument. The shell opens the window
   * **first**, and only once it has confirms do we let go here: a refused detach
   * (the window cap) then leaves the tabs exactly where they were, rather than
   * having already removed them from the only layout that names them. The window
   * that briefly exists alongside them is safe — an attach to a live PTY session
   * *takes it over*, which is the same mechanism a reload uses.
   *
   * Then `releaseTerminal` before `closeTab`, because the layouts are what
   * `pruneTerminals` collects against: remove the tab first and the effect that
   * runs on the next commit ends the shell we are in the middle of moving.
   */
  /**
   * Tabs released outside this window, at a point on the screen.
   *
   * Routed through the shell rather than decided here, because "was that over
   * another Veld window?" is a question a renderer cannot answer: a drag never
   * crosses a window boundary, so the window being dropped onto never even
   * learns one is in progress. The shell owns every window's bounds and picks
   * the destination — an existing window showing this worktree, or a new one.
   */
  const dropOutTabs = async (tabs: PaneTab[]) => {
    if (!desktopWindow || tabs.length === 0) return;
    let moved: PaneTab[] = [];
    try {
      const result = await desktopWindow.dropOut({
        worktreeId: props.worktreeId,
        repoRoot: props.repoRoot,
        ratio: layout.ratio,
        tabs,
      });
      if (!result?.moved && !result?.opened) {
        notifyError(
          "Couldn't move that pane",
          result?.reason === "cap"
            ? "Veld Desktop is at its window limit — close one and try again."
            : "The desktop shell refused the request.",
        );
        return;
      }
      const accepted = new Set(result.accepted ?? tabs.map((t) => t.id));
      moved = tabs.filter((t) => accepted.has(t.id));
    } catch (err) {
      notifyError("Couldn't move that pane", err);
      return;
    }
    for (const tab of moved) releaseForTransfer(tab);
    onLayout((prev) => moved.reduce((acc, tab) => closeTab(acc, tab.id), prev));
  };

  const detachTabs = async (tabs: PaneTab[]) => {
    if (!desktopWindow || tabs.length === 0) return;
    let moved: PaneTab[] = [];
    try {
      const result = await desktopWindow.detach({
        worktreeId: props.worktreeId,
        repoRoot: props.repoRoot,
        ratio: layout.ratio,
        tabs,
      });
      if (!result?.opened) {
        notifyError(
          "Couldn't open a new window",
          result?.reason === "cap"
            ? "Veld Desktop is at its window limit — close one and try again."
            : "The desktop shell refused the request.",
        );
        return;
      }
      // Only what the shell actually took. Its own validation drops a tab that
      // is too large, duplicated or of an unknown kind, so letting go of the
      // whole list on `opened: true` would remove a refused tab from the only
      // layout naming it. An older shell answers without the field; treat that
      // as "all of them", which is what it did.
      const accepted = new Set(result.accepted ?? tabs.map((t) => t.id));
      moved = tabs.filter((t) => accepted.has(t.id));
      if (moved.length < tabs.length) {
        notifyError(
          "Some panes stayed here",
          `${tabs.length - moved.length} of ${tabs.length} could not be moved to the new window.`,
        );
      }
    } catch (err) {
      notifyError("Couldn't open a new window", err);
      return;
    }
    for (const tab of moved) releaseForTransfer(tab);
    onLayout((prev) => moved.reduce((acc, tab) => closeTab(acc, tab.id), prev));
  };

  const onBodyDragOver = (e: React.DragEvent, dock: DockIndex) => {
    if (!e.dataTransfer.types.includes(TAB_MIME)) return;
    // Without preventDefault the browser refuses the drop entirely.
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    const area = areaRef.current?.getBoundingClientRect();
    if (!area) return;
    const next = zoneAt(area, e.clientX, dock);
    setDropZone((prev) => (sameZone(prev, next) ? prev : next));
  };

  const onBodyDrop = (e: React.DragEvent, dock: DockIndex) => {
    const id = e.dataTransfer.getData(TAB_MIME);
    const area = areaRef.current?.getBoundingClientRect();
    setDropZone(null);
    endTabDragEverywhere();
    if (!id || !area) return;
    e.preventDefault();
    const zone = zoneAt(area, e.clientX, dock);
    onLayout(
      zone.where === "into"
        ? moveTab(layout, id, zone.dock)
        : splitWithTab(layout, id, zone.where === "left" ? 0 : 1),
    );
  };

  /**
   * The insertion preview: the region the tab would occupy, drawn over it.
   *
   * Derived from the ratio rather than measured, because the docks *are* laid
   * out from the ratio — a measurement would be the same numbers a frame later,
   * and a `getBoundingClientRect` per `dragover` is a read per pointer move.
   */
  const dropPreview = (): React.CSSProperties | null => {
    // A drag from another window has no `dragover` here, so its position comes
    // from `remote` — but it is the same kind of answer and gets the same
    // picture. One preview, two sources.
    const dropZone = localDropZone ?? (remote && "zone" in remote ? remote.zone : null);
    if (!dropZone) return null;
    const pct = (n: number) => `${n * 100}%`;
    // Left and right are only a 50/50 split when there is one dock and the drop
    // is about to *create* the second. With both already on screen the drop is a
    // plain move into a dock the splitter has already sized, so previewing half
    // the area was a promise the drop did not keep — at a ratio of 0.25 the
    // highlight covered twice the region the tab landed in.
    const left = bothVisible ? pct(layout.ratio) : "50%";
    if (dropZone.where === "left") return { left: 0, width: left };
    if (dropZone.where === "right") return { left: left, width: `calc(100% - ${left})` };
    if (!bothVisible) return { left: 0, width: "100%" };
    return dropZone.dock === 0
      ? { left: 0, width: pct(layout.ratio) }
      : { left: pct(layout.ratio), width: pct(1 - layout.ratio) };
  };
  const previewStyle = dropPreview();

  /**
   * Whether a splitter drag currently holds a suspend.
   *
   * In a ref, and released from an unmount effect as well as from the pointer
   * listeners, because those listeners live on the handle element: if the dock
   * unmounts mid-drag — a worktree or mode switch from the 5s poll, a palette
   * accelerator forwarded out of a focused pane — the element takes them with it,
   * `suspendDepth` never returns to zero, and every browser pane in the page stays
   * hidden until a reload.
   */
  const dragSuspended = useRef(false);
  useEffect(
    () => () => {
      if (dragSuspended.current) {
        dragSuspended.current = false;
        popBrowserSuspend();
      }
      // The tab-drag suspend has the same failure mode from further away: its
      // `dragend` is on a tab element, and a worktree switch unmounts the whole
      // region mid-drag.
      endTabDrag();
    },
    [],
  );

  // Pointer capture rather than window listeners: the drag then survives the
  // pointer crossing an iframe or leaving the window, and releases itself.
  //
  // Pointer capture does NOT survive a native view, though. An embedded browser
  // pane is an OS-level child window in the desktop shell, so once the cursor is
  // over one it takes the mouse and the renderer stops seeing `pointermove`
  // entirely — the split froze after a few pixels, and even starting the drag
  // was hard because the splitter's widened hit area lies over the pane. So the
  // views are hidden for the duration of the drag; the panes go blank and come
  // back on release.
  const onSplitterDown = (e: React.PointerEvent<HTMLDivElement>) => {
    const area = areaRef.current;
    if (!area) return;
    e.preventDefault();
    // Held in a local: React clears `currentTarget` on the synthetic event as
    // soon as the handler returns, so the listeners below must not read it.
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    pushBrowserSuspend();
    dragSuspended.current = true;
    const rect = area.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      if (rect.width <= 0) return;
      onLayout(setRatio(layout, (ev.clientX - rect.left) / rect.width));
    };
    const up = (ev: PointerEvent) => {
      // `pointerup` and `pointercancel` can both arrive; the suspend must be
      // popped exactly once or the panes never come back.
      if (!dragSuspended.current) return;
      dragSuspended.current = false;
      popBrowserSuspend();
      handle.releasePointerCapture(ev.pointerId);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    // A cancelled pointer (a browser gesture takes over) fires no pointerup,
    // which would leave the move listener attached and the split following
    // the cursor forever.
    handle.addEventListener("pointercancel", up);
  };

  // Every tab closed: neither dock is visible, so no `DockView` mounts and the
  // whole region rendered blank with no way out. The empty state belongs here as
  // well as inside a dock.
  if (!dockVisible(layout, 0) && !dockVisible(layout, 1)) {
    return (
      <div className="dock-area" ref={areaRef}>
        <PaneChooser
          serviceUrls={props.serviceUrls}
          urlsEmptyHint={props.urlsEmptyHint}
          onTerminal={() =>
            onLayout(addTab(layout, 0, { id: newTabId(), kind: "terminal", title: "Terminal" }))
          }
          onBrowser={(tab) => onLayout(addTab(layout, 0, tab))}
          onDiag={(kind) => onLayout(addTab(layout, 0, diagTab(kind)))}
        />
      </div>
    );
  }

  return (
    <div
      className="dock-area"
      ref={areaRef}
      onDragOver={onAreaDragOver}
      onDragLeave={onAreaDragLeave}
    >
      {([0, 1] as DockIndex[]).map((index) => {
        if (!dockVisible(layout, index)) return null;
        const width = bothVisible
          ? `${(index === 0 ? layout.ratio : 1 - layout.ratio) * 100}%`
          : "100%";
        return (
          <Fragment key={index}>
            {index === 1 && bothVisible && (
              <div
                className="dock-splitter"
                role="separator"
                aria-orientation="vertical"
                aria-label="Resize panes"
                // Focusable with the aria-value triple, so `separator` is not
                // advertising an affordance that only a mouse can reach — and so
                // the `:focus-visible` style is reachable at all.
                tabIndex={0}
                aria-valuenow={Math.round(layout.ratio * 100)}
                aria-valuemin={Math.round(MIN_RATIO * 100)}
                aria-valuemax={Math.round(MAX_RATIO * 100)}
                onPointerDown={onSplitterDown}
                onDoubleClick={() => onLayout(setRatio(layout, DEFAULT_RATIO))}
                onKeyDown={(e) => {
                  const step = e.shiftKey ? 0.1 : 0.02;
                  if (e.key === "ArrowLeft") {
                    onLayout(setRatio(layout, layout.ratio - step));
                  } else if (e.key === "ArrowRight") {
                    onLayout(setRatio(layout, layout.ratio + step));
                  } else if (e.key === "Home") {
                    onLayout(setRatio(layout, DEFAULT_RATIO));
                  } else {
                    return;
                  }
                  e.preventDefault();
                }}
              />
            )}
            <DockView
              index={index}
              width={width}
              layout={layout}
              onLayout={onLayout}
              worktreeId={props.worktreeId}
              serviceUrls={props.serviceUrls}
              urlsEmptyHint={props.urlsEmptyHint}
              sessions={props.sessions}
              onAddSession={props.onAddSession}
              onRemoveSession={props.onRemoveSession}
              runCtx={props.runCtx}
              onDetach={desktopWindow ? detachTabs : undefined}
              onDropOut={desktopWindow ? dropOutTabs : undefined}
              wasOutside={() => dragOutsideRef.current}
              // The caret a drag from *another* window would insert at. Same
              // indicator as a local drag's, because it is the same answer.
              remoteAt={
                remote && "at" in remote && remote.at.dock === index ? remote.at : null
              }
              onBodyDragOver={onBodyDragOver}
              onBodyDrop={onBodyDrop}
              onClearZone={() => {
                setDropZone(null);
                setOutside(false);
              }}
            />
          </Fragment>
        );
      })}
      {/* Above the docks and un-hittable: the drop it previews is being handled
          by the body underneath it, and a target that intercepts its own
          pointer events would swallow every `dragover` after the first. */}
      {previewStyle && !dragOutside && (
        <div className="dock-drop-zone" style={previewStyle} aria-hidden />
      )}
      {/* Outside the window, the answer is a *window*, not a split — so it gets
          its own picture rather than a stale one of the wrong outcome. Drawn
          inside the window because that is the only surface there is; the
          cursor is out over the desktop. */}
      {dragOutside && desktopWindow && (
        <div className="dock-detach-hint" aria-hidden>
          {/* Both destinations, because the page cannot tell which one it is —
              only the shell knows what window is under the cursor. Promising
              just the new window would be wrong half the time. */}
          <span>Release over another Veld window to move it there — or anywhere else for a new one</span>
        </div>
      )}
    </div>
  );
}

function DockView(props: {
  index: DockIndex;
  width: string;
  layout: PaneLayout;
  onLayout: (next: PaneLayoutUpdate) => void;
  worktreeId: number;
  serviceUrls: Array<[string, string]>;
  urlsEmptyHint: string;
  sessions: BrowserProfile[];
  onAddSession: ((tabId: string) => void) | undefined;
  onRemoveSession: (profile: BrowserProfile) => void;
  runCtx: RunPaneContext;
  /** Pull tabs out into a window of their own. Absent outside Electron, which
   *  has no window manager to pull them into. */
  onDetach: ((tabs: PaneTab[]) => void | Promise<void>) | undefined;
  /** Released outside this window — the shell decides where that lands. */
  onDropOut: ((tabs: PaneTab[]) => void) | undefined;
  /** Whether the pointer was outside the window when the drag ended. Read at
   *  `dragend`, so it cannot come from a closed-over render. */
  wasOutside: () => boolean;
  /** Where a drag from another window would insert in *this* dock's strip. */
  remoteAt: { tabId: string; after: boolean } | null;
  onBodyDragOver: (e: React.DragEvent, dock: DockIndex) => void;
  onBodyDrop: (e: React.DragEvent, dock: DockIndex) => void;
  onClearZone: () => void;
}) {
  const { index, layout, onLayout } = props;
  const dock = layout.docks[index];
  const active = activeTab(layout, index);
  const { showContextMenu } = useContextMenu();
  // Which tab currently shows a drop indicator, and on which side.
  const [dropAt, setDropAt] = useState<{ id: string; after: boolean } | null>(null);

  /**
   * Any indicator is stale the moment the layout moves.
   *
   * The backstop for a drag whose `dragend` never arrives, which is the normal
   * case rather than the exotic one: `dragend` fires on the *source tab*, and a
   * drop that moves that tab to the other dock unmounts it first. So a tab
   * hovered on the way past — setting this dock's indicator — and then dropped
   * on a pane body left a 2px accent bar wedged beside a tab, surviving until
   * something else re-rendered it away.
   *
   * A drag alone never changes `layout` (`focusDock` returns the same object
   * when nothing moved), so this cannot clear an indicator mid-gesture.
   */
  // biome-ignore lint/correctness/useExhaustiveDependencies: keyed on the layout
  // object identity, which is the "something committed" signal; `dropAt` must
  // not be a dependency or clearing it would re-run this forever.
  useEffect(() => setDropAt(null), [layout]);

  /**
   * Turn the `new` pane the user is choosing from into the kind they picked, or
   * open a fresh tab when there is nothing to convert (an empty dock).
   *
   * Replacing rather than adding is what keeps the flow to one tab: `+` opens a
   * pane, the pane asks what it should be, and the answer lands in the same slot
   * instead of leaving a stale "new pane" tab behind.
   */
  const convertOrAdd = (from: PaneTab | null, tab: PaneTab) => {
    if (from && from.kind === "new") onLayout(replaceTab(layout, from.id, tab));
    else onLayout(addTab(layout, index, tab));
  };

  /** Drop a dragged tab at a position in this dock. */
  const dropTab = (e: React.DragEvent, at?: number) => {
    const id = e.dataTransfer.getData(TAB_MIME);
    setDropAt(null);
    props.onClearZone();
    // Here as well as in `onDragEnd`: a drop that moves a tab to another dock
    // unmounts the element the drag started on, and `dragend` never reaches it.
    endTabDragEverywhere();
    if (!id) return;
    e.preventDefault();
    onLayout(moveTab(layout, id, index, at));
  };

  const bothDocks = dockVisible(layout, 0) && dockVisible(layout, 1);

  const tabMenu = (tab: PaneTab) =>
    showContextMenu([
      {
        key: "move",
        icon: <IconArrowsExchange size={14} />,
        // "Move to the left pane" is meaningless when there is only one pane —
        // there is nothing to the left of anything. With one dock the action is
        // a split, and it is named as one.
        title: bothDocks
          ? index === 0
            ? "Move to the right pane"
            : "Move to the left pane"
          : "Split into a second pane",
        // Moving a dock's only tab would just swap the two sides.
        disabled: dock.tabs.length < 2,
        onClick: () => onLayout(moveTabToOtherDock(layout, tab.id)),
      },
      ...(props.onDetach
        ? [
            {
              key: "detach",
              icon: <IconExternalLink size={14} />,
              // The reload is named rather than discovered. A `WebContentsView`
              // belongs to a window, so moving a browser pane between two is a
              // destroy-and-recreate: the URL survives, the scroll position and
              // anything typed into the page do not. A terminal has no such
              // problem — the shell is the daemon's, and the new window attaches
              // to the same session.
              title:
                tab.kind === "browser"
                  ? "Open in a new window (reloads the page)"
                  : "Open in a new window",
              onClick: () => void props.onDetach?.([tab]),
            },
          ]
        : []),
      ...(tab.kind === "terminal"
        ? [
            {
              key: "restart",
              icon: <IconRefresh size={14} />,
              title: "Restart this terminal",
              onClick: () => restartTerminal(tab.id),
            },
          ]
        : []),
      ...(tab.kind === "browser"
        ? [
            {
              key: "reload",
              icon: <IconRefresh size={14} />,
              title: "Reload this page",
              onClick: () => reloadBrowser(tab.id),
            },
            {
              key: "duplicate",
              icon: <IconWorld size={14} />,
              title: "Duplicate in a new pane",
              onClick: () =>
                onLayout(
                  addTab(
                    layout,
                    index,
                    browserTab({ url: tab.url, title: tab.title, profile: tab.profile }),
                  ),
                ),
            },
          ]
        : []),
      {
        key: "close",
        icon: <IconX size={14} />,
        title: "Close",
        onClick: () => onLayout(closeTab(layout, tab.id)),
      },
    ]);

  return (
    <section
      // No focus class: which dock has the keyboard is styled with
      // `.dock:focus-within` (styles.css), because `layout.focused` only records
      // where the *next* tab would open, not where focus actually is. Hanging a
      // focus style off a class driven by that field is the trap to avoid.
      className="dock"
      // Read by `resolveRemote`, which hit-tests a cursor forwarded from
      // another window's drag with `elementFromPoint` — the DOM already knows
      // the scroll offsets, tab widths and split ratio that answer would
      // otherwise have to be re-derived from.
      data-dock={index}
      style={{ width: props.width }}
      onFocus={() => onLayout(focusDock(layout, index))}
      aria-label={index === 0 ? "Primary pane" : "Secondary pane"}
    >
      <div
        className="pane-tabs"
        // Dropping on the empty part of the strip appends to this dock, which
        // is the only way to move a tab into a dock that has none.
        onDragOver={(e) => {
          if (!e.dataTransfer.types.includes(TAB_MIME)) return;
          e.preventDefault();
          // The strip and the body are two halves of one drop model, so entering
          // the strip has to retract the body's preview — otherwise a drag that
          // crossed the body leaves a highlighted region behind it.
          props.onClearZone();
        }}
        onDrop={(e) => dropTab(e)}
      >
        {/* Only the tabs scroll. The `+` sits outside this box so it survives a
            strip full of tabs, and `role="tablist"` lives on the scroller rather
            than on this row — a tablist whose children include a menu button and a
            drop spacer is not one. The wrapper each tab needs (label and close as
            siblings) is `role="presentation"`, so the `role="tab"` button inside it
            is the tablist's effective child. */}
        <TabScroller tabKey={dock.tabs.map((t) => t.id).join(",")}>
        {dock.tabs.map((tab, at) => (
          <TabButton
            key={tab.id}
            tab={tab}
            label={paneTabLabel(layout, tab)}
            icon={tabIcon(tab)}
            selected={tab.id === dock.activeId}
            // Local drag first, then one forwarded from another window — the
            // two cannot both be live, and they draw the same caret.
            drop={
              dropAt?.id === tab.id
                ? dropAt.after
                  ? "after"
                  : "before"
                : props.remoteAt?.tabId === tab.id
                  ? props.remoteAt.after
                    ? "after"
                    : "before"
                  : null
            }
            onSelect={() => onLayout(activateTab(layout, tab.id))}
            onClose={() => onLayout(closeTab(layout, tab.id))}
            onMove={() => onLayout(moveTabToOtherDock(layout, tab.id))}
            onMenu={(e) => tabMenu(tab)(e)}
            canMove={dock.tabs.length > 1}
            canDetach={props.onDetach !== undefined}
            onDragStartTab={beginTabDragEverywhere}
            onDragOverTab={(after) => {
              props.onClearZone();
              setDropAt({ id: tab.id, after });
            }}
            onDropTab={(e, after) => {
              // The index is read in the destination's post-removal terms,
              // which is what `moveTab` documents and what the indicator drawn
              // between two tabs means.
              const dragged = e.dataTransfer.getData(TAB_MIME);
              const sameDock = dockOf(layout, dragged) === index;
              const removedBefore =
                sameDock && dock.tabs.findIndex((t) => t.id === dragged) < at;
              dropTab(e, at + (after ? 1 : 0) - (removedBefore ? 1 : 0));
            }}
            onDragEndTab={() => {
              // **Read before clearing.** `onClearZone` resets the same flag,
              // synchronously, so asking after it always answered "inside" and
              // no drag ever left the window — including the plain detach that
              // worked before this became the trigger.
              const outside = props.wasOutside();
              setDropAt(null);
              props.onClearZone();
              endTabDragEverywhere();
              // Released while the pointer was outside this window. **Not
              // `dropEffect`, and not coordinates** — see `dragOutsideRef`: the
              // OS routes drag events by stacking order, so `dragleave` is the
              // one signal that knows two Veld windows overlap. Where it landed
              // is then the shell's to resolve.
              if (props.onDropOut && outside) props.onDropOut([tab]);
            }}
          />
        ))}
        </TabScroller>
        {/* Immediately after the last tab, not pinned to the right: it is the
            end of the strip, and a control 600px from the thing it extends reads
            as unrelated to it.

            Click opens a `new` pane and lets the choice happen at pane size;
            hover offers the one-click shortcuts for people who already know what
            they want. `trigger="hover"` leaves the click to us. */}
        <Menu position="bottom-start" trigger="hover" openDelay={500} closeDelay={150} withinPortal>
          <Menu.Target>
            <ActionIcon
              className="pane-add-btn"
              size="sm"
              variant="subtle"
              color="gray"
              aria-label="New pane"
              title="New pane"
              onClick={() => onLayout(addTab(layout, index, newPaneTab()))}
            >
              <IconPlus size={14} />
            </ActionIcon>
          </Menu.Target>
          <Menu.Dropdown>
            {/* Through `convertOrAdd`, not `addTab`: clicking `+` opens a `new`
                pane and hovering it opens this menu, so the two compose — picking
                a kind here has to consume that pane rather than leave it orphaned
                beside the one it just made. */}
            <Menu.Item
              leftSection={<IconTerminal2 size={14} />}
              onClick={() =>
                convertOrAdd(active, { id: newTabId(), kind: "terminal", title: "Terminal" })
              }
            >
              New terminal
            </Menu.Item>
            <Menu.Item
              leftSection={<IconWorld size={14} />}
              onClick={() => convertOrAdd(active, browserTab({}))}
            >
              New browser pane
            </Menu.Item>
            <Menu.Item
              leftSection={<IconLogs size={14} />}
              onClick={() => convertOrAdd(active, diagTab("logs"))}
            >
              Run logs
            </Menu.Item>
            <Menu.Item
              leftSection={<IconActivityHeartbeat size={14} />}
              onClick={() => convertOrAdd(active, diagTab("nodes"))}
            >
              Node health
            </Menu.Item>
            <Menu.Divider />
            <Menu.Item
              leftSection={<IconLayoutColumns size={14} />}
              onClick={() =>
                onLayout(addTab(layout, (index === 0 ? 1 : 0) as DockIndex, newPaneTab()))
              }
            >
              Open to the side
            </Menu.Item>
            {props.serviceUrls.length > 0 && <Menu.Divider />}
            {props.serviceUrls.map(([name, url]) => (
              <Menu.Item
                key={name}
                leftSection={<IconWorld size={14} />}
                onClick={() => convertOrAdd(active, browserTab({ url, title: name }))}
              >
                {name}
              </Menu.Item>
            ))}
          </Menu.Dropdown>
        </Menu>
        {/* Takes the rest of the strip, so a drop anywhere right of the tabs
            still appends to this dock. */}
        <div style={{ flex: 1 }} />
      </div>

      {/* The body is a drop target as well as the strip. Dropping a tab where
          its content will be is the gesture people try first, and until now it
          did nothing at all — `dragover` without `preventDefault` is a refusal.
          The edge zones live here too; see `zoneAt`. */}
      <div
        className="dock-body"
        onDragOver={(e) => {
          // Leaving the strip for the body retracts the strip's own indicator,
          // so the two halves of the drop model never both claim the drag.
          setDropAt(null);
          props.onBodyDragOver(e, index);
        }}
        onDrop={(e) => {
          setDropAt(null);
          props.onBodyDrop(e, index);
        }}
      >
        {(active === null || active.kind === "new") && (
          <PaneChooser
            serviceUrls={props.serviceUrls}
            urlsEmptyHint={props.urlsEmptyHint}
            // A `new` tab becomes the chosen kind in place; an empty dock has no
            // tab to convert, so it gets a fresh one.
            onTerminal={() =>
              convertOrAdd(active, { id: newTabId(), kind: "terminal", title: "Terminal" })
            }
            onBrowser={(tab) => convertOrAdd(active, tab)}
            onDiag={(kind) => convertOrAdd(active, diagTab(kind))}
          />
        )}
        {active?.kind === "logs" && <LogsPane ctx={props.runCtx} />}
        {active?.kind === "nodes" && <NodesPane ctx={props.runCtx} />}
        {active?.kind === "browser" && (
          <BrowserPane
            key={active.id}
            tab={active}
            serviceUrls={props.serviceUrls}
            urlsEmptyHint={props.urlsEmptyHint}
            sessions={props.sessions}
            onAddSession={
              props.onAddSession && (() => props.onAddSession?.(active.id))
            }
            onRemoveSession={props.onRemoveSession}
            // Updater form on purpose: both docks can hold a browser pane, and
            // two navigations landing in the same commit would otherwise write
            // from the same stale `layout` and lose one.
            onTab={(patch) => onLayout((prev) => updateTab(prev, active.id, patch))}
          />
        )}
        {active?.kind === "terminal" && (
          <TerminalPane
            key={active.id}
            id={active.id}
            worktreeId={props.worktreeId}
            // Only the focused dock's terminal takes the keyboard. Both docks
            // mount on load, so focusing unconditionally handed it to whichever
            // mounted last — which after a reload was not the pane the user
            // left focused.
            takeFocus={layout.focused === index}
          />
        )}
      </div>
    </section>
  );
}

/**
 * What a pane can become, at pane size.
 *
 * The one screen behind three situations: a `new` tab (what `+` opens), a dock
 * with no tabs, and the whole region when everything is closed. The choice
 * belongs here rather than in a menu off the `+` button — a menu is the size of a
 * cursor and disappears when you look away, while this is where the content will
 * actually be. The `+` keeps a hover menu for the one-click path.
 */
function PaneChooser(props: {
  serviceUrls: Array<[string, string]>;
  urlsEmptyHint: string;
  onTerminal: () => void;
  onBrowser: (tab: PaneTab) => void;
  onDiag: (kind: DiagKind) => void;
}) {
  return (
    <div className="pane-chooser">
      <div className="pane-chooser-row">
        <button className="btn big" onClick={props.onTerminal}>
          <IconTerminal2 size={15} /> Terminal
        </button>
        <button className="btn big" onClick={() => props.onBrowser(browserTab({}))}>
          <IconWorld size={15} /> Browser
        </button>
      </div>
      {/* Second row, not four buttons in one: the first two are what a pane
          usually becomes, and the diagnostics are what you add when something is
          wrong. Same size, so neither reads as disabled. */}
      <div className="pane-chooser-row">
        <button className="btn big" onClick={() => props.onDiag("logs")}>
          <IconLogs size={15} /> Logs
        </button>
        <button className="btn big" onClick={() => props.onDiag("nodes")}>
          <IconActivityHeartbeat size={15} /> Nodes
        </button>
      </div>
      {/* The run's URLs, one click from being the pane's content. Not a third
          button opening a third kind — see VeldLinks.tsx. The rule separates
          "what should this pane be" from "where should it go", which are two
          questions that happen to share a screen. */}
      <hr className="pane-chooser-rule" />
      <VeldLinks
        urls={props.serviceUrls}
        emptyHint={props.urlsEmptyHint}
        onOpen={(name, url) => props.onBrowser(browserTab({ url, title: name }))}
      />
    </div>
  );
}

/**
 * A tab's kind, as a glyph.
 *
 * For a browser pane the glyph carries the session colour rather than sitting
 * beside a separate coloured dot: two markers for one fact is noise at tab size,
 * and a tinted globe says both things at once.
 */
function tabIcon(tab: PaneTab): React.ReactNode {
  switch (tab.kind) {
    case "terminal":
      return <IconTerminal2 size={12} />;
    case "browser": {
      const color = browserTabDot(tab);
      return <IconWorld size={12} style={color ? { color } : undefined} />;
    }
    case "logs":
      return <IconLogs size={12} />;
    case "nodes":
      return <IconActivityHeartbeat size={12} />;
    case "new":
      return <IconPlus size={12} />;
    default:
      // Makes a new `PaneKind` a compile error here. Without it this switch is
      // the one kind-conditional the typechecker cannot see: `React.ReactNode`
      // includes `undefined`, so falling off the end is legal — unlike
      // `paneTabLabel`, which returns `string` and so fails TS2366 on a missing
      // case. A new kind would otherwise render with no glyph and pass every
      // check in the pre-pass.
      return unhandledKind(tab.kind);
  }
}

/** Turns a missing `PaneKind` branch into a compile error at the call site. */
function unhandledKind(kind: never): never {
  throw new Error(`unhandled pane kind: ${String(kind)}`);
}

/**
 * The scrolling part of a tab strip, with fades that say which way it can go.
 *
 * A strip that scrolls with no scrollbar (there is no room for one in 30px) has a
 * hidden state: tabs to the left of the first visible one are invisible, and
 * nothing says they exist. So each edge carries a fade that appears only while
 * there is something past it — the signal a scrollbar would give, in the space
 * available.
 *
 * The edges are measured rather than guessed, because "can it scroll" has three
 * inputs that change independently: the tab count, the dock's width (the splitter
 * drags), and the scroll position. A `ResizeObserver` covers the first two, the
 * scroll event the third.
 */
function TabScroller(props: {
  children: React.ReactNode;
  /** The tab ids, joined. The effect's dependency — see below. */
  tabKey: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ left: false, right: false });

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => {
      // 1px of slack: a fractional scroll offset (a trackpad, a zoomed page) would
      // otherwise leave a fade on at the very end of the strip.
      const left = el.scrollLeft > 1;
      const right = el.scrollLeft + el.clientWidth < el.scrollWidth - 1;
      setEdges((prev) => (prev.left === left && prev.right === right ? prev : { left, right }));
    };
    measure();
    el.addEventListener("scroll", measure, { passive: true });
    const ro = new ResizeObserver(measure);
    // The box for the dock's width, its children for the tab count — a tab opening
    // or closing changes the scrollable width without resizing the box.
    ro.observe(el);
    for (const child of Array.from(el.children)) ro.observe(child);
    return () => {
      el.removeEventListener("scroll", measure);
      ro.disconnect();
    };
    // `tabKey`, not `children`: the children are a fresh array on every render, so
    // depending on them tore down and rebuilt the observer and the listener on
    // every 5s poll and every keystroke in the palette, per dock.
  }, [props.tabKey]);

  return (
    <div className="pane-tab-strip">
      <div className="pane-tab-scroll" role="tablist" aria-label="Panes" ref={box}>
        {props.children}
      </div>
      {/* Decorative and un-clickable: what it says is "there is more that way",
          which the tablist already conveys to a screen reader. */}
      <span className={`tab-fade left${edges.left ? " on" : ""}`} aria-hidden />
      <span className={`tab-fade right${edges.right ? " on" : ""}`} aria-hidden />
    </div>
  );
}

/**
 * A tab and its close button as siblings, not nested.
 *
 * A `<button>` inside a `<button>` is invalid HTML, and the pattern the
 * worktree rail is stuck with (`div[role=button]` wrapping real controls)
 * hides the inner controls from assistive tech — a known debt from #169. New
 * surfaces shouldn't add to it.
 */
function TabButton(props: {
  tab: PaneTab;
  label: string;
  /** The kind's glyph, so a strip of tabs is readable without their titles. */
  icon: React.ReactNode;
  selected: boolean;
  /** Which edge to draw a drop indicator on, if any. */
  drop: "before" | "after" | null;
  onSelect: () => void;
  onClose: () => void;
  onMove: () => void;
  onMenu: (e: React.MouseEvent) => void;
  canMove: boolean;
  /** Whether dragging this tab out of the window does anything — Electron only. */
  canDetach: boolean;
  onDragStartTab: () => void;
  onDragOverTab: (after: boolean) => void;
  onDropTab: (e: React.DragEvent, after: boolean) => void;
  onDragEndTab: () => void;
}) {
  const [dragging, setDragging] = useState(false);
  const box = useRef<HTMLSpanElement>(null);

  // A strip full of tabs scrolls, and a tab can become the active one without
  // being clicked — ⌘K, a drop, or closing its neighbour. Reveal it, or the
  // selection is somewhere off screen with nothing to say so.
  useEffect(() => {
    if (props.selected) box.current?.scrollIntoView({ inline: "nearest", block: "nearest" });
  }, [props.selected]);

  /** Which half of the tab the pointer is over — the drop goes to that side. */
  const isAfter = (e: React.DragEvent<HTMLElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    return e.clientX > box.left + box.width / 2;
  };

  return (
    <span
      ref={box}
      // Presentational: the accessible tab is the button inside, so this wrapper
      // must not sit between the tablist and it as an unlabelled group.
      role="presentation"
      // See `data-dock`: this is how a forwarded cursor finds the tab it is over.
      data-tab-id={props.tab.id}
      className={[
        "pane-tab",
        props.selected ? "sel" : "",
        dragging ? "dragging" : "",
        props.drop ? `drop-${props.drop}` : "",
      ]
        .filter(Boolean)
        .join(" ")}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData(TAB_MIME, props.tab.id);
        e.dataTransfer.effectAllowed = "move";
        setDragging(true);
        props.onDragStartTab();
      }}
      onDragEnd={() => {
        setDragging(false);
        props.onDragEndTab();
      }}
      onDragOver={(e) => {
        if (!e.dataTransfer.types.includes(TAB_MIME)) return;
        // Without preventDefault the browser refuses the drop entirely.
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        props.onDragOverTab(isAfter(e));
      }}
      onDrop={(e) => {
        e.stopPropagation();
        props.onDropTab(e, isAfter(e));
      }}
      onContextMenu={props.onMenu}
    >
      <button
        type="button"
        role="tab"
        aria-selected={props.selected}
        className="pane-tab-label"
        onClick={props.onSelect}
        onAuxClick={(e) => {
          // Middle-click closes, as everywhere else with tabs.
          if (e.button === 1) props.onClose();
        }}
        onDoubleClick={() => props.canMove && props.onMove()}
        // The label leads, because it is clamped in CSS (a page title can be a
        // sentence) and the tooltip is then the only place the whole thing is
        // readable — the drag hints follow it rather than replacing it.
        title={[
          props.label,
          props.canMove
            ? "Drag to reorder, to a pane edge to split · double-click to send to the other pane"
            : "Drag to a pane edge to split",
          ...(props.canDetach ? ["Drag out of the window to open it in its own"] : []),
          "Right-click for more",
        ].join("\n")}
      >
        <span className="pane-tab-icon" aria-hidden>
          {props.icon}
        </span>
        <span className="pane-tab-text">{props.label}</span>
      </button>
      <button
        type="button"
        className="pane-tab-close"
        aria-label={`Close ${props.label}`}
        onClick={props.onClose}
      >
        <IconX size={11} />
      </button>
    </span>
  );
}

/**
 * Host element for one terminal.
 *
 * This component owns no terminal state — it reparents the live element from
 * `terminalHost` and lets go of it on unmount. Unmounting must not end the
 * session: React unmounts this on every tab and worktree switch.
 */
function TerminalPane(props: { id: string; worktreeId: number; takeFocus: boolean }) {
  const { id, worktreeId } = props;
  const slot = useRef<HTMLDivElement>(null);
  const [, bump] = useReducer((n: number) => n + 1, 0);
  // Read through a ref so the mount effect below sees the value this pane had
  // when it mounted, without focus becoming a dependency — later changes to
  // which dock is focused are the browser's business, not ours to re-assert.
  const takeFocus = useRef(props.takeFocus);
  takeFocus.current = props.takeFocus;

  useEffect(() => {
    const el = slot.current;
    if (!el) return;
    mountTerminal(id, worktreeId, el);
    const unsubscribe = subscribeTerminal(id, bump);
    if (takeFocus.current) focusTerminal(id);
    return () => {
      unsubscribe();
      // Detach only. The session (and its shell) outlives this component;
      // `pruneTerminals` in App.tsx is what actually ends one.
      unmountTerminal(id);
    };
  }, [id, worktreeId]);

  const { state, detail } = terminalStatus(id);
  const dead = state === "ended" || state === "error";
  const restart = useCallback(() => restartTerminal(id), [id]);
  const reconnect = useCallback(() => reconnectTerminal(id), [id]);
  // `error` means the pipe broke, not that the shell did — the session very
  // likely survives on the daemon (that is what the detach grace is for), so
  // offer to reattach before offering to replace it.
  const canReconnect = state === "error";

  return (
    <div className="term-pane">
      <div className="term-slot" ref={slot} />
      {state === "connecting" && <div className="term-status">connecting…</div>}
      {/* A note about a terminal that is still running (dropped output, say).
          It goes here rather than into the terminal, which would corrupt a
          full-screen program's display — see writeNotice in terminalHost. */}
      {state === "live" && detail && <div className="term-status">{detail}</div>}
      {dead && (
        <div className="term-status">
          <span>{detail || "session ended"}</span>
          {canReconnect && (
            <button className="btn" onClick={reconnect} title="Reattach to the same shell">
              Reconnect
            </button>
          )}
          <button
            className="btn"
            onClick={restart}
            title="End this shell and start a new one"
          >
            Restart
          </button>
        </div>
      )}
    </div>
  );
}
