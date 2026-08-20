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
 * (`PlaceList.tsx`).
 */

import { ActionIcon, Button, Menu, Modal, Text, Tooltip } from "@mantine/core";
import {
  IconActivityHeartbeat,
  IconArrowsExchange,
  IconBolt,
  IconBookmark,
  IconExternalLink,
  IconHistory,
  IconLayoutColumns,
  IconLogs,
  IconPlus,
  IconRefresh,
  IconTerminal2,
  IconWindow,
  IconWorld,
  IconX,
} from "@tabler/icons-react";
import { useContextMenu } from "mantine-contextmenu";
import type { MutableRefObject } from "react";
import {
  Fragment,
  useCallback,
  useEffect,
  useImperativeHandle,
  useReducer,
  useRef,
  useState,
} from "react";
import { BrowserPane, browserTabDot } from "./BrowserPane";
import { LogsPane, NodesPane, type RunPaneContext } from "./RunPanes";
import { BookmarksModal, FilesButton, FilesModal, PlaceList } from "./PlaceList";
import { inlineFiles, placesFor, suggestionsFor } from "./places";
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
  configPaneTab,
  diagTab,
  dockOf,
  dockVisible,
  fileLabel,
  focusDock,
  hasTab,
  insertTab,
  moveTab,
  moveTabToOtherDock,
  newPaneTab,
  newTabId,
  paneTabLabel,
  parseTransferTabs,
  replaceTab,
  paneAnswerFor,
  type PaneSessionAnswer,
  setRatio,
  splitWithTab,
  updateTab,
} from "./model";
import { inbox, type RowState } from "../inbox/inbox";
import { HEADLINE, PaneActivityIcon } from "../inbox/InboxIcon";
import { useInbox } from "../inbox/useInbox";
import { notifyError } from "../shared/notify";
import type { QuickSwitchPrefs } from "../shared/settings";
import type { PaneSpec, Quicklink, ViewableFile } from "../api";
import { api } from "../api";
import { paneIcon } from "./paneIcons";
import { desktopWindow } from "../shell";
import { type DropZone, sameZone, zoneAt } from "./dropModel";
import { tabKeyAction } from "./tabKeys";
import { TAB_MIME } from "./terminalPaste";


/** Where a tab dragged from *another* window would land in this one: at a caret
 *  in a dock's strip, or in a region of the pane area. */
type RemoteTarget =
  | { at: { dock: DockIndex; tabId: string; after: boolean } }
  | { zone: DropZone }
  | null;

/**
 * The caret a pointer at `x` means, anywhere in a tab strip.
 *
 * Measured against the tabs' own midpoints rather than asking which element is
 * under the pointer, because a strip is not only tabs: it has padding, a
 * scroller, a `+` button and a flex spacer, and the gaps between those are
 * exactly where the *first* and *last* positions live. Hit-testing the element
 * meant the left edge of the first tab resolved to "somewhere in the strip" and
 * fell through to appending — so index 0 was reachable only by landing inside
 * the first tab's left half, and missing it silently sent the pane to the end.
 *
 * `null` only for a strip with no tabs at all, where there is no caret to draw
 * and appending is the whole answer.
 */
function caretAt(strip: Element, x: number): { tabId: string; after: boolean } | null {
  let last: { tabId: string; after: boolean } | null = null;
  for (const el of strip.querySelectorAll<HTMLElement>("[data-tab-id]")) {
    const id = el.dataset.tabId;
    if (!id) continue;
    const box = el.getBoundingClientRect();
    if (x < box.left + box.width / 2) return { tabId: id, after: false };
    last = { tabId: id, after: true };
  }
  return last;
}
import {
  focusTerminal,
  mountTerminal,
  reconnectTerminal,
  releaseTerminal,
  restartTerminal,
  startTerminal,
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
      // `unhandledKind` returns `never`, so the missing `return` is deliberate:
      // this branch never completes normally, and a bare call keeps the final
      // statement's value from tripping `noVoidTypeReturn` on a `void` function.
      unhandledKind(tab.kind);
  }
}

/**
 * What the app can do to this pane area without a pointer.
 *
 * Every member is the **same call the equivalent mouse affordance makes** — the
 * tab button's `onClick`, the tab's close button, the strip's `+`. That is the
 * whole design: a keyboard chord and a menu accelerator get whatever a click
 * does, by construction, rather than by a second copy of the logic in `App.tsx`
 * that has to be kept in step with this file.
 *
 * The history is worth keeping. #315's tab-cycling re-derived activation in
 * `App.tsx`, could not reproduce what a click did, and ended up dispatching a
 * literal `.focus()` + `.click()` at the real button to get it — a workaround
 * for a root cause never found. There was nothing to find: the tab button's
 * handler is exactly `onLayout(activateTab(layout, id))`, so the two were the
 * same call all along. A handle makes that identity structural instead of
 * something to re-verify.
 */
