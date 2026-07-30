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
 *
 * Only 1-3 are enforced. Note what is *not* a kind: the run's URLs, which are a
 * launcher shown inside a pane rather than a pane of their own (`VeldLinks.tsx`).
 */

import { ActionIcon, Menu } from "@mantine/core";
import {
  IconActivityHeartbeat,
  IconArrowsExchange,
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
  type DockIndex,
  MAX_RATIO,
  MIN_RATIO,
  type PaneLayout,
  type PaneLayoutUpdate,
  type PaneTab,
  activateTab,
  activeTab,
  addTab,
  browserTab,
  closeTab,
  diagTab,
  dockOf,
  dockVisible,
  focusDock,
  moveTab,
  moveTabToOtherDock,
  newPaneTab,
  newTabId,
  paneTabLabel,
  replaceTab,
  setRatio,
  updateTab,
} from "./model";

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
  restartTerminal,
  subscribeTerminal,
  terminalStatus,
  unmountTerminal,
} from "./terminalHost";

export function PaneArea(props: {
  layout: PaneLayout;
  onLayout: (next: PaneLayoutUpdate) => void;
  /** Which worktree's terminals these are. */
  worktreeId: number;
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
    <div className="dock-area" ref={areaRef}>
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
            />
          </Fragment>
        );
      })}
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
}) {
  const { index, layout, onLayout } = props;
  const dock = layout.docks[index];
  const active = activeTab(layout, index);
  const { showContextMenu } = useContextMenu();
  // Which tab currently shows a drop indicator, and on which side.
  const [dropAt, setDropAt] = useState<{ id: string; after: boolean } | null>(null);

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
      style={{ width: props.width }}
      onFocus={() => onLayout(focusDock(layout, index))}
      aria-label={index === 0 ? "Primary pane" : "Secondary pane"}
    >
      <div
        className="pane-tabs"
        // Dropping on the empty part of the strip appends to this dock, which
        // is the only way to move a tab into a dock that has none.
        onDragOver={(e) => {
          if (e.dataTransfer.types.includes(TAB_MIME)) e.preventDefault();
        }}
        onDrop={(e) => dropTab(e)}
      >
        {/* Only the tabs scroll. The `+` sits outside this box so it survives a
            strip full of tabs — and `role="tablist"` lives on the scroller rather
            than on the row, because a tablist's tabs have to be its own children. */}
        <TabScroller>
        {dock.tabs.map((tab, at) => (
          <TabButton
            key={tab.id}
            tab={tab}
            label={paneTabLabel(layout, tab)}
            icon={tabIcon(tab)}
            selected={tab.id === dock.activeId}
            drop={dropAt?.id === tab.id ? (dropAt.after ? "after" : "before") : null}
            onSelect={() => onLayout(activateTab(layout, tab.id))}
            onClose={() => onLayout(closeTab(layout, tab.id))}
            onMove={() => onLayout(moveTabToOtherDock(layout, tab.id))}
            onMenu={(e) => tabMenu(tab)(e)}
            canMove={dock.tabs.length > 1}
            onDragOverTab={(after) => setDropAt({ id: tab.id, after })}
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
            onDragEndTab={() => setDropAt(null)}
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

      <div className="dock-body">
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
  onDiag: (kind: "logs" | "nodes") => void;
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
function TabScroller(props: { children: React.ReactNode }) {
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
    // Re-run when the tab set changes, so the observer follows the new children.
  }, [props.children]);

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
        title={
          props.canMove
            ? `${props.label}\nDrag to reorder or move · double-click to send to the other pane · right-click for more`
            : `${props.label}\nDrag to move to the other pane · right-click for more`
        }
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
