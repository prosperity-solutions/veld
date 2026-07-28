/**
 * The dock region of IDE mode: two tab hosts side by side with a draggable
 * split. See `model.ts` for why this replaced the fixed columns.
 *
 * The embedded browser (#167 batch 3) and run diagnostics (batch 4) are meant to
 * land as tabs here rather than as new columns. Adding a content type touches
 * exactly four places: `PANE_KINDS` in `model.ts` (which is also what validates
 * a restored layout), the `active?.kind === …` branches in `DockView`'s body
 * below, the `+` menu beside them, and — if it needs one — a label rule like
 * `terminalLabel`.
 */

import { ActionIcon, Menu } from "@mantine/core";
import {
  IconArrowsExchange,
  IconPlus,
  IconRefresh,
  IconSquares,
  IconTerminal2,
  IconX,
} from "@tabler/icons-react";
import { useContextMenu } from "mantine-contextmenu";
import { Fragment, useCallback, useEffect, useReducer, useRef, useState } from "react";
import {
  DEFAULT_RATIO,
  type DockIndex,
  MAX_RATIO,
  MIN_RATIO,
  type PaneLayout,
  type PaneTab,
  SERVICES_TAB_ID,
  activateTab,
  activeTab,
  addTab,
  closeTab,
  dockOf,
  dockVisible,
  focusDock,
  hasTab,
  moveTab,
  moveTabToOtherDock,
  newTerminalId,
  setRatio,
  terminalLabel,
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
  onLayout: (next: PaneLayout) => void;
  /** Which worktree's terminals these are. */
  worktreeId: number;
  /** The services tab's content, owned by the app (it needs run state). */
  renderServices: () => React.ReactNode;
}) {
  const { layout, onLayout } = props;
  const areaRef = useRef<HTMLDivElement>(null);
  const bothVisible = dockVisible(layout, 0) && dockVisible(layout, 1);

  // Pointer capture rather than window listeners: the drag then survives the
  // pointer crossing an iframe or leaving the window, and releases itself.
  const onSplitterDown = (e: React.PointerEvent<HTMLDivElement>) => {
    const area = areaRef.current;
    if (!area) return;
    e.preventDefault();
    // Held in a local: React clears `currentTarget` on the synthetic event as
    // soon as the handler returns, so the listeners below must not read it.
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    const rect = area.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      if (rect.width <= 0) return;
      onLayout(setRatio(layout, (ev.clientX - rect.left) / rect.width));
    };
    const up = (ev: PointerEvent) => {
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
              renderServices={props.renderServices}
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
  onLayout: (next: PaneLayout) => void;
  worktreeId: number;
  renderServices: () => React.ReactNode;
}) {
  const { index, layout, onLayout } = props;
  const dock = layout.docks[index];
  const active = activeTab(layout, index);
  const { showContextMenu } = useContextMenu();
  // Which tab currently shows a drop indicator, and on which side.
  const [dropAt, setDropAt] = useState<{ id: string; after: boolean } | null>(null);

  const openTerminal = () =>
    onLayout(
      addTab(layout, index, {
        id: newTerminalId(),
        kind: "terminal",
        title: "terminal",
      }),
    );

  /** Drop a dragged tab at a position in this dock. */
  const dropTab = (e: React.DragEvent, at?: number) => {
    const id = e.dataTransfer.getData(TAB_MIME);
    setDropAt(null);
    if (!id) return;
    e.preventDefault();
    onLayout(moveTab(layout, id, index, at));
  };

  const tabMenu = (tab: PaneTab) =>
    showContextMenu([
      {
        key: "move",
        icon: <IconArrowsExchange size={14} />,
        title: index === 0 ? "Move to the right pane" : "Move to the left pane",
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
        role="tablist"
        aria-label="Panes"
        // Dropping on the empty part of the strip appends to this dock, which
        // is the only way to move a tab into a dock that has none.
        onDragOver={(e) => {
          if (e.dataTransfer.types.includes(TAB_MIME)) e.preventDefault();
        }}
        onDrop={(e) => dropTab(e)}
      >
        {dock.tabs.map((tab, at) => (
          <TabButton
            key={tab.id}
            tab={tab}
            label={tab.kind === "terminal" ? terminalLabel(layout, tab.id) : tab.title}
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
        <div style={{ flex: 1 }} />
        <Menu position="bottom-end" withinPortal>
          <Menu.Target>
            <ActionIcon size="sm" variant="subtle" color="gray" aria-label="New pane">
              <IconPlus size={14} />
            </ActionIcon>
          </Menu.Target>
          <Menu.Dropdown>
            <Menu.Item leftSection={<IconTerminal2 size={14} />} onClick={openTerminal}>
              New terminal
            </Menu.Item>
            <Menu.Item
              leftSection={<IconSquares size={14} />}
              disabled={hasTab(layout, SERVICES_TAB_ID)}
              onClick={() =>
                onLayout(
                  addTab(layout, index, {
                    id: SERVICES_TAB_ID,
                    kind: "services",
                    title: "services",
                  }),
                )
              }
            >
              Services
            </Menu.Item>
          </Menu.Dropdown>
        </Menu>
      </div>

      <div className="dock-body">
        {active === null && (
          <div className="dock-empty">
            <button className="btn" onClick={openTerminal}>
              New terminal
            </button>
          </div>
        )}
        {active?.kind === "services" && props.renderServices()}
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

  /** Which half of the tab the pointer is over — the drop goes to that side. */
  const isAfter = (e: React.DragEvent<HTMLElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    return e.clientX > box.left + box.width / 2;
  };

  return (
    <span
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
        title={
          props.canMove
            ? "Drag to reorder or move · double-click to send to the other pane · right-click for more"
            : "Drag to move to the other pane · right-click for more"
        }
      >
        {props.label}
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