export interface PaneAreaHandle {
  /** Make `id` the active tab in its own dock and put real DOM focus on its
   *  strip button — exactly what clicking that button does. */
  selectTab(id: string): void;
  /**
   * Close the focused dock's active tab, through the same confirmation a busy
   * terminal gets from its × button.
   *
   * Returns whether there *was* a tab to close. `false` is what lets ⌘W fall
   * back to closing the window, so that the chord always does something — it
   * used to be Electron's `close` role and closed the window from any state, and
   * "does nothing at all" is the one outcome that trade cannot justify.
   */
  closeActiveTab(): boolean;
  /** Open a `new` chooser pane in the focused dock — the strip's `+`. */
  newTab(): void;
  /**
   * Put something on the other side of the dock.
   *
   * **Two behaviours, because "split" means two things depending on what is
   * there.** With more than one tab in the focused half, it *moves* the active
   * one across — what the tab's context menu and a double-click on its label do.
   * With only one tab there is nothing to move (moving it would just swap which
   * side the single pane sits on), so it opens a **new** pane on the other side
   * instead — what the `+` button's "Open to the side" does.
   *
   * So the chord always produces a split, which is what someone pressing it
   * wants; the alternative was a shortcut that silently did nothing in the most
   * common starting state of all, a worktree with one tab.
   */
  splitActiveTab(): void;
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
  /** The project's own links from `ide.quicklinks`, shown beside the veld URLs. */
  quicklinks: Quicklink[];
  /** The worktree's recently-edited viewable files, newest first. */
  files: ViewableFile[];
  /** Whether that list is still being fetched **for this worktree**. Distinct from
   *  "empty": switching worktrees must not render the previous one's files, so the
   *  app reports not-yet rather than handing over a stale list. */
  filesLoading: boolean;
  /** `files.watchByDefault` — whether a pane opened on a file starts watching it. */
  watchFilesByDefault: boolean;
  /** Pane types the project declares in `ide.panes`. */
  panes: PaneSpec[];
  /** Which of a worktree's config panes the daemon has a token for, **carrying
   *  the worktree it answers for**.
   *
   *  Compared against `worktreeId` during render rather than cleared by an
   *  effect: a switch to another worktree must read as "not known yet", and an
   *  effect cannot un-render the commit that already mounted the pane. `null`
   *  before the first answer arrives. */
  paneSessions: PaneSessionAnswer | null;
  /** Why there are none — only the app knows (no run, or no veld.json). */
  urlsEmptyHint: string;
  /** Browser sessions: the set that exists for this worktree, and how to add or
   *  remove one. */
  sessions: BrowserProfile[];
  onAddSession: ((tabId: string) => void) | undefined;
  onRemoveSession: (profile: BrowserProfile) => void;
  /** Which one-click toggles a browser pane's chrome shows. */
  quickSwitches: QuickSwitchPrefs;
  /** `activity.showWorking` — whether a tab shows a spinner while its pane is busy. */
  showWorking: boolean;
  /** The selected worktree's run, for the `logs` and `nodes` panes. */
  runCtx: RunPaneContext;
  /**
   * `browser.searchUrl` — where a pane's address bar sends words that are not an
   * address, or `""` for nowhere. Read once by the app and passed down rather than
   * re-read per pane: every pane in every dock resolves the same setting, and a
   * component that fetches it is a component that renders before it arrives.
   */
  searchUrl: string;
  /**
   * Filled with [`PaneAreaHandle`] — how the app drives the tab strip from a
   * keyboard chord or a menu accelerator.
   *
   * A handle rather than three more props calling back up, because the point is
   * that these are *the same functions the mouse affordances call*, not a
   * second implementation of them living in `App.tsx`. `requestClose`'s
   * busy-terminal confirmation, in particular, is state local to this component
   * and cannot be reached any other way.
   */
  handleRef?: MutableRefObject<PaneAreaHandle | null>;
  /**
   * Whether this component is on screen at all.
   *
   * The app advertises its tab strip to the desktop shell so other windows can
   * cycle into it, and that advertisement is only true while there is a strip
   * being drawn. `PaneArea` is behind several conditions in `App.tsx` — the
   * IDE/Runs view switch, `claimBlocked`, having a worktree and a layout — and
   * re-deriving them at the push site is the drift this reports away: a window
   * in Runs view kept advertising four tabs, so cycling raised it and asked a
   * `PaneAreaHandle` that no longer existed to activate one.
   */
  onMounted?: (mounted: boolean) => void;
}) {
  const { layout, onLayout } = props;
  const areaRef = useRef<HTMLDivElement>(null);
  const bothVisible = dockVisible(layout, 0) && dockVisible(layout, 1);
  /**
   * The same fact, readable from a closure captured once at mount.
   *
   * `resolveRemote` is registered with the shell in a `[]` effect, so it closes
   * over the first render — reading `bothVisible` there directly would freeze
   * the split-mode decision at mount. The ref carries the live value instead,
   * the same pattern `dragOutsideRef` uses for the same reason.
   */
  const bothVisibleRef = useRef(bothVisible);
  bothVisibleRef.current = bothVisible;

  /**
   * A terminal tab whose close is waiting on a confirmation, or `null`.
   *
   * Only ever set when the tab is a terminal *with a foreground job* — an idle
   * terminal closes without being asked. Clearing it on the next layout change
   * would dismiss the dialog if the tab is closed by another path meanwhile;
   * it is not cleared here because the modal's own Cancel is the dismissal.
   */
  const [pendingClose, setPendingClose] = useState<string | null>(null);

  /**
   * Close a tab — asking first when it is a terminal running a foreground job.
   *
   * A terminal is not re-creatable state: closing it hangs up the shell and
   * anything running in it, so a real terminal asks before doing that. This is
   * the same signal, derived on demand by the daemon (`/api/pty/sessions/{id}/busy`);
   * only a genuinely busy terminal interrupts the close with a dialog.
   *
   * Every other case closes immediately — a non-terminal kind, an idle
   * terminal, or a terminal the daemon cannot answer for (the session is gone,
   * or its holder predates the busy query). An unknown answer never blocks a
   * close. The busy check is async, so the close uses a functional update to
   * read the layout as it is when it lands, not as it was when the click fired.
   */
  const requestClose = (tabId: string) => {
    const dock = dockOf(layout, tabId);
    const tab =
      dock === null ? null : layout.docks[dock].tabs.find((t) => t.id === tabId);
    if (tab?.kind !== "terminal") {
      onLayout(closeTab(layout, tabId));
      return;
    }
    api
      .ptyBusy(tabId)
      .then(({ busy }) => {
        if (busy) setPendingClose(tabId);
        else onLayout((prev) => closeTab(prev, tabId));
      })
      .catch(() => onLayout((prev) => closeTab(prev, tabId)));
  };

  // Told once each way, so the app can stop advertising a tab strip the moment
  // this stops drawing one. Depending on `onMounted` rather than on nothing is
  // the honest spelling — a `setState` identity is stable, so it re-runs only if
  // a caller ever passes something that is not one, which is exactly when a
  // fixed `[]` would silently keep calling the previous callback.
  const onMounted = props.onMounted;
  useEffect(() => {
    onMounted?.(true);
    return () => onMounted?.(false);
  }, [onMounted]);

  /**
   * The keyboard/menu entry points — see [`PaneAreaHandle`] for why they live
   * here rather than in `App.tsx`.
   *
   * No dependency array: each member closes over this render's `layout`, which
   * is what the mouse handlers beside them do too, and a stale `layout` here
   * would activate a tab that has since moved dock or close one that has gone.
   */
  useImperativeHandle(props.handleRef, () => ({
    selectTab: (id: string) => {
      // Not just a guard against a stale id: `activateTab` returns the layout
      // unchanged for an id it cannot find, so without this the DOM focus below
      // would move to a button belonging to some other worktree's strip.
      if (!hasTab(layout, id)) return;
      // **Focus first, activate second — the order a real mouse click produces**,
      // because the browser's default mousedown action moves focus before the
      // `click` event fires. Doing it the other way round is not a style
      // difference: focusing the button runs the dock's `focusin` handler, so
      // whatever that handler writes lands *after* whatever is written here.
      // Activation therefore has to be the last word, exactly as it is for a
      // click. (That handler is now an updater and no longer clobbers anything,
      // so this ordering is belt to its braces — but the braces are one edit
      // away from being loosened again, and this is the half a reader can see.)
      //
      // The `:focus-within` "focused pane" border reads *real* DOM focus, not
      // `layout.focused`, which is why the focus call is needed at all. The
      // button already exists — the tab is in the strip being read — so nothing
      // has to wait for a commit.
      document.getElementById(tabElementId(id))?.focus();
      // The updater form for the same reason the `focusin` handler above uses
      // one: this runs from a keyboard chord and from a cross-window IPC push,
      // neither of which is guaranteed to be the only writer in its tick.
      onLayout((prev) => activateTab(prev, id));
    },
    closeActiveTab: () => {
      const id = layout.docks[layout.focused].activeId;
      // `requestClose`, never `closeTab`: a terminal running a foreground job
      // gets the same confirmation from ⌘W that it gets from its × button.
      // Closing a running agent's shell because a chord was faster than a click
      // is the one outcome here worth being careful about.
      if (!id) return false;
      requestClose(id);
      return true;
    },
    splitActiveTab: () => {
      // Minted once, outside the updater, for the same reason `newTab` does it:
      // an updater may run more than once for a single write. Unused on the move
      // branch, which costs a discarded uuid and nothing else.
      const fresh = newPaneTab();
      // **Everything derived from `prev`, not from this render's `layout`.** The
      // value form would commit a snapshot taken before any writer that landed
      // in the same tick — a browser pane reporting a navigation, say — and
      // persist the tab with its previous URL. That is the exact write-shape
      // `paneAreaContract.test.ts` exists to police in this file.
      onLayout((prev) => {
        const id = prev.docks[prev.focused].activeId;
        // Branch on the model's own answer: `moveTabToOtherDock` returns the
        // *same object* when there is nothing to move, so identity is the exact
        // test for "that tab is alone in its half" and stays correct if the rule
        // ever changes.
        const moved = id ? moveTabToOtherDock(prev, id) : prev;
        if (moved !== prev) return moved;
        return addTab(prev, (prev.focused === 0 ? 1 : 0) as DockIndex, fresh);
      });
    },
    newTab: () => {
      // Minted once, outside the updater: an updater may be invoked more than
      // once for a single write (React re-runs them in StrictMode and on a
      // rebase), and a fresh id each time would open a different tab than the
      // one that ends up committed.
      const tab = newPaneTab();
      onLayout((prev) => addTab(prev, prev.focused, tab));
    },
  }));

  /** The terminal pending confirmation, for the dialog's label. */
  const pendingTab = pendingClose
    ? ([0, 1] as DockIndex[])
        .flatMap((i) => layout.docks[i].tabs)
        .find((t) => t.id === pendingClose)
    : null;

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
  const [remote, setRemote] = useState<RemoteTarget>(null);

  /**
   * The same target, kept **past the end of the drag** so the drop can commit it.
   *
   * `remote` drives the indicator, so it has to clear the moment the gesture
   * stops — and the shell ends the drag (every window thaws) *before* it tells
   * this one that tabs landed here. Reading the rendered value at that point
   * found it already cleared, and every cross-window drop fell back to
   * appending: the split and the caret were shown honestly and then ignored.
   *
   * Same rule as `lastOverId` in the shell, one level down. The drag resolves
   * the target continuously; the drop commits what it resolved; only the *next*
   * drag invalidates it.
   */
  const lastRemoteRef = useRef<RemoteTarget>(null);
  const setRemoteTarget = (v: RemoteTarget) => {
    lastRemoteRef.current = v;
    setRemote(v);
  };

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
      setRemoteTarget(null);
      return;
    }
    const dockEl = el.closest<HTMLElement>("[data-dock]");
    const dock = (dockEl ? Number(dockEl.dataset.dock) : 0) as DockIndex;

    // Anywhere in the strip resolves to a caret — over a tab, over the padding
    // beside it, or past the last one. See `caretAt` for why the element under
    // the pointer is the wrong question to ask here.
    const strip = el.closest(".pane-tabs");
    if (strip) {
      const at = caretAt(strip, x);
      setRemoteTarget(at ? { at: { dock, ...at } } : { zone: { where: "into", dock } });
      return;
    }
    setRemoteTarget({ zone: zoneAt(area.getBoundingClientRect(), x, dock, !bothVisibleRef.current) });
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
    const offBegin = shell.onDragBegin(() => {
      beginTabDrag();
      // A new gesture is the only thing that invalidates the last one's target.
      setRemoteTarget(null);
    });
    const offEnd = shell.onDragEnd(() => {
      endTabDrag();
      // The indicator goes; the *target* is kept for the `drop-here` that may be
      // right behind this — see `lastRemoteRef`.
      setRemote(null);
    });
    const offOver = shell.onDragOver(({ x, y }) => resolveRemote(x, y));
    // The pointer left this window, so nothing here is the target any more —
    // and nothing will be delivered here either.
    const offOut = shell.onDragOut(() => setRemoteTarget(null));
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
   *
   * **The shell is told when this listener exists**, because its own answer to
   * "which window owns this worktree" is a claim, and a claim is recorded the
   * moment a window *asks* for a worktree — long before `/ide` has mounted, and
   * again through every reload. Told, it queues a drop for the gap instead of
   * pushing into it and reporting a refusal two seconds later.
   */
  useEffect(() => {
    const shell = desktopWindow;
    if (!shell) return;
    const off = shell.onDropHere(({ dropId, tabs }) => {
      const parsed = parseTransferTabs(tabs);
      if (parsed.length === 0) {
        void shell.dropApplied(dropId, []).catch(() => {});
        return;
      }
      // **Reserve before applying, and apply only if the reservation held.**
      // The acknowledgement is what makes the source let go, so acking and
      // inserting unconditionally left a window in which both happened: past
      // the 2s deadline the source keeps its tabs *and* this window has already
      // inserted them, so one tab id lives in two windows and both attach the
      // same shell — the very outcome the protocol was added to prevent, with
      // the failure moved rather than removed. Awaiting the answer makes the
      // two decisions one.
      void (async () => {
        const held = await shell.dropApplied(dropId, parsed.map((t) => t.id)).catch(() => false);
        if (!held) return;
        applyDrop(parsed);
      })();
    });
    // After the listener, never before: the window between saying "ready" and
    // being ready is one where the shell pushes at nothing, which is the state
    // this exists to remove.
    void shell.dropsReady?.(true).catch(() => {});
    return () => {
      // …and withdrawn before it goes, for the same reason in reverse.
      void shell.dropsReady?.(false).catch(() => {});
      off();
    };

    function applyDrop(parsed: PaneTab[]) {
      const where = lastRemoteRef.current;
      setRemoteTarget(null);
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
    }
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
    const next = zoneAt(area, e.clientX, dock, !bothVisible);
    setDropZone((prev) => (sameZone(prev, next) ? prev : next));
  };

  const onBodyDrop = (e: React.DragEvent, dock: DockIndex) => {
    const id = e.dataTransfer.getData(TAB_MIME);
    const area = areaRef.current?.getBoundingClientRect();
    setDropZone(null);
    endTabDragEverywhere();
    if (!id || !area) return;
    e.preventDefault();
    const zone = zoneAt(area, e.clientX, dock, !bothVisible);
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
          quicklinks={props.quicklinks}
          files={props.files}
          filesLoading={props.filesLoading}
          panes={props.panes}
          urlsEmptyHint={props.urlsEmptyHint}
          searchUrl={props.searchUrl}
          onTerminal={() =>
            onLayout(addTab(layout, 0, { id: newTabId(), kind: "terminal", title: "Terminal" }))
          }
          onPane={(spec) => onLayout(addTab(layout, 0, configPaneTab(spec)))}
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
              requestClose={requestClose}
              worktreeId={props.worktreeId}
              serviceUrls={props.serviceUrls}
              quicklinks={props.quicklinks}
              files={props.files}
              filesLoading={props.filesLoading}
              watchFilesByDefault={props.watchFilesByDefault}
              panes={props.panes}
              paneSessions={props.paneSessions}
              urlsEmptyHint={props.urlsEmptyHint}
              sessions={props.sessions}
              onAddSession={props.onAddSession}
              onRemoveSession={props.onRemoveSession}
              quickSwitches={props.quickSwitches}
              showWorking={props.showWorking}
              runCtx={props.runCtx}
              searchUrl={props.searchUrl}
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

      {/* Confirm before hanging up a shell that is running a foreground job.
          Portalled, so overlayGuard hides any embedded browser pane underneath
          it the way it does every other Mantine modal. */}
      {pendingTab && pendingTab.kind === "terminal" && (
        <Modal
          opened
          onClose={() => setPendingClose(null)}
          title="Close this terminal?"
          centered
        >
          <Text size="sm">
            A process is still running in “{pendingTab.title}”. Closing the terminal will
            hang up that process. Close anyway?
          </Text>
          <div
            style={{
              display: "flex",
              justifyContent: "flex-end",
              gap: 8,
              marginTop: 20,
            }}
          >
            <Button variant="default" onClick={() => setPendingClose(null)}>
              Cancel
            </Button>
            <Button
              color="red"
              onClick={() => {
                const id = pendingTab.id;
                setPendingClose(null);
                onLayout((prev) => closeTab(prev, id));
              }}
            >
              Close terminal
            </Button>
          </div>
        </Modal>
      )}
    </div>
  );
}

function DockView(props: {
  index: DockIndex;
  width: string;
  layout: PaneLayout;
  onLayout: (next: PaneLayoutUpdate) => void;
  /** Close a tab, confirming first if a terminal has a foreground job. */
  requestClose: (tabId: string) => void;
  worktreeId: number;
  serviceUrls: Array<[string, string]>;
  /** The project's own links from `ide.quicklinks`, shown beside the veld URLs. */
  quicklinks: Quicklink[];
  /** The worktree's recently-edited viewable files, newest first. */
  files: ViewableFile[];
  /** Whether that list is still being fetched **for this worktree**. Distinct from
   *  "empty": switching worktrees must not render the previous one's files, so the
   *  app reports not-yet rather than handing over a stale list. */
  filesLoading: boolean;
  /** `files.watchByDefault` — whether a pane opened on a file starts watching it. */
  watchFilesByDefault: boolean;
  /** Pane types the project declares in `ide.panes`. */
  panes: PaneSpec[];
  /** Which of a worktree's config panes the daemon has a token for, **carrying
   *  the worktree it answers for**.
   *
   *  Compared against `worktreeId` during render rather than cleared by an
   *  effect: a switch to another worktree must read as "not known yet", and an
   *  effect cannot un-render the commit that already mounted the pane. `null`
   *  before the first answer arrives. */
  paneSessions: PaneSessionAnswer | null;
  urlsEmptyHint: string;
  sessions: BrowserProfile[];
  onAddSession: ((tabId: string) => void) | undefined;
  onRemoveSession: (profile: BrowserProfile) => void;
  quickSwitches: QuickSwitchPrefs;
  showWorking: boolean;
  runCtx: RunPaneContext;
  /** `browser.searchUrl`, for every pane in this dock's address bar. */
  searchUrl: string;
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
  // The tabs carry an unseen dot, so this strip has to re-render when the inbox does.
  // Without it a pane's dot appears only when something else happens to re-render the
  // dock — which for a pane nobody is touching is "never".
  useInbox();
  // Only counts for the worktree it was fetched for — see `paneAnswerFor`,
  // which is where the reasoning and the tests live.
  const paneAnswer = paneAnswerFor(props.paneSessions, props.worktreeId);
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
        onClick: () => props.requestClose(tab.id),
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
      // **The updater form, not `focusDock(layout, index)`.** This is `focusin`,
      // so it fires for anything focused anywhere inside the dock — including a
      // focus moved *by code that has just written to the layout in the same
      // tick*. A value computed from this render's `layout` would then be
      // committed on top of that write and silently undo it, which is precisely
      // what `PaneLayoutUpdate`'s own doc comment warns about.
      //
      // It cost a release. Keyboard tab-cycling activated a tab and then moved
      // DOM focus onto it, and this handler put the pre-activation layout back:
      // the tab took the focus outline and never opened. A mouse click is immune
      // by accident of ordering — the browser focuses on mousedown and fires
      // `click` after, so there the activation is the *second* write — which is
      // why the same bug in #315 read as "a real click works, re-deriving one
      // does not" and was shipped around rather than found.
      onFocus={() => onLayout((prev) => focusDock(prev, index))}
      aria-label={index === 0 ? "Primary pane" : "Secondary pane"}
    >
      <div
        className="pane-tabs"
        // The **whole strip** resolves to a caret, not just the tabs in it: the
        // padding either side of them is exactly where the first and last
        // positions are aimed at, and treating it as "append" made index 0
        // reachable only by hitting the first tab's left half. Also the only way
        // to move a tab into a dock that has none, where there is no caret and
        // appending is the answer.
        onDragOver={(e) => {
          if (!e.dataTransfer.types.includes(TAB_MIME)) return;
          e.preventDefault();
          // The strip and the body are two halves of one drop model, so entering
          // the strip has to retract the body's preview — otherwise a drag that
          // crossed the body leaves a highlighted region behind it.
          props.onClearZone();
          const at = caretAt(e.currentTarget, e.clientX);
          setDropAt(at ? { id: at.tabId, after: at.after } : null);
        }}
        onDrop={(e) => {
          const at = caretAt(e.currentTarget, e.clientX);
          if (!at) {
            dropTab(e);
            return;
          }
          const dragged = e.dataTransfer.getData(TAB_MIME);
          const target = dock.tabs.findIndex((t) => t.id === at.tabId);
          const from = dock.tabs.findIndex((t) => t.id === dragged);
          // `moveTab` counts the destination *after* the tab is removed, which
          // is what a caret between two tabs means — so a move within this dock
          // from the left shifts everything after it down by one.
          const removedBefore = from >= 0 && from < target;
          dropTab(e, target + (at.after ? 1 : 0) - (removedBefore ? 1 : 0));
        }}
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
            icon={tabIcon(tab, props.panes)}
            selected={tab.id === dock.activeId}
            panelId={dockPanelId(index)}
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
            onClose={() => props.requestClose(tab.id)}
            onMove={() => onLayout(moveTabToOtherDock(layout, tab.id))}
            onMenu={(e) => tabMenu(tab)(e)}
            canMove={dock.tabs.length > 1}
            canDetach={props.onDetach !== undefined}
            activity={paneActivity(tab.id, props.showWorking)}
            activityDetail={inbox.unseen(tab.id)?.detail ?? null}
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
            {/* The project's own panes as their own labelled group, after the
                four veld ships. Ungrouped, they read as more built-ins and the
                menu's shape changed per checkout; a label says whose they are
                and where to go to change them. Unavailable ones are dropped
                here rather than shown disabled — a hover menu is the one-click
                path, and an entry that cannot be clicked has no business in it
                (the chooser still lists them, with the reason). */}
            {props.panes.some((spec) => spec.available) && (
              <>
                <Menu.Divider />
                <Menu.Label>Project panes</Menu.Label>
                {props.panes
                  .filter((spec) => spec.available)
                  .map((spec) => (
                    <Menu.Item
                      key={spec.id}
                      leftSection={paneIcon(spec.icon, 14)}
                      onClick={() => convertOrAdd(active, configPaneTab(spec))}
                    >
                      New {spec.label} pane
                    </Menu.Item>
                  ))}
              </>
            )}
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
        id={dockPanelId(index)}
        // The other half of the tab relationship. Both attributes are conditional
        // on there being an active tab: an empty dock shows the chooser, which is
        // no tab's panel and has no tab to be labelled by.
        role={active ? "tabpanel" : undefined}
        aria-labelledby={active ? tabElementId(active.id) : undefined}
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
            quicklinks={props.quicklinks}
            files={props.files}
            filesLoading={props.filesLoading}
            panes={props.panes}
            urlsEmptyHint={props.urlsEmptyHint}
            searchUrl={props.searchUrl}
            // A `new` tab becomes the chosen kind in place; an empty dock has no
            // tab to convert, so it gets a fresh one.
            onTerminal={() =>
              convertOrAdd(active, { id: newTabId(), kind: "terminal", title: "Terminal" })
            }
            onPane={(spec) => convertOrAdd(active, configPaneTab(spec))}
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
            quicklinks={props.quicklinks}
            files={props.files}
            filesLoading={props.filesLoading}
            worktreeId={props.worktreeId}
            watchFilesByDefault={props.watchFilesByDefault}
            urlsEmptyHint={props.urlsEmptyHint}
            sessions={props.sessions}
            onAddSession={
              props.onAddSession && (() => props.onAddSession?.(active.id))
            }
            onRemoveSession={props.onRemoveSession}
            quickSwitches={props.quickSwitches}
            searchUrl={props.searchUrl}
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
            spec={
              active.spec ? props.panes.find((p) => p.id === active.spec) : undefined
            }
            specId={active.spec}
            resumable={paneAnswer?.resumable.has(active.id) ?? false}
            panesKnown={paneAnswer !== null}
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
 *
 * **Grouped by what you are trying to do, not by who wrote it.** The previous shape
 * was a centred stack of same-weight buttons — veld's four first, the project's
 * panes third under a caption, the run's URLs last — and a first user test found
 * every failure that shape invites. People opened a Claude pane by pressing
 * `Terminal`, because Claude was in a different section further down instead of
 * beside the thing it is an alternative to. People pressed `Browser` and never
 * connected it to their app's URL five rows below. The order was chosen so that
 * Terminal would not move between checkouts, which optimises for veld's consistency
 * over what the person is there to do; that trade is reversed here.
 *
 * Three groups, always in this order, so nothing a user hunts for changes position
 * between projects: what to *work in*, where to *look*, and what to check when
 * something is wrong. Membership inside a group varies with the project — that is the
 * part a config is allowed to change.
 */
function PaneChooser(props: {
  serviceUrls: Array<[string, string]>;
  /** The project's own links from `ide.quicklinks`, shown beside the veld URLs. */
  quicklinks: Quicklink[];
  /** The worktree's recently-edited viewable files, newest first. */
  files: ViewableFile[];
  /** Whether that list is still being fetched **for this worktree**. Distinct from
   *  "empty": switching worktrees must not render the previous one's files, so the
   *  app reports not-yet rather than handing over a stale list. */
  filesLoading: boolean;
  /** Pane types the project declares in `ide.panes`. */
  panes: PaneSpec[];
  urlsEmptyHint: string;
  /** `browser.searchUrl`, only to say whether a blank pane can search. */
  searchUrl: string;
  onTerminal: () => void;
  onPane: (spec: PaneSpec) => void;
  onBrowser: (tab: PaneTab) => void;
  onDiag: (kind: DiagKind) => void;
}) {
  const [bookmarksOpen, setBookmarksOpen] = useState(false);
  const [filesOpen, setFilesOpen] = useState(false);
  // **Two lists, deliberately.** `places` is what the screen shows unprompted, so
  // its files are the handful `inlineFiles` allows — at most three, from the last
  // day, and none at all while the run is serving URLs of its own. `filePlaces` is
  // every file, for the modal behind the Files button, which is where the search
  // field lives. Building the inline list from the full one would have made the
  // screen a directory listing; building the modal from the inline one would have
  // made the search field a lie.
  const places = placesFor(
    props.serviceUrls,
    props.quicklinks,
    inlineFiles(props.files, {
      hasRunUrls: props.serviceUrls.length > 0,
      now: Date.now(),
    }),
  );
  const fileplaces = placesFor([], [], props.files);
  // No query here — the chooser has no field of its own, so the list is always the
  // run's URLs and the bookmarks are always the ones behind the button.
  const suggestions = suggestionsFor(places, "", props.searchUrl);
  const bookmarks = places.filter((p) => p.kind === "bookmark");
  return (
    <div className="pane-chooser">
      {/* Cards of one size, in declaration order, with a plain shell as the last of
          them. **Nothing is promoted.** The first cut of this gave the first declared
          pane a full-width lead button, which read as a recommendation veld has no
          business making: a repo that declares Claude, Pi, Codex and a git log has
          four things a contributor might want, and picking one for them is a guess
          dressed as a default. Equal cards let each one carry its own description
          instead, which is the information that actually tells them apart. */}
      <section className="chooser-group">
        <h3 className="chooser-heading">
          <IconTerminal2 size={16} /> Work in a terminal
        </h3>
        <div className="chooser-cards">
          {/* An unavailable pane is shown disabled with the reason rather than
              omitted — a repo that declares a Claude pane should not look like it
              forgot to. */}
          {props.panes.map((spec) => (
            <PaneButton key={spec.id} spec={spec} onPick={props.onPane} />
          ))}
          <button className="pane-card" onClick={props.onTerminal}>
            <span className="pane-card-main">
              <IconTerminal2 size={15} /> Terminal
            </span>
            <span className="pane-card-sub">A shell in this worktree</span>
          </button>
        </div>
      </section>

      {/* The run's URLs as the list, with the two escape hatches in the heading.
          Emphatically *not* a `Browser` button up in the first group with the URLs
          somewhere below: that split is what made "open my app" undiscoverable, and
          the reason these two controls are allowed to be buttons is that they sit
          *on* the list they are alternatives to rather than in another section.

          Blank was a dashed row at the bottom and the bookmarks were a second group
          above it. In a project with four to eight services per run that put the
          addresses veld is serving *now* — the answer, most of the time — in the
          middle of a screen that scrolled, between a config's bookmarks and a row
          nobody was looking for. */}
      <section className="chooser-group">
        <div className="chooser-head">
          <h3 className="chooser-heading">
            <IconWorld size={16} /> Open a page
          </h3>
          <div className="chooser-head-actions">
            {/* Icon-only, both of these. They are peers — "files this worktree has"
                and "addresses this project declared" — and a heading row with two
                labelled buttons plus Blank browser had no room left for the third.
                Never disabled, even with none to show: a disabled button dispatches
                no pointer events, so its tooltip can never open (#205), and each
                modal's own empty state is where the absence gets explained. */}
            <FilesButton
              count={props.files.length}
              loading={props.filesLoading}
              onOpen={() => setFilesOpen(true)}
            />
            <Tooltip
              label="Every address this project declares"
              openDelay={250}
              withArrow
            >
              <ActionIcon
                variant="default"
                size="sm"
                aria-label={`Project bookmarks (${bookmarks.length})`}
                onClick={() => setBookmarksOpen(true)}
              >
                <IconBookmark size={13} />
              </ActionIcon>
            </Tooltip>
            <Button
              size="compact-xs"
              variant="default"
              leftSection={<IconWindow size={13} />}
              title={
                props.searchUrl.trim() === ""
                  ? "A browser pane with nothing loaded — type any address"
                  : "A browser pane with nothing loaded — type any address, or search"
              }
              onClick={() => props.onBrowser(browserTab({}))}
            >
              Blank browser
            </Button>
          </div>
        </div>
        <PlaceList
          suggestions={suggestions}
          emptyHint={props.urlsEmptyHint}
          onOpen={(url, title, path) =>
            props.onBrowser(
              browserTab({ url, title: path ? fileLabel(path) : title, path }),
            )
          }
        />
        {/* Said out loud, and only where the answer is the main content of this part
            of the screen — with a run serving URLs there are rows above and the
            heading's spinner is enough. This exists because the same frame used to
            render the *previous* worktree's files: they are now correctly absent, and
            absent-because-not-answered has to look different from absent-because-none
            or a switch reads as "this worktree has nothing". */}
        {props.filesLoading && props.serviceUrls.length === 0 && (
          <p className="faint place-nomatch">Looking for recently edited files…</p>
        )}
      </section>
      <FilesModal
        files={fileplaces}
        opened={filesOpen}
        onClose={() => setFilesOpen(false)}
        onOpen={(url, title, path) => {
          setFilesOpen(false);
          props.onBrowser(
            browserTab({ url, title: path ? fileLabel(path) : title, path }),
          );
        }}
      />
      <BookmarksModal
        bookmarks={bookmarks}
        opened={bookmarksOpen}
        onClose={() => setBookmarksOpen(false)}
        // The chooser's job is to become something, so a picked bookmark converts this
        // pane exactly as a row does — and the modal has served its purpose either way.
        onOpen={(url, title) => {
          setBookmarksOpen(false);
          props.onBrowser(browserTab({ url, title }));
        }}
      />

      {/* Last, but the same cards as the terminals above: these are panes you sit in
          front of and arrange beside a shell, not toolbar actions, and rendering them
          as small chips made them look like a lesser class of thing. The run's node
          actions used to sit here as a fourth group; they are gone, because the top
          bar carries them permanently and a screen this crowded cannot afford the
          same surface twice. */}
      <section className="chooser-group">
        <h3 className="chooser-heading">
          <IconActivityHeartbeat size={16} /> Check the run
        </h3>
        <div className="chooser-cards">
          <button className="pane-card" onClick={() => props.onDiag("logs")}>
            <span className="pane-card-main">
              <IconLogs size={15} /> Logs
            </span>
            <span className="pane-card-sub">Every node's output, interleaved</span>
          </button>
          <button className="pane-card" onClick={() => props.onDiag("nodes")}>
            <span className="pane-card-main">
              <IconActivityHeartbeat size={15} /> Nodes
            </span>
            <span className="pane-card-sub">Health, CPU and memory per node</span>
          </button>
        </div>
      </section>
    </div>
  );
}

/** One config-declared pane, as a card of the same size as every other. */
function PaneButton(props: { spec: PaneSpec; onPick: (spec: PaneSpec) => void }) {
  const { spec } = props;
  const missing = `${spec.label} needs ${(spec.missing ?? []).join(", ")} — not found on your PATH`;
  return (
    <button
      className="pane-card"
      // `aria-disabled`, not `disabled`. A disabled button dispatches no pointer
      // events, so its native tooltip can never open — the trap #205 already paid
      // for — and the reason a pane is unavailable lives *only* in that tooltip,
      // since the `+` menu drops unavailable panes entirely. So the button stays
      // interactive to the browser, refuses the click itself, and can explain why.
      aria-disabled={!spec.available || undefined}
      title={spec.available ? (spec.description ?? `Open a ${spec.label} pane`) : missing}
      onClick={() => {
        if (spec.available) props.onPick(spec);
      }}
    >
      <span className="pane-card-main">
        {paneIcon(spec.icon, 15)} {spec.label}
        {/* This is what teaches a contributor that a pane is worth keeping open:
            `resume`/`auto_resume` are config fields most people would never open
            `veld.json` to discover otherwise. Auto gets its own glyph rather than
            sharing one with a qualifier, because the two are genuinely different
            promises — the Codex pane below is resumable but never auto, and the
            distinction is exactly what a glance at the chooser should surface. */}
        {spec.auto_resume ? (
          <Tooltip
            label="Resumes its last session automatically — no click needed"
            withArrow
            openDelay={200}
          >
            {/* `title=""` blocks inheritance of the card button's own `title` —
                without it, hovering the badge fires the fast Mantine tooltip
                *and*, after the browser's native delay, the button's title
                (the pane's description), stacked over the same spot. `tabIndex`
                makes the badge focusable, which is what lets Mantine's Tooltip
                show it on keyboard focus too, not only on mouse hover. */}
            <span
              className="pane-resume-badge auto"
              title=""
              tabIndex={0}
              aria-label="Resumes its last session automatically — no click needed"
            >
              <IconBolt size={13} />
            </span>
          </Tooltip>
        ) : spec.can_resume ? (
          <Tooltip
            label="Can resume its last session — offered as a choice, not automatic"
            withArrow
            openDelay={200}
          >
            <span
              className="pane-resume-badge"
              title=""
              tabIndex={0}
              aria-label="Can resume its last session — offered as a choice, not automatic"
            >
              <IconHistory size={13} />
            </span>
          </Tooltip>
        ) : null}
      </span>
      {/* Every card carries its second line, not only a chosen one: the description
          is what tells four agent panes apart, and giving it to one of them is how
          the previous cut ended up looking like a recommendation. An unavailable
          pane spends the line on why instead. */}
      <span className="pane-card-sub">
        {spec.available ? (spec.description ?? "") : missing}
      </span>
    </button>
  );
}

/**
 * A tab's kind, as a glyph.
 *
 * For a browser pane the glyph carries the session colour rather than sitting
 * beside a separate coloured dot: two markers for one fact is noise at tab size,
 * and a tinted globe says both things at once.
 */
function tabIcon(tab: PaneTab, panes: PaneSpec[]): React.ReactNode {
  switch (tab.kind) {
    case "terminal": {
      // A config-declared pane keeps its own glyph, so two Claude tabs and a
      // shell are tellable apart at tab size. A spec the project has since
      // removed falls back to the terminal glyph rather than to nothing.
      const spec = tab.spec ? panes.find((p) => p.id === tab.spec) : undefined;
      return tab.spec ? paneIcon(spec?.icon, 12) : <IconTerminal2 size={12} />;
    }
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

/** The DOM id of a tab's button, and of a dock's panel. Both exist only so the
 *  tab and the panel it controls can name each other. Stays private: keyboard
 *  activation goes through `PaneAreaHandle.selectTab` in this same file, so the
 *  id scheme has no reader outside it — which is the point, since #315 exported
 *  it precisely so `App.tsx` could reimplement a click against it. */
function tabElementId(tabId: string): string {
  return `pane-tab-${tabId}`;
}
function dockPanelId(index: DockIndex): string {
  return `pane-panel-${index}`;
}

/**
 * Keyboard navigation for a tab strip: arrows move, Home/End jump, Delete closes.
 *
 * What each key *means* is `tabKeyAction`, which is pure and tested. This half is
 * the DOM: which tabs there are, and moving focus between them. The strip is
 * walked through the DOM rather than through the layout on purpose — threading an
 * index, the sibling ids and a ref per tab down from the dock reproduces what
 * `role="tablist"` already holds: the tabs of *this* strip, in visual order,
 * including whatever a drag just reordered.
 */
function onTabKeyDown(onClose: () => void) {
  return (e: React.KeyboardEvent<HTMLButtonElement>) => {
    const strip = e.currentTarget.closest('[role="tablist"]');
    const tabs = strip
      ? Array.from(strip.querySelectorAll<HTMLElement>('[role="tab"]'))
      : [e.currentTarget as HTMLElement];
    const action = tabKeyAction(e.key, tabs.indexOf(e.currentTarget), tabs.length);
    if (action.kind === "ignore") return;
    e.preventDefault();
    if (action.kind === "close") {
      // The close button is not a tab stop (see its `tabIndex`), so this is the
      // keyboard's route to closing a pane — the key every tab strip uses.
      onClose();
      return;
    }
    tabs[action.index]?.focus();
  };
}

/**
 * A tab and its close button as siblings, not nested.
 *
 * A `<button>` inside a `<button>` is invalid HTML, and the pattern the
 * worktree rail is stuck with (`div[role=button]` wrapping real controls)
 * hides the inner controls from assistive tech — a known debt from #169. New
 * surfaces shouldn't add to it.
 */
/**
 * One pane's glyph: its unseen event if it has one, else whether it is running.
 *
 * Same precedence the rail uses, one pane wide — an unread event outranks activity,
 * because "this finished" is news and "this is running" is not.
 */
function paneActivity(id: string, showWorking: boolean): RowState | null {
  const unseen = inbox.unseen(id);
  if (unseen) return unseen.kind;
  return showWorking && inbox.isRunning(id) ? "working" : null;
}

function TabButton(props: {
  tab: PaneTab;
  label: string;
  /** The kind's glyph, so a strip of tabs is readable without their titles. */
  icon: React.ReactNode;
  selected: boolean;
  /** The dock's panel, for `aria-controls` on the selected tab. */
  panelId: string;
  /** Which edge to draw a drop indicator on, if any. */
  drop: "before" | "after" | null;
  onSelect: () => void;
  onClose: () => void;
  onMove: () => void;
  onMenu: (e: React.MouseEvent) => void;
  canMove: boolean;
  /** Whether dragging this tab out of the window does anything — Electron only. */
  canDetach: boolean;
  /**
   * What this pane has to say — the tab's half of the rail's glyph: the rail says
   * *that* something happened in the worktree, this says *which pane*.
   *
   * A `RowState` rather than a boolean so it carries the same four-way vocabulary the
   * rail does, `working` included; `null` when the pane has nothing to report.
   */
  activity: RowState | null;
  /** What happened, for the glyph's tooltip. Absent for `working`, which has no event. */
  activityDetail: string | null;
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
        id={tabElementId(props.tab.id)}
        aria-selected={props.selected}
        // Only the selected tab names the panel, because there is one panel per
        // dock and it shows the *active* tab: pointing an unselected tab at it
        // would send a screen reader to content that belongs to another tab.
        aria-controls={props.selected ? props.panelId : undefined}
        // Roving tabindex — a tab strip is **one** tab stop, and the arrows move
        // within it. Before this, Tab walked every tab and every close button in
        // both docks before reaching the pane content.
        tabIndex={props.selected ? 0 : -1}
        className="pane-tab-label"
        aria-description={
          props.activity === null ? undefined : HEADLINE[props.activity]
        }
        onClick={props.onSelect}
        onKeyDown={onTabKeyDown(props.onClose)}
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
          "← → to move between tabs · Delete to close",
          "Right-click for more",
        ].join("\n")}
      >
        <span className="pane-tab-icon" aria-hidden>
          {props.icon}
        </span>
        <span className="pane-tab-text">{props.label}</span>
      </button>
      {/* Which pane the worktree's rail glyph was talking about — the SAME glyph, so
          the two surfaces share one vocabulary instead of having to be learned
          separately. Outside the label button and immediately left of the close
          button: inside it, the icon was part of the button's accessible name and sat
          under the label's `text-overflow: ellipsis`, so a long tab title could clip
          the one thing that says this pane needs you. `aria-hidden`, with the state on
          the label button's `aria-description`. */}
      {props.activity !== null && (
        <PaneActivityIcon state={props.activity} detail={props.activityDetail} />
      )}
      <button
        type="button"
        className="pane-tab-close"
        aria-label={`Close ${props.label}`}
        // Deliberately out of the tab order: a tablist is one tab stop, and with
        // both docks full, tabbing through every close button on the way to the
        // content is worse than the Delete key being the keyboard route (which the
        // tab's tooltip names). Still clickable, and still announced as a button.
        tabIndex={-1}
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
function TerminalPane(props: {
  id: string;
  worktreeId: number;
  /** The project's declaration for this pane, when it is one and the project
   *  still declares it. */
  spec: PaneSpec | undefined;
  /** What the tab says it is, which outlives the declaration. */
  specId: string | undefined;
  /** Whether the daemon holds a session token for this pane. */
  resumable: boolean;
  /** Whether `resumable` has been fetched yet. A config pane must not decide
   *  what to start before this is true. */
  panesKnown: boolean;
  takeFocus: boolean;
}) {
  const { id, worktreeId, spec, specId, resumable, panesKnown } = props;
  // A plain terminal has nothing to look up, so it is ready immediately — and
  // this must be what the mount effect depends on rather than `panesKnown`
  // itself. `panesKnown` goes false→true once per worktree *selection*, so
  // depending on it directly re-ran the effect for plain terminals too:
  // unmount, remount, re-fit (a RESIZE frame, i.e. a redraw for vim or a coding
  // agent) and re-assert focus, on every worktree switch, for a pane that never
  // needed the answer.
  //
  // `existed` is the third case: a session already in the host's registry keeps
  // running whatever it is running, and `ensure` discards the start plan for it
  // entirely — so waiting on an answer that cannot change anything only blanks a
  // *live* agent behind an empty pane for a round trip on every return to the
  // worktree, and indefinitely if that request hangs. Read once per instance, so
  // it stays a constant and cannot reintroduce a remount.
  const [existed] = useState(() => terminalStatus(id).state !== "absent");
  const ready = !specId || panesKnown || existed;
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
    // **Wait for the token lookup before deciding anything.** `ensure` is
    // idempotent, so the start plan is chosen once and never revisited — and
    // this effect runs in the same commit that *starts* the fetch (child
    // effects run before parent ones). Mounting eagerly therefore decided every
    // restored pane against an empty set, which silently made `auto_resume`
    // unreachable in production: the one path the whole feature exists for.
    if (!ready) return;
    // `autoResume` is only ever consulted on this first mount — the
    // materialization edge. A shell that dies later, with the user watching,
    // gets buttons no matter what the config says.
    mountTerminal(
      id,
      worktreeId,
      el,
      specId
        ? {
            spec: specId,
            autoResume: resumable && (spec?.auto_resume ?? false),
            closeOnExit: spec?.close_on_exit ?? false,
            allowTerminalRenaming: spec?.allow_terminal_renaming ?? false,
          }
        : undefined,
    );
    const unsubscribe = subscribeTerminal(id, bump);
    if (takeFocus.current) focusTerminal(id);
    return () => {
      unsubscribe();
      // Detach only. The session (and its shell) outlives this component;
      // `pruneTerminals` in App.tsx is what actually ends one.
      unmountTerminal(id);
    };
    // `spec`/`resumable` are deliberately absent: they decide what the *first*
    // mount does, and re-running because a poll refreshed the pane list would
    // remount a live terminal. `ready` **is** here, because it is the gate
    // above — for a config pane it is the moment the answer for *this* worktree
    // arrives, and for a plain terminal it is a constant `true`.
    // biome-ignore lint/correctness/useExhaustiveDependencies: first-mount decision, see above.
  }, [id, worktreeId, ready]);

  const { state, detail, launched } = terminalStatus(id);
  // Whether the user has asked to see the terminal behind the panel.
  //
  // Reset on every state change, not just on `id`: after a resume the panel is
  // gone anyway, and a pane that died a *second* time must present its own
  // ending rather than inheriting a dismissal from the previous one.
  const [showOutput, setShowOutput] = useState(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `state` is the reset trigger, not a value read here.
  useEffect(() => setShowOutput(false), [id, state]);
  const dead = state === "ended" || state === "error";
  const restart = useCallback(() => restartTerminal(id), [id]);
  const reconnect = useCallback(() => reconnectTerminal(id), [id]);
  const startFresh = useCallback(() => startTerminal(id, "fresh"), [id]);
  const startResume = useCallback(() => startTerminal(id, "resume"), [id]);
  // A resume that cannot work must not be a button that looks like it can, so
  // this needs the pane to still declare a resume command *and* the daemon to
  // hold a token for it. Where the token knowledge comes from differs by state:
  //
  //  - `launched` is the strongest: this session reached `ready` under a spec
  //    in *this* window, so the daemon recorded a token. It has to be here
  //    because `resumable` is fetched once per worktree selection and never
  //    refreshed — a pane that launched afterwards is simply absent from it,
  //    and without `launched` such a pane hitting `error` offered only "Start
  //    fresh", minting a new token and abandoning the conversation.
  //  - `ended` means a holder ran the command and it exited, so a token exists.
  //  - `resumable` is the fetched set, and the only evidence available for a
  //    pane restored from storage that has not connected in this window.
  const hasToken = launched || state === "ended" || resumable;
  const canResume = Boolean(specId && hasToken && spec?.can_resume);
  const label = spec?.label ?? specId ?? "";
  // `error` means the pipe broke, not that the shell did — the session very
  // likely survives on the daemon (that is what the detach grace is for), so
  // offer to reattach before offering to replace it.
  const canReconnect = state === "error";
  // A plain terminal (no spec) has no configured label; the full-size card that
  // a dead terminal now gets needs a name to sit under the icon.
  const cardTitle = label || "Terminal";

  return (
    <div className="term-pane">
      <div className="term-slot" ref={slot} />
      {state === "connecting" && <div className="term-status">connecting…</div>}
      {/* A note about a terminal that is still running (dropped output, say).
          It goes here rather than into the terminal, which would corrupt a
          full-screen program's display — see writeNotice in terminalHost. */}
      {state === "live" && detail && <div className="term-status">{detail}</div>}
      {/* A terminal that is not running gets the pane, not a chip in the
          corner. That is true for a config pane that has not started (the only
          thing the pane is for at that moment) and — since a dropped socket
          with the shell still alive is exactly what the Reconnect button is
          for — for any terminal that has ended, plain or configured: after a
          full-screen program exits or the pipe breaks there is usually nothing
          else on screen. Previously a plain terminal reported the loss as a
          chip in the corner, which read as minor for the case the whole
          reconnect feature exists for.
          
          **Dismissible, and that is not decoration.** `close_on_exit` closes a
          pane that ended cleanly, so a pane still sitting here has almost
          always *failed* — and whatever is on the screen underneath is the
          reason. Covering that permanently would repeat the mistake the
          non-zero-exit guard exists to prevent. */}
      {((specId && state === "idle") || dead) && !showOutput && (
        <div className="term-overlay">
          <div className="term-card">
            <div className="term-card-icon">{paneIcon(spec?.icon, 26)}</div>
            <div className="term-card-title">{cardTitle}</div>
            <div className="term-card-detail">
              {state === "idle"
                ? canResume
                  ? "is not running"
                  : "has not started"
                : detail || "session ended"}
            </div>
            <div className="term-card-actions">
              {canReconnect && (
                <button
                  className="btn big"
                  onClick={reconnect}
                  title="Reattach to the same shell"
                >
                  Reconnect
                </button>
              )}
              {canResume && (
                <button
                  className="btn big"
                  onClick={startResume}
                  title="Pick up where this pane left off"
                >
                  Resume {label}
                </button>
              )}
              <button
                className="btn big"
                onClick={label ? startFresh : restart}
                title={
                  label
                    ? canResume
                      ? "Start over, discarding the previous session"
                      : undefined
                    : "End this shell and start a new one"
                }
              >
                {canResume ? "Start fresh" : label ? `Start ${label}` : "Restart"}
              </button>
            </div>
            {dead && (
              <button className="term-card-link" onClick={() => setShowOutput(true)}>
                Show output
              </button>
            )}
          </div>
        </div>
      )}
      {/* The full-card "Show output" view: the user asked to see what was under
          the overlay, so the terminal scrollback is back on screen and the
          dead-state actions become a corner chip rather than covering it. */}
      {dead && showOutput && (
        <div className="term-status">
          <span>{detail || "session ended"}</span>
          {canReconnect && (
            <button className="btn" onClick={reconnect} title="Reattach to the same shell">
              Reconnect
            </button>
          )}
          {canResume ? (
            <>
              <button
                className="btn"
                onClick={startResume}
                title="Pick up where this pane left off"
              >
                Resume {label}
              </button>
              <button
                className="btn"
                onClick={startFresh}
                title="Start over, discarding the previous session"
              >
                Start fresh
              </button>
            </>
          ) : (
            <button
              className="btn"
              onClick={restart}
              title="End this shell and start a new one"
            >
              Restart
            </button>
          )}
        </div>
      )}
    </div>
  );
}
