import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  runRef,
  type EmojiHolder,
  type EnvironmentList,
  type ExtensionSpec,
  type Lane,
  type Preset,
  type Repo,
  type RepoGitStatus,
  type RepoList,
  type RunInfo,
  type RunRef,
  type SharesList,
  type SettingsDoc,
  type StatsResponse,
  type Worktree,
  type WorktreeGitStatus,
} from "./api";
import {
  gitCreateFrom,
  hideDisabledActions,
  logsTimeZone,
  stalenessHue,
  markerFace,
  markerStyle,
  quickSwitchPrefs,
  runHistoryDays,
  searchUrl,
  terminalPrefs,
  activityPrefs,
  focusPrefs,
  showProjectColumn as showProjectColumnPref,
  focusSuppresses,
  FOCUS_SUPPRESS_BELL,
  FOCUS_SUPPRESS_TOASTS,
  FOCUS_SUPPRESS_OS_NOTIFICATIONS,
} from "./shared/settings";
import { pruneRunHistory } from "./shared/runHistory";
import {
  applyTerminalPrefs,
  setBellSuppressed,
  setPaneCloseHandler,
} from "./panes/terminalHost";
import { StartScreen } from "./promotions/StartScreen";
import { usePromotions } from "./promotions/usePromotions";
import { WhatsNewDialog } from "./promotions/WhatsNew";
import { SettingsDialog } from "./components/SettingsDialog";
import { ShortcutsDialog } from "./shortcuts/ShortcutsDialog";
import { nextIndex } from "./shortcuts/registry";
import { InboxIcon, inboxDescription } from "./inbox/InboxIcon";
import { inbox, notifyKey } from "./inbox/inbox";
import { useInbox } from "./inbox/useInbox";
import { useSettings } from "./shared/useSettings";
import { TopBarControls } from "./components/TopBarControls";
import {
  activeRun,
  bestFuzzyMatch,
  freshRunName,
  fuzzyMatch,
  laneDropTarget,
  liveRuns,
  moveLane,
  moveWorktree,
  needsAttention,
  parsePendingKey,
  pendingKey,
  pickRun,
  prunePending,
  railGroups,
  runSignatureFor,
  runStatus,
  runsForWorktree,
  selectorRuns,
  siblingRuns,
  sortRunsForDisplay,
  sortedUrls,
  spinnerAction,
  startRunName,
  worktreeStatus,
  worstStatus,
  DELETING_LANE,
  DETACHED_LANE,
  isDetached,
  TRASH_LANE,
  type PendingAction,
  type RailGroup,
  type PendingMap,
  type WorktreeStatus,
} from "./model";
import { startOriginLabel } from "./shared/startOrigin";
import { worktreeLabel } from "./shared/worktreeName";
import { nodeRows, type NodeRow } from "./shared/NodeList";
import { NodeActions } from "./shared/NodeActions";
import {
  ActionIcon,
  Badge,
  Button,
  Loader,
  MantineProvider,
  Menu,
  Tooltip,
  TextInput,
} from "@mantine/core";
import {
  IconAlertTriangleFilled,
  IconArrowBackUp,
  IconArrowsExchange,
  IconCheck,
  IconChevronDown,
  IconChevronLeft,
  IconChevronRight,
  IconChevronUp,
  IconDots,
  IconDotsVertical,
  IconFolderPlus,
  IconHistory,
  IconKeyboard,
  IconMoon,
  IconPlayerPlayFilled,
  IconPlayerStopFilled,
  IconPlus,
  IconAdjustments,
  IconReload,
  IconRefreshDot,
  IconSettings,
  IconSparkles,
  IconStack2,
  IconBuildingBroadcastTower,
  IconShare,
  IconTrash,
  IconSun,
  IconDeviceDesktop,
  IconExternalLink,
  IconWorld,
  IconTools,
  IconX,
  IconHelp,
} from "@tabler/icons-react";
import { Notifications } from "@mantine/notifications";
import { ContextMenuProvider, useContextMenu } from "mantine-contextmenu";
import { theme as mantineTheme } from "./theme";
import { RunsMode } from "./runs/RunsMode";
import { PaneArea, tabElementId } from "./panes/PaneArea";
import type { RunPaneContext } from "./panes/RunPanes";
import { notifyDone, notifyError, notifyRedirect, notifyTerminal, showSystemNotification } from "./shared/notify";
import {
  JoinRequestRow,
  RunSharePanel,
  runOfShare,
  sharesForRun,
} from "./shared/Sharing";
import {
  type BrowserProfile,
  DEFAULT_RATIO,
  type PaneLayout,
  type PaneLayoutUpdate,
  SESSIONS_STORAGE_KEY,
  activeTab,
  activateTab,
  addTab,
  addTabToFocused,
  adoptTabs,
  allTabs,
  findTab,
  browserIds,
  browserTab,
  closeTab,
  configPaneTab,
  defaultLayout,
  diagTab,
  dockOf,
  lastBlankBrowserId,
  loadLayouts,
  newTabId,
  nextFreeProfile,
  paneTabBaseLabel,
  paneTabLabel,
  parseSessionSets,
  parseTransferTabs,
  revealDiagPane,
  saveLayouts,
  serializeSessionSets,
  sessionSetFor,
  normalizeBrowserUrl,
  terminalIds,
  updateTab,
  urlLabel,
} from "./panes/model";
import { acquireWorktree } from "./ide/acquire";
import { channel, type ClaimResult, type ClientInfo } from "./ide/channel";
import { awayNote, openableWorktrees, worktreeSetKey } from "./ide/ownership";
import {
  MAX_PROJECT_SHORTCUTS,
  isProjectNews,
  otherProjectWorktreeIds,
  dropTargetIndex,
  projectForShortcut,
  projectHolder,
  projectShortcutDigit,
  projectInitials,
  projectWorktreeIds,
  reorderedRoots,
  toggleTarget,
} from "./shared/projects";
import {
  adoptLegacyLayouts,
  cancelPendingWrite,
  dropLayout,
  flushPendingOnUnload,
  onExternalLayoutChange,
  readLayout,
  refreshLayout,
  syncLayouts,
} from "./ide/layoutStore";
import {
  applyTerminalTheme,
  onTerminalOpenUrl,
  onTerminalTitleChange,
  openExternally,
  pruneTerminals,
  noteExpectedResumes,
  releaseTerminal,
  restartTerminal,
} from "./panes/terminalHost";
import {
  onBrowserAccelerator,
  onBrowserOpenRequest,
  popBrowserSuspend,
  pruneBrowsers,
  pushBrowserSuspend,
  reloadBrowser,
  setBrowserPolicy,
} from "./panes/browserHost";
import { watchOverlays } from "./panes/overlayGuard";
import {
  StartConfig,
  defaultStartSelection,
  parseStartSelection,
  pruneStartSelection,
  resolveStartSelection,
  resolveStoredSelection,
  startBody,
  startSelectionLabel,
  startStorageKey,
} from "./components/StartConfig";
import { ConfigVarsDialog } from "./components/ConfigVars";
import { TopBarExtensions, useExtensionStatus } from "./components/Extensions";
import {
  ChangeMarkerDialog,
  ConfirmDeleteWorktreeDialog,
  ImportRepoDialog,
  LaneNameDialog,
  Modal,
  NewWorktreeDialog,
  RemoveRepoDialog,
  RenameWorktreeDialog,
  TrashWorktreeDialog,
  UpdateMainDirtyDialog,
} from "./components/dialogs";

import {
  chromeless,
  desktopApp,
  desktopWindow,
  getOriginWindowId,
  isElectron,
  layoutSlot,
  openSettingsOnBoot,
  topbarClass,
  windowRestored,
  windowSeed,
} from "./shell";

const POLL_MS = 5000;

/**
 * How this client is described to the others when it is holding a worktree they
 * want.
 *
 * Only ever *rendered* for a browser holder — a desktop window is raised, so
 * there is nothing to tell the user about where it is — and a browser tab is
 * named by its browser, because that is the whole of what can honestly be said
 * about a client nothing can raise. The Electron string exists so the daemon's
 * log and any future surface have something better than an empty field; it is
 * deliberately not `document.title`, which the shell never sets (it sets the
 * *native* title) and which is therefore the same word in every window.
 */
function clientLabel(): string {
  // An Electron window that cannot raise itself is described as the place it is
  // rather than as a window that will come forward — see `clientKind`.
  if (canRaiseSelf()) return "Veld Desktop";
  if (isElectron) return "another Veld Desktop window";
  const ua = navigator.userAgent;
  for (const [name, probe] of [
    ["Firefox", /Firefox\//],
    ["Edge", /Edg\//],
    ["Chrome", /Chrome\//],
    ["Safari", /Safari\//],
  ] as const) {
    if (probe.test(ua)) return name;
  }
  return "a browser tab";
}

/**
 * Whether this client can bring itself to the front when the daemon asks.
 *
 * **A capability, not a platform.** `isElectron` is a URL parameter and is true
 * on any shell, including one older than `focusSelf` — so reporting the kind
 * from it told the daemon a window could be raised when nothing was there to
 * raise it, and the client that was refused promised "switched to it" for a
 * switch that visibly did not happen. Asking what this page can actually do
 * cannot drift from what it does.
 */
function canRaiseSelf(): boolean {
  return typeof desktopWindow?.focusSelf === "function";
}

/** How the daemon should describe this client to the others. */
function clientKind(): "electron" | "browser" {
  return canRaiseSelf() ? "electron" : "browser";
}

/**
 * What to tell someone whose click was refused.
 *
 * The distinction is the point, and it is why the daemon sends the holder's
 * kind rather than just refusing. A desktop window has been raised and is now in
 * front of them, so the notice explains where they went. A **browser tab cannot
 * be raised** — `window.focus()` outside a user gesture is ignored by every
 * browser — so promising "switched to it" there would be a lie about something
 * they can see did not happen. It says where the worktree is instead, and the
 * tab marks itself so it is findable.
 */
function holderNotice(w: Worktree, holder?: ClientInfo): string {
  const label = worktreeLabel(w);
  if (holder?.kind === "browser") {
    const where = holder.label || "a browser tab";
    return `${label} is open in ${where} — switch to it there`;
  }
  return `${label} is open in another window — switched to it`;
}

/**
 * Mark this page as wanted, for a client that cannot raise itself.
 *
 * The title, because it is the one thing a background tab still shows. Cleared
 * on the next interaction rather than a timer: the marker's job is to survive
 * until the person looks, and a timeout that fires while they are in another
 * app is a marker that was never seen.
 */
function flashAttention(): void {
  const original = document.title;
  if (original.startsWith("\u25CF ")) return;
  document.title = `\u25CF ${original}`;
  const clear = () => {
    document.title = original;
    window.removeEventListener("focus", clear);
    window.removeEventListener("pointerdown", clear);
  };
  window.addEventListener("focus", clear);
  window.addEventListener("pointerdown", clear);
}

/** How long an optimistic pending marker survives without an observed run
 *  signature change. Several polls' worth, so a slow `veld start` isn't cut
 *  off. */
const PENDING_TTL_MS = 60_000;

/**
 * How long the control socket may be down before the page says so.
 *
 * Long enough to cover a reconnect (the channel's own backoff starts at 300ms)
 * and a daemon restart, so `veld update` does not flash a warning at everyone;
 * short enough that a page whose upgrade is being *refused* — a scheme the
 * daemon's origin allowlist does not know, a proxy that drops upgrades — says
 * something while the person is still looking at it. That case is why this
 * exists: without arbitration the dashboard loads, polls and renders a rail, and
 * then quietly refuses to open a worktree or attach a terminal.
 */
const CHANNEL_DOWN_NOTICE_MS = 4000;

/**
 * Whether a keystroke landed in something the user is typing into.
 *
 * For the one shortcut that is a plain letter. `contentEditable` as well as the two
 * input elements, because the rename fields and the ⌘K box are not the only places a
 * caret can be.
 */
function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  // xterm's own hidden input (`xterm-helper-textarea`), not a real text field
  // someone is composing in — a terminal's content is a PTY relay, not prose.
  // Every chord guarded by this function used to be one `panes/terminalKeys.ts`
  // let xterm consume first, so `target` here was never actually this element
  // in practice — xterm's own `preventDefault`/`stopPropagation` meant the
  // keydown never reached this listener at all. `isAppShortcutChord` changed
  // that: it explicitly bypasses xterm for the new shortcuts below, which is
  // exactly what makes this exemption load-bearing now rather than dead code
  // — without it, every one of those chords would silently do nothing from a
  // focused terminal, the opposite of what the bypass exists for.
  if (target.classList.contains("xterm-helper-textarea")) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable;
}

/**
 * The three theme values, named.
 *
 * A list rather than a cycle, because the bar's overflow menu shows all three at
 * once — "system" especially is a value you have to be able to *ask* for, and a
 * three-step cycle only ever let you arrive at it. `auto` is the stored value's
 * name and stays that way; "System" is what it is called on screen.
 */
const THEME_CHOICES = [
  { value: "auto", label: "System", icon: IconDeviceDesktop },
  { value: "light", label: "Light", icon: IconSun },
  { value: "dark", label: "Dark", icon: IconMoon },
] as const;

/**
 * A worktree's marker, in whichever face the `worktree.markerStyle` setting asks
 * for.
 *
 * One component for every DOM render site, so the rail and the command palette
 * cannot drift into showing different faces — which they did before this existed,
 * because each had its own `{w.emoji && …}`.
 *
 * `settings` may be `null` (nothing loaded yet). `markerFace` handles that by
 * falling back to the glyph, which is also the right answer during the upgrade
 * window: a worktree migrated from before the colour column has no hue until the
 * next sync, and colour is the default style, so a naive renderer would show
 * nothing at all for every existing worktree.
 *
 * Deliberately not used for the OS window title or the native tray menu — those
 * are plain strings handed to the OS, where a CSS custom property means nothing.
 * They take the glyph unconditionally; the rule is stated in `desktop/src/main.js`,
 * where the label is built.
 */
function WorktreeMark(props: {
  settings: SettingsDoc | null;
  worktree: { emoji: string; marker_color: string };
}) {
  const face = markerFace(props.settings ?? {}, props.worktree);
  if (!face) return null;
  if (face.kind === "emoji") {
    return <span className="wt-emoji">{face.emoji}</span>;
  }
  return (
    <span
      className="wt-dot"
      // The stored value is the colour itself, so there is no palette to look it up
      // in and nothing that repaints an existing worktree when the offered set is
      // retuned. Shape-checked by `hasMarkerColor` before reaching here.
      style={{ background: face.color }}
      // Decorative: the alias is rendered right beside it, so announcing a colour
      // would add nothing a screen reader user can act on. That the label is always
      // present is also the colour-vision answer — the swatch is a scanning aid over
      // text, never the identifier.
      aria-hidden
    />
  );
}

function usePersisted(key: string, initial: string): [string, (v: string) => void] {
  const [value, setValue] = useState(
    () => window.localStorage.getItem(key) ?? initial,
  );
  // useState's initializer runs once per component, not per key — when the
  // key changes (e.g. the per-worktree preset), re-read the stored value or
  // the previous key's value silently carries over and overwrites.
  useEffect(() => {
    setValue(window.localStorage.getItem(key) ?? initial);
    // `initial` is intentionally not a dependency — only a key switch re-reads.
  }, [key]); // eslint-disable-line react-hooks/exhaustive-deps
  const set = useCallback(
    (v: string) => {
      setValue(v);
      window.localStorage.setItem(key, v);
    },
    [key],
  );
  return [value, set];
}

/**
 * A window's own remembered selection, plus the app-wide one.
 *
 * Two keys, written together and read in that order. The slot-scoped one is what
 * makes several windows work: layouts are already per slot, so without a
 * matching selection every window reopened after a restart read the *same*
 * last-written worktree and showed a layout its panes were not for — a window
 * would come back on the wrong repository with its own terminals sitting
 * unattached. The unscoped one stays as the fallback, and it is the reason `⌘N`
 * opens on what you were just looking at rather than on the first repo in the
 * list: a brand-new slot has nothing of its own yet.
 *
 * A plain browser tab has no slot and therefore only the unscoped key, which is
 * the behaviour it had before windows existed.
 */
/** Removal failures this window has already announced. See the effect that uses it. */
const TRASH_ACK_KEY = "veld.trashAcked";

function selectionKeys(name: string): [string, string] {
  return layoutSlot ? [`${name}.slot.${layoutSlot}`, name] : [name, name];
}

function usePersistedPerWindow(name: string): [string, (v: string) => void] {
  const [scoped, global] = selectionKeys(name);
  const [value, setValue] = useState(
    () => window.localStorage.getItem(scoped) ?? window.localStorage.getItem(global) ?? "",
  );
  const set = useCallback(
    (v: string) => {
      setValue(v);
      try {
        window.localStorage.setItem(scoped, v);
        window.localStorage.setItem(global, v);
      } catch {
        // Storage unavailable: the selection lives in the URL for this session
        // anyway, and losing it costs a default worktree on the next launch.
      }
    },
    [scoped, global],
  );
  return [value, set];
}

/**
 * Which of a worktree's runs this window is bound to.
 *
 * Per worktree **and** per window slot, for the same reason the worktree
 * selection is: layouts are per slot, so two windows open on one directory must
 * be able to watch different runs — otherwise picking a run in one window moves
 * the other window's logs pane out from under its reader. The unscoped key is the
 * seed for a brand-new slot, exactly as `usePersistedPerWindow` uses it.
 *
 * Deliberately **not** in the URL. A run in the query string would make it a
 * navigation coordinate, and then every worktree-keyed surface — pane layouts,
 * browser sessions, terminal PTY sessions — would have to declare whether it sits
 * inside run scope; for terminals and browsers the answer is no (a shell is a
 * shell in a directory). `""` means "no explicit choice", which resolves through
 * `pickRun`, not to an error.
 *
 * **The stored key travels *with* the value, and no effect corrects it.** The
 * obvious shape — `useState(read)` plus an effect that re-reads when the key
 * changes — commits one frame in which the new worktree is bound to the *previous*
 * worktree's run name, so `pickRun` reports `missing` and the control whose entire
 * job is never to misname a run renders "no environment named api here" before the
 * effect fixes it. Staleness has to be impossible in the rendered value, not
 * repaired after the fact.
 */
function useSelectedRun(
  worktreePath: string | undefined,
): [string, (v: string) => void] {
  const name = `veld.run.${worktreePath ?? "_"}`;
  const [scoped, global] = selectionKeys(name);
  const read = useCallback(
    () =>
      window.localStorage.getItem(scoped) ??
      window.localStorage.getItem(global) ??
      "",
    [scoped, global],
  );
  // `key` is what makes the pair self-invalidating: a state entry written for
  // another worktree simply does not apply, and the fallback read happens during
  // this render rather than after it.
  const [choice, setChoice] = useState<{ key: string; value: string }>(() => ({
    key: scoped,
    value: read(),
  }));
  const value = choice.key === scoped ? choice.value : read();
  const set = useCallback(
    (v: string) => {
      setChoice({ key: scoped, value: v });
      try {
        window.localStorage.setItem(scoped, v);
        window.localStorage.setItem(global, v);
      } catch {
        // Storage unavailable: the choice holds for this session and is
        // re-resolved on the next launch. Nothing else depends on it.
      }
    },
    [scoped, global],
  );
  return [value, set];
}

/**
 * Selection state lives in the URL (`?repo=…&wt=…`) so views are addressable:
 * a multi-window Electron layout opens one URL per worktree, browser tabs
 * deep-link, and reload restores the exact view. localStorage is the fallback
 * when the URL carries no selection — per window slot first, see above.
 */
function useUrlSelection(): {
  repo: string;
  wt: string;
  setRepo: (root: string) => void;
  setWt: (key: string) => void;
} {
  const params = new URLSearchParams(window.location.search);
  const [repo, setRepoState] = usePersistedPerWindow("veld.repo");
  const [wt, setWtState] = usePersistedPerWindow("veld.worktree");
  const [urlRepo, setUrlRepo] = useState(params.get("repo") ?? "");
  const [urlWt, setUrlWt] = useState(params.get("wt") ?? "");

  const effectiveRepo = urlRepo || repo;
  const effectiveWt = urlWt || wt;

  return {
    repo: effectiveRepo,
    wt: effectiveWt,
    setRepo: (root) => {
      setUrlRepo(root);
      setRepoState(root);
    },
    setWt: (key) => {
      setUrlWt(key);
      setWtState(key);
    },
  };
}

/**
 * The Runs|IDE switcher, first control in both top bars.
 *
 * It used to hide behind the wordmark and appear on hover. That made the one
 * control that moves between the app's two modes invisible until the pointer
 * happened to cross it — a logo where a button belongs. The wordmark is gone
 * from this bar entirely rather than moved: the bar is dense, and the brand is
 * carried by the favicon, the window/app icon and the daemon's own pages.
 *
 * The swap glyph is shown statically — the discoverability win of showing it
 * on hover is worth more when it is always visible, and there is no brand mark
 * left to trade against it. The tooltip names both halves the glyph cannot:
 * which mode you are in, and which one a click reaches. The word labels are
 * gone: in a bar this dense they read as the loudest text in it, and
 * "IDE"/"Runs" as words carry no information the tooltip and the destination
 * glyph do not. A screen reader still gets the full state + destination from
 * the `aria-label`.
 */
function ModeSwitch(props: {
  mode: string;
  onMode: (m: string) => void;
}) {
  const other = props.mode === "ide" ? "runs" : "ide";
  const modeLabel = (m: string) => (m === "runs" ? "Runs" : "IDE");
  return (
    <Tooltip label={`Switch to ${modeLabel(other)}`}>
      <Button
        size="compact-sm"
        variant="default"
        className="mode-switch"
        aria-label={`${modeLabel(props.mode)}, switch to ${modeLabel(other)}`}
        onClick={() => props.onMode(other)}
      >
        <IconArrowsExchange size={14} />
      </Button>
    </Tooltip>
  );
}

export function App() {
  // Three-state preference: auto (follow OS) → light → dark. Stored values
  // from the two-state era ("dark"/"light") remain valid.
  const [themePref, setThemePref] = usePersisted("veld.theme", "auto");
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (e: MediaQueryListEvent) => setSystemDark(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  const theme =
    themePref === "auto" ? (systemDark ? "dark" : "light") : themePref;
  useEffect(() => {
    document.body.dataset.theme = theme;
    // Pushed from *this* effect, immediately after the attribute above, and
    // not from a child: xterm can't read CSS variables, so it resolves the
    // theme tokens off `document.body` — and React runs child effects before
    // the parent's, so doing this in AppInner read the outgoing palette and
    // left open terminals on the old colours.
    applyTerminalTheme(theme === "light" ? "light" : "dark");
  }, [theme]);
  const cycleTheme = () =>
    setThemePref(
      themePref === "auto" ? "light" : themePref === "light" ? "dark" : "auto",
    );

  // An embedded browser pane is a native view in the desktop shell, so it paints
  // over every menu and dropdown. This hides the panes while one is open; see
  // panes/overlayGuard.ts. Mounted here rather than in IDE mode because it
  // watches the document, and no-op in a plain browser.
  useEffect(watchOverlays, []);

  // Providers live above AppInner so useContextMenu / Mantine hooks work
  // anywhere below; the color scheme follows our own persisted toggle.
  return (
    <MantineProvider
      theme={mantineTheme}
      forceColorScheme={theme === "light" ? "light" : "dark"}
    >
      <ContextMenuProvider borderRadius="md">
        {/* Toasts are the app's one error surface (see shared/notify.ts). Top-right
            rather than the default bottom-right: that is the corner the run
            controls and the Sharing surface sit in, so a failure appears next to
            what was clicked. */}
        <Notifications position="top-right" limit={4} />
        <AppInner
          theme={theme}
          themePref={themePref}
          onCycleTheme={cycleTheme}
          onSetTheme={setThemePref}
        />
      </ContextMenuProvider>
    </MantineProvider>
  );
}

function AppInner(props: {
  theme: string;
  themePref: string;
  onCycleTheme: () => void;
  /** Set the theme outright. The overflow menu names all three values, where the
   *  cycle only ever stepped through them. */
  onSetTheme: (value: string) => void;
}) {
  const { theme, themePref, onCycleTheme, onSetTheme } = props;

  // Daemon-owned preferences, shared with every other window and with a plain
  // browser tab against the same daemon. `settings` is null until the first read
  // of either the local mirror or the daemon resolves.
  const {
    settings,
    save: saveSettings,
    saving: savingSettings,
    error: settingsError,
  } = useSettings();

  // The Electron menu's ⌘, and the "opened for settings" boot flag.
  //
  // Both exist because a menu accelerator is the only binding that survives a
  // focused `WebContentsView` swallowing every keystroke; the page's own handler
  // covers a browser tab, where there is no menu.
  useEffect(() => {
    if (openSettingsOnBoot) setDialog({ kind: "settings" });
    return desktopApp?.onOpenSettings(() => setDialog({ kind: "settings" }));
  }, []);

  // Publish terminal preferences to the xterm layer. xterm cannot inherit CSS
  // variables or read the settings document, so this is the only path by which a
  // font or cursor change reaches a live shell — and it re-fits every session,
  // including hidden ones, which keep running and would otherwise render at the
  // old metrics until you next looked at them.
  useEffect(() => {
    if (!settings) return;
    applyTerminalPrefs(terminalPrefs(settings));
    setBellSuppressed(focusSuppresses(focusPrefs(settings), FOCUS_SUPPRESS_BELL));
  }, [settings]);

  // How much run history the pickers offer. Read here and applied to the polled
  // payload once (see `pruneRunHistory`), so every surface that renders history
  // agrees without each one filtering for itself.
  const historyDays = runHistoryDays(settings ?? {});

  // Whether an inapplicable top-bar action is hidden rather than shown greyed.
  // Read here and threaded to the bar — see shared/settings.ts.
  const hideDisabled = hideDisabledActions(settings ?? {});

  // Which quick switches a browser pane's chrome shows. Read here and threaded,
  // rather than each pane calling `useSettings` — that would be a fetch and a
  // focus listener per pane for one document the app already holds.
  const quickSwitches = quickSwitchPrefs(settings ?? {});

  // Where a pane's address bar sends words that are not an address, `""` for
  // nowhere. Threaded for the same reason as the switches above — one document the
  // app already holds — and read by both a pane's chrome and the new-pane chooser,
  // which only needs to know whether searching is possible at all.
  const searchTemplate = searchUrl(settings ?? {});

  // Which zone the logs views spell a line's timestamp in. Read here and threaded for
  // the same reason as the two above, and it is the same key `veld logs` reads — so the
  // two agree on the *policy*.
  //
  // They do not necessarily agree on the zone, and it would be wrong to claim they do:
  // "local" is resolved twice and independently — `chrono::Local` from the CLI
  // process's environment, this side from the browser. An empty `TZ` makes chrono
  // answer UTC while the browser stays on the machine's zone, and a browser on another
  // host is simply somewhere else. "Local" means *each reader's own clock*, which is
  // the intent — not a promise that two readers share one.
  //
  // A Veld terminal pane is NOT one of those cases: the holder spawns `$SHELL -l` on a
  // tty, so it reads the same startup files a real terminal does. That is the same
  // reason it is the documented exception to AGENTS.md's daemon-`PATH` rule.
  const logsTz = logsTimeZone(settings ?? {});

  // Which of this worktree's config-declared panes the daemon holds a session
  // token for, so a restored pane can offer "Resume" rather than only "Start".
  //
  // **The answer carries the question.** A config pane decides whether to
  // auto-resume at the moment it first mounts, and that mount happens in the
  // same commit that starts this fetch — child effects run before parent ones,
  // so the data is never there yet. A bare `Set` made every restored pane read
  // "nothing to resume", which killed `auto_resume` outright; a *nullable* `Set`
  // fixed the first render but not a worktree switch, where the previous
  // worktree's set is still non-null and reads as an answer. No effect can fix
  // that, because an effect cannot un-render the commit that already mounted
  // the child.
  //
  // So the worktree id travels *with* the set, and `paneAnswerFor` compares it
  // to the worktree being rendered. Nothing clears this on a switch — a stale
  // answer is indistinguishable from no answer by *value*, at render time, with
  // nothing to sequence. Do not "tidy" that back into an effect that resets it:
  // that was the version this replaced, and it cannot work.
  //
  // Fetched once per worktree rather than polled: it only decides what a pane
  // does as it comes into being, and a pane that launches while the window is
  // open records that itself (see `launched` in `terminalHost`).
  const [paneSessions, setPaneSessions] = useState<{
    worktreeId: number;
    resumable: Set<string>;
  } | null>(null);

  // View mode: worktree cockpit ("ide") vs runs management ("runs").
  // Defaults by serving path (the app will also own `/` at v1 parity);
  // `?view=` overrides and persists across navigation.
  const [mode, setModeState] = useState<string>(() => {
    const q = new URLSearchParams(window.location.search).get("view");
    if (q === "runs" || q === "ide") return q;
    return window.location.pathname.endsWith("/ide") ? "ide" : "runs";
  });
  const setMode = (m: string) => {
    setModeState(m);
    const p = new URLSearchParams(window.location.search);
    p.set("view", m);
    window.history.replaceState(null, "", `?${p.toString()}`);
  };

  // ---- polled server state ------------------------------------------------
  const [repoList, setRepoList] = useState<RepoList | null>(null);
  const [envs, setEnvs] = useState<EnvironmentList | null>(null);
  const [shares, setShares] = useState<SharesList | null>(null);
  /** Whether the last `/api/shares` read failed — see `refresh`. */
  const [sharesStale, setSharesStale] = useState(false);
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [offline, setOffline] = useState(false);
  // Runs mode does its own polling on its own cadence, so the diagnostics reads
  // below are fetched only while IDE mode is the one on screen.
  const wantRunState = mode === "ide";

  // Known, accepted: refreshes are not sequenced. A poll issued before a
  // mutation but resolving after it writes the pre-mutation payload back, so
  // a rename or emoji change can visibly revert for up to one poll before
  // settling. Fixing it needs a monotonic request counter guarding the
  // setters; not worth it while every mutation is followed by its own
  // `refresh()`.
  // `historyDays` is a dependency: a change to the horizon must reach the next poll,
  // and re-creating `refresh` is what restarts the interval effect below with it.
  const refresh = useCallback(async () => {
    // Shares and stats ride the same tick, but they are `allSettled` and must not
    // decide the offline banner: the view is built on repos + environments, while
    // a hiccup on either of these two keeps the last known values (the same rule
    // runs mode follows for stats). Issued before the await below so all four
    // requests are in flight together.
    const extras = wantRunState
      ? Promise.allSettled([api.shares(), api.stats()])
      : null;
    try {
      // refreshRepos (not the plain GET): reconciles worktree rows with git
      // so out-of-app `git worktree add/remove` appears on the next poll.
      const [repos, environments] = await Promise.all([
        api.refreshRepos(),
        api.environments(),
      ]);
      setRepoList(repos);
      setEnvs(pruneRunHistory(environments, historyDays, new Date()));
      setOffline(false);
    } catch {
      setOffline(true);
    }
    if (!extras) return;
    const [sharesResult, statsResult] = await extras;
    if (sharesResult.status === "fulfilled") setShares(sharesResult.value);
    // Tracked, unlike stats: shares drive a *control* surface, and a panel that
    // says "nothing shared" from a read that never landed invites the user to
    // start a second share of a run that already has one.
    setSharesStale(sharesResult.status !== "fulfilled");
    if (statsResult.status === "fulfilled") setStats(statsResult.value);
  }, [wantRunState, historyDays]);

  // Tick counter, bumped per poll. Effects that must re-evaluate on a
  // schedule (not only when the payload changes) depend on it — a failed
  // refresh leaves `envs`/`repoList` at the same identity.
  const [poll, setPoll] = useState(0);
  useEffect(() => {
    void refresh();
    const t = window.setInterval(() => {
      setPoll((n) => n + 1);
      void refresh();
    }, POLL_MS);
    return () => window.clearInterval(t);
  }, [refresh]);

  // ---- selection ----------------------------------------------------------
  const {
    repo: activeRepoRoot,
    wt: activeWtKey,
    setRepo: setActiveRepoRoot,
    setWt: setActiveWtKey,
  } = useUrlSelection();

  const repos = useMemo(() => repoList?.repos ?? [], [repoList]);
  const repo: Repo | null =
    repos.find((r) => r.root === activeRepoRoot) ?? repos[0] ?? null;
  // Feature promotions, Veld's and the selected project's. Suppressed while the
  // first-run screen is up (or before the first fetch has said whether it will
  // be): a panel thrown over the screen that is trying to get somebody started is
  // the wrong moment, and `repoList === null` is the pre-data state, not "no
  // projects".
  //
  // The *selected* project only, not every imported one. The stored state row
  // grows monotonically and the daemon cannot prune an id it does not understand,
  // so "everything the user has a repo for" is a row that only gets bigger — and
  // the selected project is the one whose news the reader has any context for.
  const promotions = usePromotions({
    suppressAuto: repoList === null || repos.length === 0,
    project:
      repo && {
        root: repo.root,
        name: repo.name,
        created_at: repo.created_at,
        news: repo.news,
      },
    // `settings` itself, not `settings ?? {}`. Every other reader here can take the
    // fallback for one frame and reflow; this one cannot. `showProjectNews`
    // defaults to *true*, so a reader who switched project news off would — on a
    // first load in this browser profile, where there is no localStorage mirror —
    // get their project's cards auto-opened before their answer arrived, and *Got
    // it!* then writes read rows for cards they opted out of. Arriving settings
    // cannot re-close a latched dialog. `null` means "not known yet", and
    // `usePromotions` builds no project cards until it is.
    settings,
  });
  const worktrees = useMemo(() => repo?.worktrees ?? [], [repo]);
  // Mirrored, like `layoutsRef` below, so an effect can read the current list
  // without being *keyed* on it: this list is replaced on every 5s poll, and the
  // claim effect that reads it must not re-run on that cadence. Assigned during
  // render rather than in an effect, for the reason given there.
  const worktreesRef = useRef(worktrees);
  worktreesRef.current = worktrees;
  const lanes = useMemo(() => repo?.lanes ?? [], [repo]);
  // The fallbacks skip pending removals: when the worktree you were looking at is
  // being deleted, the app has to land somewhere that still exists rather than
  // opening panes on a vanishing directory.
  const selectable = useMemo(
    () => worktrees.filter((w) => !w.trashed_at),
    [worktrees],
  );
  const worktree: Worktree | null =
    selectable.find((w) => String(w.id) === activeWtKey) ??
    selectable.find((w) => w.is_main) ??
    selectable[0] ??
    null;

  // Refetched when the selection changes, and cleared first so a pane in the
  // new worktree can never be judged against the previous one's tokens — that
  // would offer a resume for somebody else's conversation.
  const worktreeId = worktree?.id ?? null;
  useEffect(() => {
    if (worktreeId === null) return;
    let live = true;
    api
      .paneSessions(worktreeId)
      .then((res) => {
        if (live) {
          setPaneSessions({
            worktreeId,
            resumable: new Set(res.resumable.map((r) => r.session_id)),
          });
        }
      })
      // A pane that cannot be shown as resumable still starts fresh, which is
      // the safe direction: the alternative is offering a resume we could not
      // confirm. It must still resolve to an *answer*, though — leaving it
      // absent would hold every config pane unmounted forever. Not worth a toast.
      .catch(() => {
        if (live) setPaneSessions({ worktreeId, resumable: new Set() });
      });
    return () => {
      live = false;
    };
  }, [worktreeId]);

  // A config pane that exits cleanly closes itself, and the host is what tells
  // us — not the pane's own component, which is only mounted while its tab is
  // the *active* one. A background tab's session keeps running and keeps
  // receiving frames, so deciding in the renderer deferred the close until the
  // user next clicked that tab, which reads as the click having closed it.
  //
  // Keyed by worktree because a session outlives the selection: the pane that
  // exited may belong to a worktree this window is no longer showing.
  useEffect(() => {
    setPaneCloseHandler((tabId, wtId) => {
      setLayouts((prev) => {
        const current = prev[wtId];
        if (!current) return prev;
        const next = closeTab(current, tabId);
        return next === current ? prev : { ...prev, [wtId]: next };
      });
    });
    return () => setPaneCloseHandler(null);
  }, []);

  /**
   * Worktrees another client is showing, and **which** client each one is in.
   *
   * Only for the rail's benefit — the shell is the authority on who may show
   * what, and `selectWorktree` still asks it. Rendering this is what stops the
   * ownership model reading as a bug: without it, a row simply refuses to open
   * and some other window jumps forward with no stated connection between the
   * two.
   *
   * A map rather than a set of ids, because "somewhere else" and "in Veld
   * Desktop" are different things to be told: the daemon already sends the
   * holder's kind and label with every row (it decides `focus` on the kind), and
   * throwing them away here left the rail with one greyed row and one tooltip
   * for a browser tab and a desktop window alike.
   */
  const [elsewhere, setElsewhere] = useState<Map<number, ClientInfo>>(new Map());

  /**
   * Whether to tell the user the control socket is not up.
   *
   * Reported on a delay rather than on the first drop — see
   * [`CHANNEL_DOWN_NOTICE_MS`] — so a reconnect is silent and a refusal is not.
   * The timer is the flag's only writer towards `true`, which is what keeps a
   * reconnect storm from re-arming it repeatedly.
   */
  const [channelDown, setChannelDown] = useState(false);
  const channelTimer = useRef<number | null>(null);
  const noteChannelDown = () => {
    // Already counting down: the retry that just failed is the same outage, and
    // restarting the timer on every attempt would push the notice past the
    // backoff for as long as the daemon keeps refusing.
    if (channelTimer.current !== null) return;
    channelTimer.current = window.setTimeout(() => {
      channelTimer.current = null;
      setChannelDown(true);
    }, CHANNEL_DOWN_NOTICE_MS);
  };
  const noteChannelUp = () => {
    if (channelTimer.current !== null) {
      clearTimeout(channelTimer.current);
      channelTimer.current = null;
    }
    setChannelDown(false);
  };

  /**
   * Open this client's control socket and keep it open for the life of the page.
   *
   * Deliberately not torn down on a dependency change: the socket **is** this
   * client's membership of the daemon's claim registry, so a lifecycle tied to
   * anything that re-renders would release every claim the page holds. It is
   * closed by the page going away, which is exactly the event a claim should not
   * survive.
   *
   * A detached window has no part in this: its tabs were transferred out of a
   * worktree its origin owns, so it is a satellite of that claim rather than a
   * claimant.
   */
  useEffect(() => {
    if (chromeless) return;
    // Before anything else touches a layout: whichever client still holds the
    // old browser store is the only one that can move it, and the first client
    // to open a worktree creates its row.
    void adoptLegacyLayouts();
    channel.start(clientKind(), clientLabel(), {
      // `mine` is the daemon's per-recipient answer, so the rail never needs to
      // know any client's identity to work out which rows are not its own.
      onClaims: (claims) => {
        setElsewhere(
          new Map(claims.filter((c) => !c.mine).map((c) => [c.worktree_id, c.client])),
        );
      },
      onYield: (worktreeId, ack) => onYield.current(worktreeId, ack),
      onFocus: () => {
        // An Electron window raises itself through its shell. A browser tab
        // **cannot** — `window.focus()` without a user gesture is ignored — so it
        // marks itself instead of pretending, and the client that asked was told
        // the holder is a browser and said so (see `holderNotice`).
        if (desktopWindow?.focusSelf) {
          void desktopWindow.focusSelf().catch(() => {});
          return;
        }
        flashAttention();
      },
      onLayoutChanged: (worktreeId) => {
        // Only for a worktree this client is showing. Anything else is a
        // notification about panes it does not have mounted, and re-reading it
        // would put a layout into state that nothing is attached to.
        if (layoutsRef.current[worktreeId]) void refreshLayout(worktreeId);
      },
      /**
       * **Ask for the worktree back on every connect.**
       *
       * A claim lives on the socket, so a socket that went away took this
       * client's claims with it — including across a daemon restart, where the
       * PTY holder processes keep the shells alive and two clients can each be
       * showing a worktree with nothing arbitrating between them. Re-claiming is
       * what resolves that, and it is why a reconnect is not a no-op.
       *
       * `sameEpoch` is deliberately *not* used to reset the layout store's
       * cached versions. Those describe rows in a database that outlives the
       * daemon, and clearing them made the next save present version 0, lose the
       * check against a row still at N, and adopt the pre-restart document —
       * silently reverting whatever the user had just done, and unmounting (and
       * hanging up) any terminal they had opened in the meantime.
       */
      onReady: (sameEpoch) => {
        noteChannelUp();
        // What to ask for. The worktree this client holds, first. Otherwise the
        // selection — but only when asking again cannot take a worktree off
        // somebody: either this client was never granted anything (the boot
        // claim ran before the socket was up), or the daemon **restarted**, in
        // which case its registry is empty and nobody holds anything. Without
        // the second case a window that had yielded sat empty for good: the boot
        // effect is keyed on a selection that has not changed, and nothing else
        // re-acquires.
        const mayAskAgain = !grantedRef.current || !sameEpoch;
        const wanted = shownRef.current ?? (mayAskAgain ? selectedRef.current : null);
        const target = worktreesRef.current.find((w) => w.id === wanted);
        // Gone from the list between the disconnect and now — a `git worktree
        // remove` in a terminal, say. Nothing to ask for.
        if (!target) return;
        void acquireRef.current(target);
      },
      onClosed: () => {
        // Nobody's claims are knowable now, and rendering the last table would
        // grey out rows whose holders may already be gone.
        setElsewhere(new Map());
        // "Every worktree is open somewhere else" is a claim about a table this
        // client can no longer read, so it must not stay on screen — with the
        // socket down it is indistinguishable from "the arbitration is gone",
        // which the banner below says truthfully.
        setClaimBlocked(false);
        // **Say so on screen.** A page whose channel cannot come up still loads,
        // still polls, and still renders a rail — it simply has no arbitration,
        // so worktrees do not open and terminals do not attach. The only report
        // was a `WebSocket connection failed` in the devtools console, whose
        // status a browser will not show; a dashboard served over plaintext
        // spent a whole session like that. See `channelDown`.
        noteChannelDown();
      },
    });
  }, [chromeless]);

  /**
   * Write anything still sitting in the layout store's debounce before the page
   * goes.
   *
   * `pagehide` rather than `beforeunload`: it fires for a bfcache eviction and on
   * mobile Safari, where `beforeunload` does not, and the whole window being
   * closed is precisely when the last thing the user did (moved a tab, dragged a
   * split) would otherwise be lost.
   */
  useEffect(() => {
    const flush = () => flushPendingOnUnload();
    window.addEventListener("pagehide", flush);
    return () => window.removeEventListener("pagehide", flush);
  }, []);

  // Adopt layouts this client did not write: a save that lost its version check
  // (the hand-off race) and the daemon's push for a worktree it is showing.
  useEffect(() => {
    onExternalLayoutChange((worktreeId, next) => {
      setLayouts((prev) => {
        if (!prev[worktreeId]) return prev;
        if (!next) {
          const without = { ...prev };
          delete without[worktreeId];
          return without;
        }
        noteExpectedResumes(terminalIds(next));
        return { ...prev, [worktreeId]: next };
      });
    });
  }, []);

  /**
   * Show a worktree — or go to the window that already is.
   *
   * A worktree has one set of panes and one window showing them, so this asks
   * the shell first. When another window has it, the shell focuses that window
   * and this one stays put: the rail row becomes "take me to where this
   * already is" rather than a way to grow a second set of terminals nobody can
   * keep track of. In a plain browser there is no shell to ask and no second
   * window to collide with, so the claim is a no-op.
   */
  /**
   * Select a worktree, and report whether the selection actually landed.
   *
   * The boolean exists for callers that do something *to* the worktree they are
   * switching to — today the rail's attention affordance, which opens a Nodes
   * pane. A claim can be refused, and a refusal that still wrote into that
   * worktree's layout would open a pane in a set of panes another window owns.
   */
  const selectWorktree = async (w: Worktree): Promise<boolean> => {
    // A detached window shows one dock of a worktree its origin owns; it is a
    // satellite of that claim and never makes one of its own.
    if (chromeless) {
      setActiveRepoRoot(w.repo_root);
      setActiveWtKey(String(w.id));
      setShownId(w.id);
      return true;
    }
    // A click owns the outcome from here: cancel whatever acquire is running, so
    // a hunt still waiting on somebody's yield cannot land afterwards and move
    // the window off the row that was just picked.
    acquireGenRef.current++;
    // Counted, not just awaited: the re-acquire effect must not start an acquire
    // while this click's own claim is unanswered. See `claimsInFlight`.
    claimsInFlight.current++;
    let result: ClaimResult;
    try {
      result = await channel.claim(w.id);
    } finally {
      claimsInFlight.current--;
    }
    if (!result.ok) {
      // Without this the click reads as ignored: the row does not open, and
      // either a different window comes forward with nothing tying the two
      // together, or — when the holder is a browser tab — nothing visibly
      // happens at all. The notice is in *this* client, which is where the
      // person is looking.
      if (result.reason === "shown_elsewhere") notifyRedirect(holderNotice(w, result.holder));
      // Nothing to say for `superseded` — a later click owns the outcome — but
      // an unreachable daemon has to be said, or the click reads as ignored.
      else if (result.reason === "offline") {
        notifyRedirect("Not connected to the Veld daemon yet — try that again in a moment");
      }
      // **The click cancelled whatever was running, and then failed.** The bump
      // above is pre-emptive by necessity — a hunt has to stop the moment the
      // user picks something, not when the daemon eventually answers — so a
      // refused click can leave this client having cancelled its own acquire and
      // acquired nothing, with the boot effect keyed on a selection that did not
      // change. Re-arm, unless something was granted in the meantime.
      //
      // **Only when somebody else has it.** `superseded` means a *later* request
      // from this client owns the outcome, so re-arming there starts an acquire
      // whose claim outranks the one still in flight — and that one is then
      // refused as superseded, which the UI deliberately says nothing about. The
      // user's second click would vanish, which is the symptom two earlier
      // rounds of this review already removed once.
      if (result.reason === "shown_elsewhere" && shownRef.current === null && worktreeRef.current) {
        void acquireRef.current(worktreeRef.current);
      }
      return false;
    }
    setActiveRepoRoot(w.repo_root);
    setActiveWtKey(String(w.id));
    // Both, and `grantedRef` is not redundant: it is what stops a reconnect
    // asking for a worktree this client *yielded* (where `shownId` is also
    // null), and a click can be the only grant this client ever gets — it
    // supersedes a boot claim without changing the selection.
    grantedRef.current = true;
    setShownId(w.id);
    return true;
  };

  // Self-heal the URL to the RESOLVED selection: a stale/deep-linked
  // `?repo=`/`?wt=` that doesn't resolve falls back (repos[0] / main) for
  // display, and the URL must advertise what is actually shown — otherwise a
  // copied link carries a dead selection. Skipped until the first list load.
  useEffect(() => {
    if (!repoList) return;
    const p = new URLSearchParams(window.location.search);
    if (repo) p.set("repo", repo.root);
    else p.delete("repo");
    if (worktree) p.set("wt", String(worktree.id));
    else p.delete("wt");
    const query = p.toString();
    const next = query ? `?${query}` : "";
    // Every poll produces fresh repo objects; skip the no-op replaceState.
    if (next === window.location.search) return;
    window.history.replaceState(null, "", next || window.location.pathname);
  }, [repoList, repo, worktree]);

  // Optimistic pending markers while 202'd start/stop/restarts take effect,
  // keyed by worktree AND environment name: the rail can fire actions on several
  // rows at once, and a single global slot would let the second overwrite the
  // first's marker and strand its spinner. The environment half matters for the
  // same reason one directory apart — stopping one run while another starts in
  // the same worktree used to collapse into one slot. Each entry clears when
  // THAT environment's run signature moves off the value it had when the action
  // was fired.
  //
  // Declared **above** the derived run state, not beside its helpers below,
  // because the binding depends on it: a chosen environment that does not exist
  // *yet* is only distinguishable from one that is gone by whether this window has
  // a start in flight against that name.
  const [pending, setPending] = useState<PendingMap>({});

  // ---- derived run state --------------------------------------------------
  const runs = worktree ? runsForWorktree(envs, worktree) : [];
  const [selectedRunName, setSelectedRunName] = useSelectedRun(worktree?.path);
  /**
   * The run every surface in this window is bound to, and whether the stored
   * choice still resolves.
   *
   * One resolution point, used by the top bar, the URL launcher, the diagnostics
   * panes, the resource graphs and the Sharing surface. Before this each of them
   * called `activeRun`/`diagnosticsRun` and got whichever run sorted first, so a
   * second environment in the same directory — a coding agent's, typically — was
   * invisible: no dot, no logs, no way to stop it.
   */
  const pick = pickRun(runs, selectedRunName || null);
  /**
   * A chosen environment this window has just asked for and the daemon has not
   * listed yet.
   *
   * It makes the binding **empty rather than fallen-back**, which is the whole
   * point. `pickRun` falls back so a surface always has something to render, but
   * during a start the selector names the new environment while the fallback is a
   * *different, live* run — so the bar read `dev-2` next to a ■ whose tooltip said
   * `Stop dev`, and clicking it stopped a run the user was not looking at. That is
   * the exact defect this whole change exists to remove, reintroduced in the gap
   * between a click and a poll.
   */
  const awaitingStart =
    pick.missing !== null &&
    worktree !== null &&
    pending[pendingKey(worktree.id, pick.missing)]?.label === "start";
  // The selection as of *this* render, readable from a callback that fires later.
  // A start's failure handler needs to know whether the user has since chosen
  // something else, and its closure captured the value at click time. Same pattern
  // `layoutsRef` uses below, for the same reason.
  const selectedRunRef = useRef(selectedRunName);
  selectedRunRef.current = selectedRunName;
  const run = awaitingStart ? null : pick.run;
  const urls = sortedUrls(run);
  const status = runStatus(run);
  /** The worktree's other environments — what the run selector offers. */
  const siblings = siblingRuns(runs, run?.name);
  /**
   * The worst status among the runs the top bar is NOT showing.
   *
   * The selector's counter carries this, so a hidden run that failed or is stuck
   * recovering colours the control the user is already looking at. Reporting the
   * healthiest sibling instead would hide precisely the case this exists for.
   *
   * **Live siblings only.** An environment whose last run failed keeps
   * `status: "failed"` as history with `live: false`, so counting those made the
   * counter permanently red over a run that ended days ago, with no way to dismiss
   * it but to start that environment again — and the rail, one screen away, made
   * the opposite choice (`worstStatus(liveAll)`). Two surfaces disagreeing about
   * "does anything need attention" is worse than either answer.
   */
  const siblingStatus = worstStatus(liveRuns(siblings));

  // The permission policy every browser pane in this window is answered against.
  //
  // Pushed from here because this is what knows both halves: the rules are the
  // selected worktree's `ide.permissions`, and the trusted origins are the URLs
  // veld itself serves for its run — the only origins a pane may capture its own
  // contents at without asking, which is what makes `veld feedback` screenshots
  // work inside a pane. Re-sent whenever either changes; a no-op in the browser
  // build, which has no panes to govern.
  const permissionRules = worktree?.ide.permissions ?? [];
  /**
   * Every URL this worktree serves, across ALL its runs — not just the selected
   * one.
   *
   * Scoping this to the selected run would mean switching the run selector
   * silently revokes an already-open browser pane's self-capture permission: the
   * pane keeps rendering, its screenshots stop working, and nothing on screen
   * says why. A pane is worktree-scoped, so the trust set is too. The launcher
   * below still lists only the selected run's URLs — that one is a question about
   * a run, this is a property of the directory.
   */
  //
  // Sorted, and that is not cosmetic: `RunInfo.urls` is a Rust `HashMap`, the one
  // collection on that payload the daemon does not sort, so its JSON key order
  // differs between responses. The joined string below is an effect dependency, so
  // an unsorted list republishes every pane's permission policy on every poll —
  // the exact thing the comment under it says must not happen.
  //
  // Byte order, not `localeCompare`: that returns 0 for canonically-equivalent but
  // *unequal* strings (an NFC and an NFD hostname), and `Array.sort` is stable, so
  // those two would keep the HashMap's arbitrary order and the dependency string
  // would still flip between polls. A total order is the requirement here, not a
  // human-readable one — nothing renders this list.
  const trustedOrigins = runs
    .flatMap((r) => Object.values(r.urls))
    .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  useEffect(() => {
    // The daemon's own origin is in the set because veld's UI is served from it,
    // and a pane pointed at a veld surface is still veld's own page.
    setBrowserPolicy(permissionRules, [...trustedOrigins, window.location.origin]);
    // Serialised rather than compared by reference: both are rebuilt on every
    // poll, so an identity dependency would push the policy several times a
    // second and republish every pane's panel with it.
  }, [JSON.stringify(permissionRules), trustedOrigins.join(" ")]);

  // What the diagnostics panes and the Sharing surface read: the bound run, which
  // may be an ended one — logs, last node states and a share left to stop all
  // outlive the run itself, and "what happened to the one I was watching" is the
  // question right after it dies.
  //
  // `pick.run`, deliberately, not the controls' `run`: only the *controls* need the
  // binding emptied while a start is in flight (they must not act on a run they are
  // not naming). Emptying it here too made an open logs pane and the Sharing dialog
  // claim "start the run and its logs appear here" while a run was live in that very
  // worktree and another was starting. Both panes name the run they are showing, so
  // the fallback is legible rather than misleading.
  const diagRun: RunInfo | null = pick.run;
  const diagRef = worktree && diagRun ? runRef(worktree.path, diagRun) : null;
  // The running run's nodes that declare actions, raised to the top bar
  // (shared/NodeActions.tsx). The new-pane chooser used to embed the same buttons as
  // a fourth group; it does not any more — the top bar carries them permanently, and
  // a chooser nobody could read is not improved by putting the same surface on it
  // twice. Only a *live running* run can act:
  // an ended one's actions would spawn against whatever is current. `nodeRows`
  // already nulls historical actions; gating on `running` closes the live-but-
  // not-running case (a run bound but stopped).
  const actionNodes =
    diagRun && diagRun.status === "running"
      ? nodeRows(diagRun, null).filter((n) => n.actions.length > 0)
      : [];
  const nodeActionProps =
    diagRef && actionNodes.length > 0
      ? {
          run: diagRef,
          nodes: actionNodes,
          onChanged: () => void refresh(),
        }
      : null;
  const diagStats =
    worktree && diagRun
      ? stats?.projects?.[worktree.path]?.[diagRun.name]
      : undefined;
  /** Why a worktree has nothing to show — the one wording for every empty pane. */
  const runEmptyHint = !worktree
    ? "Select a worktree."
    : !worktree.has_veld_config
      ? "This worktree has no veld.json, so there is nothing to run."
      : !repo?.available
        ? "Repository directory not found on disk — showing last known state."
        : "Start the run and its logs, nodes and URLs appear here.";

  // Start configuration (preset or explicit node selections), remembered
  // per worktree. Falls back to a sensible default: first preset, else all
  // nodes at their default variants.
  // Reactive copy for the top bar's StartConfig; the rail's per-row controls
  // read the same storage through `resolveStartSelection` instead, since a
  // hook can't be called per row.
  const startKey = startStorageKey(worktree?.path ?? "_");
  const [startRaw, setStartRaw] = usePersisted(startKey, "");
  // The user's *deliberate* choice, if any. `null` on a worktree that has never
  // been started here — the point of the first-user-test change: nothing is
  // pre-selected, and the start button offers the picker instead of guessing.
  const storedStart = worktree
    ? pruneStartSelection(worktree, parseStartSelection(startRaw))
    : null;
  const effectiveStart = worktree
    ? (storedStart ?? defaultStartSelection(worktree))
    : null;
  // Whether the Start-configuration modal is open, and whether Done should start
  // (true when it was opened from the ▶ start button on a selection-less worktree).
  const [startConfigOpen, setStartConfigOpen] = useState(false);
  const startAfterDone = useRef(false);

  // Worktrees this window has asked to delete *permanently* and that have not
  // yet vanished. The daemon reports the true point of no return as
  // `worktree.deleting` only once `git worktree remove` is actually running;
  // for the window the user just confirmed from, the intent matters from the
  // click — a long run teardown can leave the row looking like recoverable
  // trash for up to STOP_TIMEOUT, which is exactly the ambiguity this lane
  // exists to remove. So the confirmed ids ride here too, and rendering treats
  // either source identically (see `isDeleting`). The daemon's flag keeps the
  // state honest across windows and for retention sweeps the UI never fired.
  //
  // **Local, optimistic, pruned on every poll.** The same discipline as
  // `pending` — see the effect below.
  const [deletingIds, setDeletingIds] = useState<Set<number>>(new Set());
  const isDeleting = (w: Worktree) => w.deleting || deletingIds.has(w.id);
  // Every loaded worktree, not just the selected repo's: switching projects
  // mid-action must not look like the worktree vanished, or the marker is
  // dropped and the re-enabled control invites a double fire.
  const allWorktrees = useMemo(
    () => repos.flatMap((r) => r.worktrees),
    [repos],
  );
  /**
   * The same list through a ref, for the notification path.
   *
   * Mirrored during render for the reason `worktreesRef` is, and read by
   * `focusPane` and the inbox's `onEvent` subscriber — both of which must resolve a
   * worktree in **any** project. `worktreesRef` is the selected project's list, and
   * looking an event's worktree up there is what made a notification from another
   * project a no-op on click: the lookup missed, the toast's heading fell back to
   * "Veld", and clicking it did nothing at all.
   */
  const allWorktreesRef = useRef(allWorktrees);
  allWorktreesRef.current = allWorktrees;
  /** Projects through a ref, so the notification path can name the one an event
   *  came from without being keyed on the 5s poll. */
  const reposRef = useRef(repos);
  reposRef.current = repos;
  /** The selected project's root, likewise — the notification heading names a
   *  project only when the event came from a different one. */
  const activeRepoRootRef = useRef(activeRepoRoot);
  activeRepoRootRef.current = repo?.root ?? activeRepoRoot;
  useEffect(() => {
    setPending((cur) =>
      prunePending(cur, Date.now(), (key) => {
        const parsed = parsePendingKey(key);
        if (!parsed) return null;
        const wt = allWorktrees.find((w) => w.id === parsed.worktreeId);
        return wt
          ? runSignatureFor(runsForWorktree(envs, wt), parsed.runName)
          : null;
      }),
    );
    // `poll` is a dependency so the TTL is re-checked on every tick, not only
    // when the payload changes: a failed refresh leaves `envs` identical, and
    // without this a marker could never expire while the daemon is down.
  }, [envs, allWorktrees, poll]);
  // Prune the optimistic deletion set to what is still a trashed worktree. A
  // successful removal drops the row entirely; a failed one comes back
  // *untrashed* (with `trash_error`) so it rejoins the rail; a restore un-trashes
  // it too. In every case `trashed_at` is empty or the id is gone, so keeping
  // only ids that are still trashed is exactly "still on its way out". Without
  // this the set would grow forever with every row ever confirmed for deletion.
  useEffect(() => {
    setDeletingIds((cur) => {
      if (cur.size === 0) return cur;
      const next = new Set<number>();
      for (const id of cur) {
        const wt = allWorktrees.find((w) => w.id === id);
        if (wt && wt.trashed_at) next.add(id);
      }
      return next.size === cur.size ? cur : next;
    });
  }, [allWorktrees]);
  /**
   * Whether ANY action is in flight on this worktree.
   *
   * The rail row and the ⌘K entries act on one worktree without naming a run, so
   * this is the predicate that gates them; the top bar, which is bound to a
   * specific environment, uses `pendingForRun` instead. Keeping both means the
   * rail cannot double-fire while the top bar still reports per-run state
   * correctly.
   */
  const pendingFor = (w: Worktree | null): PendingAction | null => {
    if (!w) return null;
    for (const [key, marker] of Object.entries(pending)) {
      if (parsePendingKey(key)?.worktreeId === w.id) return marker.label;
    }
    return null;
  };
  const pendingForRun = (
    w: Worktree | null,
    runName: string | undefined,
  ): PendingAction | null =>
    w && runName ? (pending[pendingKey(w.id, runName)]?.label ?? null) : null;

  /** Run actions make sense only for a worktree of an on-disk repo. */
  const canRunWorktreeNow = (w: Worktree) =>
    w.has_veld_config && (repo?.available ?? false);

  // The top-bar node-actions button. `nodeActionProps` being null is a *disabled*
  // state, governed by `ui.hideDisabledActions` like the other inapplicable
  // actions: hidden when the setting is on, shown greyed with a reason when off.
  const nodeActionsDisabled = !nodeActionProps;
  const nodeActionsButton =
    worktree &&
    canRunWorktreeNow(worktree) &&
    !(nodeActionsDisabled && hideDisabled) ? (
      <NodeActionsButton
        disabled={nodeActionsDisabled}
        run={nodeActionProps?.run ?? null}
        nodes={nodeActionProps?.nodes ?? []}
        onChanged={() => void refresh()}
      />
    ) : null;

  /**
   * Whether any of the selected worktree's machine vars has a value answered on
   * this machine, plus a tick to force a re-read after the vars dialog closes.
   *
   * Powers the small badge on the variables button: a project whose vars are
   * already answered reads as settled, and one with a gap (or none at all) does not. A
   * var counts as overridden when its scope is `project` or `worktree` — the two
   * values *this* machine supplied — never `default` (the config's own fallback)
   * or `unset`.
   */
  const [configVarsOverridden, setConfigVarsOverridden] = useState(false);
  const [varsTick, setVarsTick] = useState(0);
  /** The "update main" action is in flight (the top bar button spins). */
  const [updatingMain, setUpdatingMain] = useState(false);
  useEffect(() => {
    const path = worktree?.path;
    // No config (or one that declares nothing) cannot be overridden. `null`
    // `machine_vars` (an unreadable config) falls through to the fetch, which
    // errors and reads as not-overridden — the honest answer.
    if (!path || !worktree.has_veld_config || worktree.machine_vars === 0) {
      setConfigVarsOverridden(false);
      return;
    }
    let cancelled = false;
    api
      .configVars(path)
      .then((r) => {
        if (!cancelled) {
          setConfigVarsOverridden(
            r.vars.some((v) => v.from === "project" || v.from === "worktree"),
          );
        }
      })
      .catch(() => {
        if (!cancelled) setConfigVarsOverridden(false);
      });
    return () => {
      cancelled = true;
    };
  }, [worktree?.path, varsTick]);

  /**
   * Whether ▶ can do anything for this worktree. One predicate for ALL FOUR
   * entry points (top bar, rail row, context menu, palette) — they disagreed
   * before: some checked "is anything in flight", others "is there anything
   * to start", so one surface offered an enabled button whose click was a
   * silent no-op while another allowed a double-spawned `veld start`.
   * Declared after `canRunWorktreeNow` on purpose: it closes over it, and
   * TypeScript does not flag use-before-declaration through a closure.
   */
  const canStartWorktree = (w: Worktree) =>
    canRunWorktreeNow(w) &&
    pendingFor(w) === null &&
    resolveStartSelection(w) !== null;


  /**
   * Fire a run action, marked against the environment it targets.
   *
   * `runName` is required rather than derived: a start's target does not exist
   * yet, so only the caller knows which name the action is about, and marking it
   * under a name the daemon has not created is exactly right — the marker clears
   * when that name appears.
   */
  const act = async (
    w: Worktree,
    runName: string,
    label: PendingAction,
    fn: () => Promise<void>,
    /** Undo whatever the caller changed optimistically alongside the marker. */
    onFailure?: () => void,
  ) => {
    const key = pendingKey(w.id, runName);
    const sigAtSet = runSignatureFor(runsForWorktree(envs, w), runName);
    setPending((cur) => ({
      ...cur,
      [key]: { label, sigAtSet, expiresAt: Date.now() + PENDING_TTL_MS },
    }));
    try {
      await fn();
    } catch (e) {
      setPending((cur) => {
        const next = { ...cur };
        delete next[key];
        return next;
      });
      onFailure?.();
      // Name the worktree: actions fire from the rail, the context menu and the
      // palette on ANY row, so an unattributed message leaves the user guessing
      // which one failed.
      notifyError(`${label} failed on ${worktreeLabel(w)}`, e);
    }
  };

  /**
   * Which run a **worktree-level** surface acts on: the rail row, the context
   * menu, the palette.
   *
   * Still `activeRun`, deliberately. Those surfaces name a worktree and never a
   * run — a rail row cannot ask "which one" — and their ■ appears exactly when
   * `worktreeStatus` says something is live. Binding them to the window's *chosen*
   * run instead would silently break that pairing: with an ended run selected and
   * a sibling live, the row would show ■ (a run IS up) and clicking it would do
   * nothing at all.
   *
   * The top bar is the run-level surface and passes its bound run explicitly.
   */
  const targetRun = (w: Worktree): RunInfo | null =>
    activeRun(runsForWorktree(envs, w));
  /** The environment name ▶ will start for a worktree — see `startRunName`. */
  const startNameFor = (w: Worktree): string =>
    worktree && w.id === worktree.id
      ? startRunName(w.alias, runs, run)
      : startRunName(w.alias, runsForWorktree(envs, w), null);

  /**
   * The name an explicit "start **another** run" creates — always a fresh
   * environment.
   *
   * Not `startNameFor`, and the difference is not cosmetic: that one re-runs a
   * *bound ended* environment under its own name, which is right for ▶ ("run that
   * again") and wrong for a command that says "another". With a crashed `dev`
   * selected next to a live `api`, the two answers are `dev` and `dev-2`.
   *
   * `freshRunName`, so history is avoided too — a stopped `dev` in the list makes
   * "another run named `dev`" a false label, not a new environment.
   */
  const anotherNameFor = (w: Worktree): string =>
    freshRunName(w.alias, runsForWorktree(envs, w));

  /**
   * Whether a **second** environment can be started here while another is live.
   *
   * The ▶/■ control is a toggle: once anything is live it shows ■, so all four run
   * surfaces offered "start" only while the worktree was idle. That left a state
   * the product now treats as ordinary — two environments in one directory —
   * reachable from the CLI alone, and made the run selector's "▶ starts a run named
   * `dev-2`" a promise nothing could keep.
   *
   * Gated on `pendingForRun`, deliberately not on `pendingFor`: the point is
   * starting one environment while another is mid-transition, which a
   * worktree-wide "is anything in flight" check forbids outright. The name checked
   * is the one `anotherNameFor` will use, so a double click is still blocked by that
   * name's own marker.
   *
   * Requires something to be **live**, because that is the only state in which
   * "another" means anything: with the whole directory stopped, ▶ is the start
   * affordance and it re-runs the environment you are looking at. Offering both
   * there put two start actions side by side, one of which quietly created a
   * *different* environment than the one on screen.
   */
  const canStartAnother = (w: Worktree) =>
    canRunWorktreeNow(w) &&
    resolveStartSelection(w) !== null &&
    liveRuns(runsForWorktree(envs, w)).length > 0 &&
    pendingForRun(w, anotherNameFor(w)) === null;

  // Run actions for ANY worktree, not just the selected one — the rail rows,
  // the context menu and the palette all drive these.
  //
  // `name` overrides the default target, which is how "start another run" asks for
  // a fresh environment rather than for whatever ▶ would have started.
  const startWorktree = (w: Worktree, name?: string) => {
    const sel = resolveStartSelection(w);
    if (!sel) {
      // Defence in depth: all four ▶ surfaces gate on `canStartWorktree`,
      // which rejects exactly this case, so this should be unreachable. If a
      // future caller skips the guard, say what's wrong instead of no-opping.
      notifyError(
        `Start ${worktreeLabel(w)}`,
        "nothing to start — no presets or startable nodes in its veld.json.",
      );
      return;
    }
    const body = startBody(sel);
    // The name is computed here and sent explicitly. Leaving it to the daemon's
    // default (the worktree alias) is what minted a *third* environment when an
    // agent already had one live, and it also meant this window could not mark
    // the action against the run it was about.
    const target = name ?? startNameFor(w);
    void (async () => {
      // Ask before starting, not after failing. A daemon-spawned `veld start`
      // has no terminal, so its own pre-flight can only refuse — this is the
      // channel that refusal assumes exists.
      //
      // Ahead of the run binding below on purpose: a start held back here never
      // happened, so binding this window to `target` first would point it at a
      // run nothing created.
      try {
        const { needed } = await api.configVarsPreflight({
          project: w.path,
          ...body,
        });
        if (needed.length > 0) {
          setDialog({
            kind: "config-vars",
            project: w.path,
            // Carries `name` through, so retrying a "start another run" still
            // starts *another* one rather than the default.
            retry: () => startWorktree(w, name),
          });
          return;
        }
      } catch {
        // Advisory only. If the check itself fails — an older daemon, a config
        // that no longer parses — start anyway and let `veld start` report the
        // real problem. Refusing to start because a *check* broke would be a
        // worse failure than the one it was looking for.
      }
      /**
       * Bind this window to what it just started.
       *
       * Written at the moment of the action, not repaired afterwards by an effect:
       * the intent ("I started this, I am looking at it") exists only here, and a
       * poll-time rule like "select the newest run" would also hijack the selection
       * when the *other* thing in this directory — a coding agent — starts one.
       *
       * Only for the selected worktree: a rail row's ▶ deliberately does not move
       * the selection, and another worktree's choice is stored under its own key.
       */
      const previous = worktree && w.id === worktree.id ? selectedRunName : null;
      if (previous !== null) setSelectedRunName(target);
      void act(
        w,
        target,
        "start",
        () => api.startRun(w.id, { ...body, run_name: target }),
        // The start never happened, so leave the window bound to what it was
        // looking at rather than to a name nothing created — but only if the user has
        // not picked something else in the meantime. A failure can arrive seconds
        // later, and restoring unconditionally would then yank a selection they made
        // deliberately.
        () => {
          if (previous !== null && selectedRunRef.current === target) {
            setSelectedRunName(previous);
          }
        },
      );
    })();
  };
  // `w.path` is the run's project root — every worktree with a veld.json is
  // its own project (see `runsForWorktree`), and the run name alone would be
  // ambiguous across repos.
  //
  // `target` is how the top bar names the run it is bound to; without it these
  // fall back to the worktree-level answer (see `targetRun`).
  const stopWorktree = (w: Worktree, target?: RunInfo | null) => {
    const r = target ?? targetRun(w);
    if (r) void act(w, r.name, "stop", () => api.stopRun(runRef(w.path, r)));
  };

  /**
   * ▶ from any surface — the rail, the top bar, ⌘⇧Enter. With a stored choice
   * it starts straight through; with **no** choice yet it selects the worktree
   * and opens the picker, so a first start is "choose, then Done" — not a
   * silent default run of the first preset.
   *
   * Used to be three copies of this same branch (rail, top bar, and the
   * keyboard shortcut below reached in early without it), which is how the
   * keyboard one shipped missing the picker check the other two already had.
   * `resolveStoredSelection`, not the top bar's reactive `storedStart`: the
   * latter is a hook value scoped to whichever worktree is on screen when it
   * renders, and both the rail (per-row, no hook) and the keyboard chord
   * (fires long after render, off a ref) need to ask the question fresh for
   * an arbitrary worktree instead.
   */
  const startOrOpenPicker = (w: Worktree) => {
    if (resolveStoredSelection(w)) {
      startWorktree(w);
      return;
    }
    // No deliberate choice yet. Claim the row first (a row another window holds
    // cannot be driven from here), then open the picker for it.
    void (async () => {
      const ok = await selectWorktree(w);
      if (!ok) return;
      startAfterDone.current = true;
      setStartConfigOpen(true);
    })();
  };

  const restartWorktree = (w: Worktree, target?: RunInfo | null) => {
    const r = target ?? targetRun(w);
    if (!r) return;
    void (async () => {
      // Same pre-flight as ▶, and for the more likely case: pulling a commit
      // that adds a machine var with no default, then restarting what is already
      // up. Without it the daemon's headless refusal surfaces as a toast and the
      // dialog that could fix it never opens — the GUI dead end these endpoints
      // exist to remove, reintroduced on the path most likely to hit it.
      try {
        // The run's own nodes, not an empty list: with no selections the daemon
        // resolves an empty plan and every check passes, so the pre-flight would
        // silently never fire. A restart re-runs exactly these.
        const { needed } = await api.configVarsPreflight({
          project: w.path,
          selections: r.nodes.map((n) => `${n.name}:${n.variant}`),
        });
        if (needed.length > 0) {
          setDialog({
            kind: "config-vars",
            project: w.path,
            retry: () => restartWorktree(w, r),
          });
          return;
        }
      } catch {
        // Advisory, as on the start path: a broken check must not block a
        // restart that would otherwise work.
      }
      void act(w, r.name, "restart", () => api.restartRun(runRef(w.path, r)));
    })();
  };

  // ---- run diagnostics ----------------------------------------------------
  // One object rather than six props threaded through PaneArea → DockView: the
  // `logs` and `nodes` panes read the *selected* worktree's run, so every pane
  // re-points on a worktree switch and none of them captures a run of its own.
  //
  // One owner for "land a URL in the focused dock". ⌘K, the URL launcher, a
  // node's URL and an `ide.extensions` badge whose link asks for a pane all go
  // through this, rather than each inventing a placement. Updater form for the
  // reason `showBlankBrowser` uses one: a browser pane writes the layout on its
  // own schedule, so a value computed from this render would drop a navigation
  // that landed in the same commit.
  const openUrlInPane = (url: string, title?: string) =>
    setLayout((prev) => addTabToFocused(prev, browserTab({ url, ...(title ? { title } : {}) })));

  const runCtx: RunPaneContext = {
    ref: diagRef,
    run: diagRun,
    stats: diagStats,
    emptyHint: runEmptyHint,
    onChanged: () => void refresh(),
    // A node's URL, opened beside the terminal instead of in another application.
    // The same action ⌘K and the URL launcher offer, so all three land a pane in
    // the focused dock rather than each inventing a placement.
    onOpenPane: (name, url) => openUrlInPane(url, name),
    logsTz,
  };

  // ---- sharing ------------------------------------------------------------
  // Pending join requests are page-global, not scoped to the selected worktree:
  // someone waiting on a share whose worktree is not on screen must still be
  // visible, so the banner lists all of them and names the run each one is for.
  const pendingJoins = shares?.pending ?? [];
  const runShares = diagRun
    ? sharesForRun(shares?.shares ?? [], diagRun.run_id)
    : { peer: null, web: [] };
  /**
   * Shares of this worktree's *other* runs.
   *
   * A worktree can hold several environments, and a crashed run's shares outlive it
   * until the GC pass releases them — so scoping the Sharing surface to `diagRun`
   * alone hid live shares (possibly a public URL still serving) behind a button
   * that offered to start another one.
   */
  const worktreeShares = (shares?.shares ?? []).filter(
    (s) => s.run_id && runs.some((r) => r.run_id === s.run_id),
  );
  const otherRunShares = worktreeShares.filter((s) => s.run_id !== diagRun?.run_id);
  const sharingActive = worktreeShares.length > 0;
  /**
   * A share mutation fired from the palette.
   *
   * Not tracked as a pending marker: those watch the *run* signature
   * (`PendingAction`), which a share never moves — a marker for one would stay
   * stuck until its TTL. The 5s poll is what confirms it, and a failure surfaces
   * as a toast, like every other action's.
   */
  const shareAction = (label: string, fn: () => Promise<unknown>) => {
    void (async () => {
      try {
        await fn();
        await refresh();
      } catch (e) {
        notifyError(label, e);
      }
    })();
  };

  // ---- dialogs ------------------------------------------------------------
  const [dialog, setDialog] = useState<
    | { kind: "none" }
    | { kind: "import" }
    /** `lane` is where the new checkout is filed — `""` for ungrouped. Carried
     *  on the dialog state because the rail now has one create button per
     *  section, so "which lane" is decided by the click, not by the dialog. */
    | { kind: "new-worktree"; lane: string }
    | { kind: "sharing" }
    | { kind: "rename"; worktree: Worktree }
    /**
     * The trash confirmation, split out of the edit dialog: fetches the git
     * dirty state and offers trash-anyway vs revert-first.
     */
    | { kind: "trash"; worktree: Worktree }
    /**
     * A trashed worktree that is dirty, opened by the delete flow instead of
     * enqueueing a removal that would refuse. `status` is the fetched dirty
     * state; the dialog turns it into a choice (discard vs revert first).
     */
    | { kind: "confirm-delete"; worktree: Worktree; status: WorktreeGitStatus }
    /**
     * The top bar's "update main" hit a dirty repo root. `status` is the
     * fetched dirty state; the dialog turns it into a choice (revert first, or
     * cancel) instead of the daemon's refusal landing as a bare toast.
     */
    | { kind: "update-main-dirty"; root: string; status: WorktreeGitStatus }
    | { kind: "marker"; worktree: Worktree }
    /** `worktree` set means "create it, then move this one into it". */
    | { kind: "new-lane"; worktree?: Worktree }
    | { kind: "rename-lane"; lane: string }
    | { kind: "settings" }
    | { kind: "shortcuts" }
    | { kind: "remove-repo"; repo: Repo }
    | { kind: "search" }
    /**
     * Values this machine owes the project. `retry` re-fires the start that
     * was held back, so answering and starting is one flow rather than two.
     */
    | { kind: "config-vars"; project: string; retry?: () => void }
  >({ kind: "none" });

  // This app's own overlays are not portalled the way Mantine's are, so
  // `overlayGuard` cannot see them — they hide the embedded browser panes from
  // the state that opens them instead. Without this the ⌘K palette opens
  // *behind* a native view (see panes/overlayGuard.ts).
  useEffect(() => {
    if (dialog.kind === "none") return;
    pushBrowserSuspend();
    return popBrowserSuspend;
  }, [dialog.kind]);

  /**
   * emoji → every worktree holding it, across all repos. Keyed by id and
   * carrying ALL holders, not the first alias: aliases are unique only within
   * a repo (`unique_alias` scopes to one `repo_root`), so two projects both
   * checked out on `main` — the default case — would otherwise let the picker
   * mistake another project's glyph for the current worktree's own.
   */
  /**
   * Both marker faces, and which of the *selected worktree's own repo* siblings
   * hold each one.
   *
   * **Scoped to one repo**, which changed with the assigner: `pick_emoji` and
   * `pick_color` probe per repo now, so a glyph repeating across repos is the
   * expected result rather than a collision — `markers_may_repeat_across_repos`
   * pins it for the common two-repos-on-`main` case. Aggregating globally made the
   * picker warn about duplicates the assigner itself manufactures, and which the
   * rail can never show, because the rail renders one repo at a time.
   */
  const markerUsedBy = useMemo(() => {
    const emoji: Record<string, EmojiHolder[]> = {};
    const color: Record<string, EmojiHolder[]> = {};
    const repo = repos.find((r) => r.root === activeRepoRoot);
    for (const w of repo?.worktrees ?? []) {
      const holder = { id: w.id, label: worktreeLabel(w) };
      if (w.emoji) (emoji[w.emoji] ??= []).push(holder);
      if (w.marker_color) (color[w.marker_color] ??= []).push(holder);
    }
    return { emoji, color };
  }, [repos, activeRepoRoot]);

  const { showContextMenu } = useContextMenu();
  /**
   * Open a second full window already pointed at a worktree.
   *
   * The selection rides the URL rather than the new window's own persisted key,
   * because that key is per *slot* and a brand-new slot has nothing in it — the
   * window would open on whatever was last selected app-wide, which is exactly
   * the worktree you did not right-click.
   */
  const openNewWindow = async (payload: { repoRoot: string; worktreeId?: number }) => {
    if (!desktopWindow) return;
    try {
      const result = await desktopWindow.newWindow(payload);
      if (!result?.opened) {
        notifyError(
          "Couldn't open a new window",
          result?.reason === "cap"
            ? "Veld Desktop is at its window limit — close one and try again."
            : "The desktop shell refused the request.",
        );
      }
    } catch (err) {
      notifyError("Couldn't open a new window", err);
    }
  };

  const openWorktreeWindow = async (w: Worktree) => {
    await openNewWindow({ repoRoot: w.repo_root, worktreeId: w.id });
  };

  /**
   * Open a second full window on a *project*, with no worktree named.
   *
   * The window then runs its own acquire hunt and lands on whichever of that
   * project's worktrees is free — which is the right answer for "give me another
   * project to work in" and the reason this is not `openWorktreeWindow` with the
   * worktree left out by the caller: naming one would take it from whoever has it,
   * or be refused, for a request that never cared which.
   *
   * `desktop/src/windows.js`'s `appUrl` already builds `?repo=` without `?wt=`, so
   * the shell needs nothing new for this.
   */
  const openProjectWindow = async (repoRoot: string) => {
    if (desktopWindow) {
      await openNewWindow({ repoRoot });
      return;
    }
    // **A browser gets a second tab, not nothing.** A tab takes part in the
    // daemon's ownership arbitration exactly like a window does (that is why the
    // registry moved to the daemon at all), so this is a real second view of
    // another project and not a lesser imitation of one. Built from this page's own
    // URL so it works behind the gateway, on a dev server, and on any base path —
    // and `view` is carried because the new tab should open in the mode this one is
    // in rather than snapping back to the default.
    const url = new URL(window.location.href);
    url.search = "";
    url.searchParams.set("repo", repoRoot);
    const mode = new URLSearchParams(window.location.search).get("view");
    if (mode) url.searchParams.set("view", mode);
    const opened = window.open(url.toString(), "_blank");
    if (!opened) {
      notifyError(
        "Couldn't open a new tab",
        "Your browser blocked the pop-up — allow pop-ups for this site and try again.",
      );
      return;
    }
    // Same origin, so this is hygiene rather than a boundary: nothing in the new
    // tab needs a handle back to this one.
    opened.opener = null;
  };

  /**
   * Whether a second view of a project can be opened at all, and what to call it.
   *
   * Two environments, one action: Veld Desktop opens a window, a browser opens a
   * tab. Naming them differently is the point — "window" in a browser tab is a
   * promise about chrome the page cannot keep.
   */
  const canOpenSecondView = !!desktopWindow || typeof window.open === "function";
  const secondViewLabel = desktopWindow
    ? "Open in a new window"
    : "Open in a new tab";

  /**
   * Move a project to a new position, and tell the daemon.
   *
   * Optimistic on purpose: the column re-renders from `repos`, which is the poll's
   * list, so without this the square springs back to where it was and stays there
   * until the next 5s tick. `refresh()` afterwards is what makes the daemon's answer
   * the one that survives — including the entries this client did not know about,
   * which `reorder_repos` places for us.
   */
  const reorderProjectsTo = (from: number, to: number) => {
    const order = reorderedRoots(
      reposRef.current.map((r) => r.root),
      from,
      to,
    );
    setRepoList((cur) => {
      if (!cur) return cur;
      const listed = order
        .map((root) => cur.repos.find((r) => r.root === root))
        .filter((r): r is Repo => !!r);
      // **Anything the order did not name keeps its place at the end**, which is the
      // client-side mirror of what `reorder_repos` does server-side. Without it, a
      // project imported by another window and picked up by a poll this render has
      // not seen yet would vanish from the column and the menu until the next
      // refresh landed — dropped by a drag that had no idea it existed.
      const seen = new Set(listed.map((r) => r.root));
      const rest = cur.repos.filter((r) => !seen.has(r.root));
      return { ...cur, repos: [...listed, ...rest] };
    });
    void api
      .reorderProjects(order)
      .then(() => refresh())
      .catch((e) => {
        notifyError("Could not reorder projects", e);
        void refresh();
      });
  };

  /**
   * The project this window came *from*, for ⌘`.
   *
   * Two refs rather than one: `lastRepoRootRef` is what is on screen now, and the
   * previous value is only promoted when the selection actually moves. Written in
   * an effect keyed on the resolved project, so a *fallback* selection (a stale
   * `?repo=` that did not resolve) is recorded as the place you would go back to —
   * which is where you actually are.
   */
  /** The column toggle, for the key handler registered once at boot. */
  const toggleProjectColumnRef = useRef<() => void>(() => {});
  /** Same shape and same reason as `toggleProjectColumnRef`: the key handler and
   *  the Electron accelerator are registered once at boot, so they reach the
   *  current closure through a ref rather than capturing a stale one. */
  const openPaletteRef = useRef<() => void>(() => {});

  const previousRepoRootRef = useRef<string | null>(null);
  const lastRepoRootRef = useRef<string | null>(null);
  useEffect(() => {
    const root = repo?.root ?? null;
    if (!root) return;
    if (lastRepoRootRef.current && lastRepoRootRef.current !== root) {
      previousRepoRootRef.current = lastRepoRootRef.current;
    }
    lastRepoRootRef.current = root;
  }, [repo?.root]);

  /**
   * Go to the project at a keyboard position, or back to the previous one.
   *
   * Both read the list through `reposRef`, which is the *daemon's* order — the same
   * order the column renders — so ⌘2 means the same project in every window. Held
   * in refs because the key handler is registered once at boot; see the effect that
   * registers it.
   *
   * Selecting a project, not a worktree: `setActiveWtKey("")` lets the fallback pick
   * the main checkout, and the acquire hunt takes it from there. A chord that took a
   * specific worktree would fight whichever window already has it, for a gesture
   * that never named one.
   */
  const goToProject = (digit: number) => {
    const target = projectForShortcut(reposRef.current, digit);
    if (!target || target.root === lastRepoRootRef.current) return;
    setActiveRepoRoot(target.root);
    setActiveWtKey("");
  };
  const goToPreviousProject = () => {
    const target = toggleTarget(
      reposRef.current,
      lastRepoRootRef.current,
      previousRepoRootRef.current,
    );
    if (!target) return;
    setActiveRepoRoot(target.root);
    setActiveWtKey("");
  };

  /** The project column's right-click menu. The same actions the selector offers,
   *  where the pointer already is. */
  const projectMenu = (r: Repo) => (e: React.MouseEvent) => {
    e.preventDefault();
    showContextMenu([
      {
        key: "switch",
        title: `Switch to ${r.name}`,
        icon: <IconArrowsExchange size={14} />,
        onClick: () => {
          setActiveRepoRoot(r.root);
          setActiveWtKey("");
        },
      },
      ...(canOpenSecondView
        ? [
            {
              key: "new-window",
              title: secondViewLabel,
              icon: <IconExternalLink size={14} />,
              onClick: () => void openProjectWindow(r.root),
            },
          ]
        : []),
      { key: "divider" },
      {
        key: "remove",
        title: `Remove project ${r.name}…`,
        icon: <IconTrash size={14} />,
        color: "red",
        onClick: () => setDialog({ kind: "remove-repo", repo: r }),
      },
    ])(e);
  };

  const worktreeMenu = (w: Worktree) => {
    // A worktree whose removal has passed the point of no return is not in the
    // trash and cannot be restored or re-deleted — it is actively coming off the
    // disk. A menu of actions that would all fail is worse than no menu.
    if (isDeleting(w)) {
      return showContextMenu([
        {
          key: "deleting",
          title: "Being deleted — cannot be restored",
          // Disabled and inert: every action a row usually has (restore, delete,
          // start) would fail against a directory that is coming off the disk.
          disabled: true,
          onClick: () => {},
        },
      ]);
    }
    // A worktree on its way out has exactly one useful action. Offering the full
    // menu would put "Remove worktree…" on a row already being removed, and
    // "Start run" on a directory about to disappear.
    if (w.trashed_at) {
      return showContextMenu([
        {
          key: "restore",
          icon: <IconArrowBackUp size={14} />,
          title: "Restore",
          onClick: () => void restoreWorktree(w),
        },
        { key: "trash-divider" },
        {
          key: "delete-now",
          icon: <IconTrash size={14} />,
          title: "Delete permanently",
          color: "red",
          onClick: () => void deleteTrashedWorktree(w),
        },
      ]);
    }
    const running = worktreeStatus(runsForWorktree(envs, w)) !== "stopped";
    // Run entries live here as well as on the row, because the collapsed rail
    // has no space for inline controls and right-click is its only affordance.
    // `pendingFor` gates every entry: `running` lags a fired action by up to
    // one poll, so without it the menu keeps offering "Start run" while a
    // start is already in flight and `veld start` gets spawned twice.
    const busy = pendingFor(w) !== null;
    const runItems = canRunWorktreeNow(w)
      ? [
          {
            key: "run",
            title: running ? "Stop run" : "Start run",
            disabled: busy || (!running && !canStartWorktree(w)),
            onClick: () => (running ? stopWorktree(w) : startWorktree(w)),
          },
          {
            key: "restart",
            title: "Restart run",
            disabled: busy || !running,
            onClick: () => restartWorktree(w),
          },
          // Only while something is live: idle, the entry above already says
          // "Start run" and there is no *another* to distinguish it from. This is
          // the collapsed rail's only route to a second environment, since it has
          // no inline controls and no run selector.
          ...(running
            ? [
                {
                  key: "start-another",
                  title: `Start another run (${anotherNameFor(w)})`,
                  disabled: !canStartAnother(w),
                  onClick: () => startWorktree(w, anotherNameFor(w)),
                },
              ]
            : []),
          {
            // Reachable without a blocked start, so an answer can be changed
            // before it bites rather than only when it already has. Disabled on
            // the same condition as the top-bar button — an unreadable config
            // (`null`) still opens, because the dialog is where the reason is.
            key: "config-vars",
            title: "Values for this machine…",
            disabled: w.machine_vars === 0,
            onClick: () => setDialog({ kind: "config-vars", project: w.path }),
          },
          { key: "run-divider" },
        ]
      : [];
    return showContextMenu([
      ...runItems,
      // Electron only: a browser tab has no window manager to open one into.
      // The rail is where you pick a worktree, so it is where "…and put it on
      // the other monitor" belongs — the alternative is opening a window and
      // then navigating it to the worktree you were already pointing at.
      ...(desktopWindow
        ? [
            {
              key: "new-window",
              icon: <IconExternalLink size={14} />,
              title: "Open in a new window",
              onClick: () => void openWorktreeWindow(w),
            },
            { key: "new-window-divider" },
          ]
        : []),
      {
        key: "rename",
        title: "Rename…",
        onClick: () => setDialog({ kind: "rename", worktree: w }),
      },
      {
        key: "emoji",
        title: "Change marker…",
        onClick: () => setDialog({ kind: "marker", worktree: w }),
      },
      // Lane assignment as a submenu of the *existing* lanes, plus "New lane…".
      // A free-text field here would let two rows sit in "review" and "Review"
      // believing they are together, which is what `create_lane`'s
      // case-insensitive uniqueness exists to prevent.
      {
        key: "lane",
        title: "Move to group",
        items: [
          ...lanes.map((l) => ({
            key: `lane:${l.name}`,
            title: l.name,
            disabled: w.lane === l.name,
            onClick: () => void assignLane(w, l.name),
          })),
          ...(w.lane ? [{ key: "lane-none-divider" }] : []),
          ...(w.lane
            ? [
                {
                  key: "lane-none",
                  title: "Remove from group",
                  onClick: () => void assignLane(w, ""),
                },
              ]
            : []),
          ...(lanes.length > 0 ? [{ key: "lane-new-divider" }] : []),
          {
            key: "lane-new",
            title: "New group…",
            onClick: () => setDialog({ kind: "new-lane", worktree: w }),
          },
        ],
      },
      // The explicit mark-all-read. It lives here rather than on the rail's activity
      // glyph deliberately: that glyph used to be a button that marked the worktree
      // read, which made a click on the row mean two different things depending on
      // which pixel it hit. Selecting the worktree is what a click on a row does, so
      // the deliberate gesture belongs in the deliberate menu.
      //
      // Hidden rather than disabled when there is nothing to read: a greyed entry in a
      // context menu is a row of noise on every worktree that is quiet, which is most
      // of them most of the time.
      ...(inbox.hasUnread(w.id)
        ? [
            {
              key: "mark-read",
              title: "Mark read",
              // Reads the events, never touches what is *running*: `working` is a live
              // state, not something there is to have seen.
              onClick: () => inbox.markWorktreeRead(w.id),
            },
          ]
        : []),
      {
        key: "copy-path",
        title: "Copy path",
        onClick: () => void navigator.clipboard.writeText(w.path),
      },
      {
        key: "copy-branch",
        title: "Copy branch",
        onClick: () => void navigator.clipboard.writeText(w.branch),
      },
      ...(w.trash_error
        ? [
            { key: "trash-error-divider" },
            {
              key: "trash-retry",
              title: "Deletion failed — try again…",
              color: "red",
              // Straight into the confirmation, which now shows the recorded
              // refusal and the force checkbox beside it. Without this entry the
              // only way back to a forced removal was a dialog that no longer
              // knew a removal had been refused.
              onClick: () => setDialog({ kind: "trash", worktree: w }),
            },
            {
              key: "trash-error",
              title: "Dismiss deletion error",
              onClick: () => void dismissTrashError(w),
            },
          ]
        : []),
      { key: "divider" },
      {
        key: "remove",
        icon: <IconTrash size={14} />,
        title: "Move to trash…",
        color: "red",
        disabled: w.is_main,
        onClick: () => setDialog({ kind: "trash", worktree: w }),
      },
    ]);
  };
  const assignLane = async (w: Worktree, lane: string) => {
    try {
      await api.patchWorktree(w.id, { lane });
    } catch (e) {
      notifyError(`Could not move ${worktreeLabel(w)}`, e);
    }
    await refresh();
  };

  /**
   * Move `lane` onto the place `onto` currently holds — the drag's write path,
   * and the ⋮ menu's.
   *
   * One write for both gestures: `moveLane` owns the arithmetic and returns
   * `null` for a move that changes nothing, which a drop on the lane itself is.
   */
  const moveLaneTo = async (lane: string, onto: string) => {
    if (!repo) return;
    const order = moveLane(lanes, lane, onto);
    if (!order) return;
    try {
      await api.reorderLanes(repo.root, order);
    } catch (e) {
      notifyError("Could not reorder the groups", e);
    }
    await refresh();
  };

  const laneMenu = (lane: string) => {
    const index = lanes.findIndex((l) => l.name === lane);
    // One step is "swap places with that neighbour" — the same thing a drop onto
    // it says, which is why both go through `moveLane` by name. The bounds are
    // the `disabled` flags below; a neighbour that is not there is `null` here
    // and `moveLane` refuses it anyway.
    const move = (neighbour: Lane | undefined) =>
      void (neighbour && moveLaneTo(lane, neighbour.name));
    return showContextMenu([
      {
        key: "lane-rename",
        title: "Rename group…",
        onClick: () => setDialog({ kind: "rename-lane", lane }),
      },
      {
        key: "lane-up",
        title: "Move group up",
        disabled: index <= 0,
        onClick: () => move(lanes[index - 1]),
      },
      {
        key: "lane-down",
        title: "Move group down",
        disabled: index < 0 || index >= lanes.length - 1,
        onClick: () => move(lanes[index + 1]),
      },
      { key: "lane-divider" },
      {
        key: "lane-delete",
        title: "Delete group",
        color: "red",
        // No confirm: deleting a lane ungroups its worktrees and removes
        // nothing, so there is nothing to lose and a dialog would only train
        // people to dismiss dialogs.
        onClick: () => void deleteLane(lane),
      },
    ]);
  };

  const deleteLane = async (lane: string) => {
    if (!repo) return;
    try {
      await api.deleteLane(repo.root, lane);
    } catch (e) {
      notifyError(`Could not delete the group "${lane}"`, e);
    }
    await refresh();
  };

  const closeDialog = () => setDialog({ kind: "none" });

  // `dialog` is read inside the listener but deliberately not a dependency —
  // rebinding a window listener on every dialog change is wasteful, so the
  // current value comes through a ref instead.
  const dialogRef = useRef(dialog);
  dialogRef.current = dialog;
  // Key routing with a terminal on screen, since it is not what it looks like:
  // xterm binds keydown on its own textarea and calls preventDefault +
  // stopPropagation for every key it consumes, so for a focused terminal this
  // window listener (bubble phase, no capture) runs *after* xterm's and never
  // sees those keys at all. Consequences, both deliberate:
  //   - Escape reaches the shell, not this handler. vim, less and TUI menus keep
  //     working. The dialog guard below therefore only matters when focus is
  //     outside a terminal.
  //   - Ctrl+K is readline's kill-to-end-of-line and stays with the shell. That
  //     leaves the palette unreachable from a focused terminal on Linux/Windows
  //     (⌘K survives, because xterm doesn't claim meta combos), which is why
  //     Ctrl/⌘+Shift+P exists as a second accelerator — terminalHost lets that
  //     one through explicitly.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      // `!e.shiftKey` on the `k` arm is load-bearing, not defensive styling:
      // with Caps Lock on, Shift+K reports `e.key === "k"` (Caps Lock inverts
      // the usual shift-uppercases relationship per the UI Events spec), so
      // without this a Caps-Lock-on ⌘⇧K — the restart-run chord below — would
      // also open the palette on the same press.
      if (mod && ((e.key === "k" && !e.shiftKey) || ((e.key === "P" || e.key === "p") && e.shiftKey))) {
        e.preventDefault();
        openPaletteRef.current();
      }
      // ⌘, / Ctrl+, — the platform convention for preferences. Bound here so a
      // plain browser tab has the shortcut too; the Electron app *also* has it as
      // a menu accelerator, which is what makes it work while a native browser
      // pane has focus and swallows every keystroke the page would otherwise see.
      // `e.key` rather than `e.code`: on a German or French layout comma is not
      // on the key `Comma` names.
      if (mod && !e.shiftKey && !e.altKey && e.key === ",") {
        e.preventDefault();
        setDialog({ kind: "settings" });
      }
      // ⌘/Ctrl+/ — open the Shortcuts overview. The dialog's own reason to
      // exist is discoverability, so it gets the closest thing this app has
      // to a universal "help" chord (Slack, Notion, GitHub all bind it).
      // **Shift is not excluded**, unlike every mod-only chord above: `/` sits
      // behind Shift on German and Spanish layouts (QWERTZ), the same class
      // of layout hazard the comma chord above already documents by name —
      // excluding Shift the way `,` does would make this chord unreachable on
      // exactly those keyboards. **Not in a chromeless window** — it is "a
      // bare dock and nothing else" by design (see its own render branch
      // below) and renders no dialogs at all; setting this state there would
      // be a silent no-op.
      //
      // **`e.code === "Digit7"` is a second, `e.shiftKey`-gated match**, added
      // because `e.key === "/"` alone was reported still broken on German
      // layouts even after Shift stopped being excluded above. On German and
      // Spanish QWERTZ, `/` is physically Shift+7 (code `Digit7`) — the
      // suspected cause is a Chromium-on-macOS behaviour where holding
      // **Cmd** together with Shift+digit reports the unshifted `"7"` instead
      // of the shifted `"/"` for `e.key`, but this has not been confirmed
      // against a live German layout, only inferred from the symptom. This
      // fallback is the mitigation either way: it is gated on `e.shiftKey` so
      // it cannot also fire the shiftless ⌘7 "go to project 7" chord below,
      // and does nothing if `e.key === "/"` was already correct.
      if (mod && !e.altKey && (e.key === "/" || (e.shiftKey && e.code === "Digit7"))) {
        if (chromeless) return;
        e.preventDefault();
        setDialog({ kind: "shortcuts" });
        return;
      }
      // ⌘1…⌘9 / ⌘` — go to a project by position, or back to the last one.
      //
      // **`e.code`, not `e.key`.** On AZERTY and several other layouts the
      // unshifted digit row is punctuation, so ⌘2 arrives with `key === "é"`; the
      // chord people mean is the key with 2 printed on it. `key` stays as the
      // fallback for anything that reports no `code`.
      //
      // Bound here so a plain browser tab has them too — though only ⌘` survives
      // there, since Chrome and Safari reserve ⌘1-9 for their own tabs and a page
      // cannot intercept them. In Veld Desktop both work, and `browserViews.js`
      // forwards them back when a native browser pane has the keyboard.
      if (mod && !e.shiftKey && !e.altKey) {
        const digit = projectShortcutDigit(e.code, e.key);
        if (digit) {
          e.preventDefault();
          goToProject(Number(digit));
          return;
        }
        if (e.key === "`" || e.code === "Backquote") {
          e.preventDefault();
          goToPreviousProject();
          return;
        }
        // ⌘B — show/hide the project column. The near-universal "toggle the
        // sidebar" chord (VS Code, Slack), and free here. **Deliberately not
        // forwarded from a focused browser pane** the way ⌘K and ⌘1…⌘9 are: ⌘B is
        // bold in every rich-text editor, and taking it from a previewed page to
        // reach a toggle you can also click is a worse trade than ⌘F's was.
        // `e.key`, and **deliberately not `e.code`** — the opposite of the digits
        // above, which is why this is not the same test. `code` names the physical
        // key, which is right for a digit (⌘2 means the key with 2 printed on it)
        // and wrong for a letter: on Dvorak the key at QWERTY's `KeyB` prints `x`,
        // so matching `code` here would swallow ⌘X — cut — in every text field.
        if (e.key === "b" || e.key === "B") {
          // **Not while the caret is in a text field.** `mod` is `meta || ctrl`, so
          // this binds Ctrl+B as well — which is what Linux and Windows users will
          // press, and also the Cocoa emacs binding for "back one character" that
          // macOS honours in every editable field. Without the guard, Ctrl+B in the
          // new-worktree name box moved the project column instead of the caret.
          // Terminals are already safe: xterm consumes the key before this listener
          // ever runs (see the note above).
          if (isEditableTarget(e.target)) return;
          e.preventDefault();
          toggleProjectColumnRef.current();
          return;
        }
        // ⌘/Ctrl + ↑/↓/←/→ — move the rail's selection to the worktree above
        // or below the current one (←/→ are plain aliases for ↑/↓, for anyone
        // whose rail reads left-to-right in their head), in the order the
        // rail actually renders them (`railGroups`, flattened — not raw
        // `worktrees`, which is grouping-blind), wrapping past either end.
        // Guarded like ⌘B: `mod` alone also means Cmd+Up/Down's own
        // text-field binding on macOS (jump to the start/end of a textarea),
        // so a focused input keeps that instead. **Not in a chromeless window**
        // — a detached window is a satellite of the worktree its origin claimed
        // (`selectWorktree`'s own comment), and moving its selection off that
        // worktree is exactly the thing being a satellite means it must not do.
        if (
          e.key === "ArrowUp" ||
          e.key === "ArrowDown" ||
          e.key === "ArrowLeft" ||
          e.key === "ArrowRight"
        ) {
          if (isEditableTarget(e.target) || chromeless) return;
          e.preventDefault();
          stepWorktree(e.key === "ArrowUp" || e.key === "ArrowLeft" ? -1 : 1);
          return;
        }
      }
      // Ctrl+Tab / Ctrl+⇧Tab — focus the next/previous tab, continuing into
      // this worktree's own detached windows once the docked ones run out.
      // **Literal `ctrlKey`, not `mod`.** Cmd+Tab is the OS's own app switcher
      // on macOS and never reaches a page at all, so every tabbed app on that
      // platform (Safari, Chrome, VS Code) binds tab-switching to the physical
      // Ctrl key even there — the one chord in this file where the Mac/other
      // split does not run through `mod`. Not guarded on `isEditableTarget`: no
      // text field binds Ctrl+Tab for anything, and every tabbed app keeps this
      // working regardless of where the caret is. Runs everywhere, chromeless
      // included: a detached window has its own dock to cycle, and `stepTab`
      // is what makes that this worktree's *whole* tab list, not just this
      // window's slice of it.
      if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === "Tab") {
        e.preventDefault();
        stepTab(e.shiftKey ? -1 : 1);
        return;
      }
      // ⌘/Ctrl+⇧ + a letter, arrow or Enter — the run/worktree actions with no
      // chord yet: focus mode, the IDE/Runs view switch, update main, cycling
      // the run selector, start/stop, restart, and a second way to cycle
      // tabs. One guard for all of them: none means anything to a focused
      // text field, unlike ⌘B's emacs binding or Cmd+Up/Down's caret motion
      // above.
      if (mod && e.shiftKey && !e.altKey && !isEditableTarget(e.target)) {
        // ⌘⇧←/↑ (previous) and ⌘⇧→/↓ (next) — a mod+shift alias for the
        // Ctrl+Tab chord above, for anyone who reaches for shift-arrows
        // before they reach for the literal-Ctrl chord browsers reserve for
        // their own tabs. No `chromeless`/`mode` guard, same as Ctrl+Tab: a
        // detached window has its own dock to cycle too.
        if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
          e.preventDefault();
          stepTab(-1);
          return;
        }
        if (e.key === "ArrowRight" || e.key === "ArrowDown") {
          e.preventDefault();
          stepTab(1);
          return;
        }
        // Focus mode is a plain settings toggle with nothing worktree- or
        // view-specific about it, so it is the one chord in this block with no
        // `chromeless`/`mode` guard — it means the same thing everywhere.
        // **⌘⇧L, not ⌘⇧F.** The veld feedback overlay's own keydown listener
        // (`feedback-overlay/keyboard.ts`) binds mod+Shift+F itself (its
        // "select an element" mode) on a **capture-phase** document listener,
        // which always runs before this bubble-phase one and neither side
        // calls `stopPropagation` — the overlay's chord wins outright, by
        // design (the overlay keeps its own shortcuts), so this one has to
        // live somewhere else.
        if (e.key === "l" || e.key === "L") {
          e.preventDefault();
          saveSettingsRef.current({ "focus.enabled": !focusPrefsRef.current.enabled });
          return;
        }
        // Switching view is the one worktree-scoped chord that must keep
        // working from *both* views — that is its whole job — so it is
        // guarded on `chromeless` alone, never on `mode`.
        // **⌘⇧X, not ⌘⇧V** — the feedback overlay claims mod+Shift+V for its
        // own toolbar toggle, the same collision as ⌘⇧F above.
        if (e.key === "x" || e.key === "X") {
          e.preventDefault();
          if (chromeless) return;
          setModeRef.current(modeRef.current === "ide" ? "runs" : "ide");
          return;
        }
        // Everything below acts on the IDE cockpit's own selection (a
        // worktree, its run, its rail) and renders its confirmation dialogs
        // only in the IDE-mode return — see `settingsDialog`-style hoisting
        // above for the two that ARE hoisted, and note `update-main-dirty`,
        // the failure path here, is not one of them. Firing from Runs view or
        // a chromeless window would act on state the user cannot see or, for
        // `update-main-dirty`, silently open a dialog nothing renders.
        if (modeRef.current !== "ide" || chromeless) return;
        if (e.key === "u" || e.key === "U") {
          e.preventDefault();
          updateMainRef.current();
          return;
        }
        if (e.key === "o" || e.key === "O") {
          e.preventDefault();
          const wt = worktreeRef.current;
          if (wt && canRunWorktreeNowRef.current(wt)) stepPreset();
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          const wt = worktreeRef.current;
          if (!wt) return;
          // Mirrors `TopBar`'s ▶/■ button exactly: same `disabled` expression,
          // same choice of target by `running || starting`. The start side
          // goes through `startOrOpenPickerRef` rather than starting the
          // worktree directly — a worktree with no stored selection yet needs
          // the picker, the same as the ▶ button and the rail's row already do.
          const pendingAction = pendingActionRef.current;
          const starting = startingRef.current;
          const running = runningRef.current;
          const disabled =
            (pendingAction !== null && !starting) ||
            (!running && !canStartRef.current && !starting);
          if (disabled) return;
          if (running || starting) stopWorktreeRef.current(wt, boundRunRef.current);
          else startOrOpenPickerRef.current(wt);
          return;
        }
        if (e.key === "k" || e.key === "K") {
          e.preventDefault();
          const wt = worktreeRef.current;
          // Mirrors the restart button's own `disabled={!running || pending
          // !== null}` exactly. Not ⌘⇧R: that chord is Electron's own
          // `forceReload` menu accelerator (`desktop/src/main.js`'s Edit menu),
          // which the OS delivers to the menu before this listener ever runs —
          // the shortcut would silently reload the window instead of firing.
          if (wt && runningRef.current && pendingActionRef.current === null) {
            restartWorktreeRef.current(wt, boundRunRef.current);
          }
          return;
        }
      }
      if (e.key === "Escape" && dialogRef.current.kind !== "none") closeDialog();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // ---- Panes -------------------------------------------------------------
  // One layout per worktree, restored so a reload comes back to the same tabs —
  // and, because a terminal tab's id is its daemon session id, to the same
  // running shells. In Veld Desktop the layout also survives the app itself
  // restarting (see `layoutSlotKey`), which is the half that makes surviving
  // shells reachable after an app update rather than merely alive. Read with a
  // lazy initialiser rather than in an effect: an empty first render would prune
  // every restored session before the restore landed.
  /** Every worktree is shown by another window, so this one has nothing to
   *  display that is its own. */
  const [claimBlocked, setClaimBlocked] = useState(false);
  /**
   * The worktree this client has actually been **granted**, as opposed to the
   * one it has selected.
   *
   * The two are not the same and conflating them was a real defect: the panes of
   * a worktree name live PTY sessions, and attaching to one *takes it over*, so
   * a client that renders a layout before its claim is answered steals the
   * shells of whichever client legitimately holds it. On boot the gap is wide —
   * the selection is restored from `?wt=`/storage immediately while the claim
   * waits out every other holder's yield — which is exactly when a second client
   * is most likely to be up.
   *
   * So nothing reads a layout until this is set, and only a granted claim sets
   * it. A detached window sets it directly: it is a satellite of its origin's
   * claim and makes none of its own.
   */
  const [shownId, setShownId] = useState<number | null>(null);
  /** …and through a ref, for the reconnect handler, which is registered once. */
  const shownRef = useRef<number | null>(null);
  shownRef.current = shownId;
  /**
   * Whether this client has ever been granted a worktree.
   *
   * The difference between the two ways `shownId` can be `null`, and they need
   * opposite answers on reconnect. **Never granted** — the boot claim ran while
   * the socket was still connecting and answered `offline` — means the window is
   * showing nothing and must try again, or it stays blank forever with the boot
   * effect keyed on a selection that has not changed. **Granted and then
   * yielded** means another client has it now, and re-claiming on reconnect
   * would take it straight back off them.
   */
  const grantedRef = useRef(false);
  /** Bumped by anything that supersedes a running acquire — a newer acquire, or
   *  a rail click. See `acquireRef`. */
  const acquireGenRef = useRef(0);
  /**
   * How many claims this window is waiting on an answer for.
   *
   * **Read by the re-acquire effect, which must not fire into an open question.**
   * The daemon broadcasts the claims table as soon as it *records* a claim, up to
   * `YIELD_ACK` before it answers the claimer — so the click that wins a worktree
   * changes `freeKey` while `claimBlocked` is still set, and an effect keyed on
   * that alone would start an acquire that supersedes the very click that woke it.
   * A superseded claim is answered with `superseded`, which the UI deliberately
   * says nothing about: the user's click would simply vanish.
   */
  const claimsInFlight = useRef(0);
  /** The selection, for the reconnect handler — see `grantedRef`. */
  const selectedRef = useRef<number | null>(null);
  /** …and the row itself, for the paths that re-acquire it. */
  const worktreeRef = useRef<Worktree | null>(null);
  // Assigned during render, like `layoutsRef`: an acquire started from a socket
  // handler must see the selection this render was built from.
  selectedRef.current = worktree?.id ?? null;
  worktreeRef.current = worktree;
  /** Acknowledgements for yields whose release has been scheduled but not yet
   *  committed. State rather than a ref, because the point is to have something
   *  an effect can run *after* — see the yield handler below. */
  const [yieldAcks, setYieldAcks] = useState<(() => void)[]>([]);

  // Mirrored into a ref, the same idiom `dialogRef` uses above: an effect with `[]`
  // deps closes over the first render's `layouts` forever, and a decision that has to
  // be made *before* a `setLayouts` call cannot come from inside the updater — see the
  // terminal `open_url` handler.
  const layoutsRef = useRef<Record<number, PaneLayout>>({});
  const [layouts, setLayouts] = useState<Record<number, PaneLayout>>(() =>
    loadLayouts(layoutSlot, windowSeed, windowRestored, chromeless),
  );
  const layout = worktree ? layouts[worktree.id] : undefined;

  /**
   * Detached windows this app knows about, per worktree — in the order they
   * were opened. The only registry of them on this side: once a tab leaves via
   * `detach`/`dropOut`, it is gone from every `PaneLayout` here (see
   * `panes/model.ts`), so without this list "next/previous tab" cycling has no
   * way to reach a detached tab at all, only to skip past where it used to be.
   *
   * Not reconciled against which of these windows are still actually open —
   * there is no push from the shell for "a window closed". `focus()` returning
   * `false` is what tells the keyboard handler an entry is stale, at which
   * point it prunes that id; see the cycling helper in the keydown effect.
   */
  const [detachedWindows, setDetachedWindows] = useState<Record<number, string[]>>({});
  const detachedWindowsRef = useRef(detachedWindows);
  detachedWindowsRef.current = detachedWindows;
  const onWorktreeDetachedWindow = useCallback((worktreeId: number, windowId: string) => {
    setDetachedWindows((prev) => {
      const list = prev[worktreeId] ?? [];
      return list.includes(windowId) ? prev : { ...prev, [worktreeId]: [...list, windowId] };
    });
  }, []);
  /** Drop a stale id once `focus()` reports the window it named is gone. */
  const forgetDetachedWindow = useCallback((worktreeId: number, windowId: string) => {
    setDetachedWindows((prev) => {
      const list = prev[worktreeId];
      if (!list || !list.includes(windowId)) return prev;
      return { ...prev, [worktreeId]: list.filter((id) => id !== windowId) };
    });
  }, []);

  /**
   * The rail's attention affordance: go to this worktree's node health.
   *
   * Two steps that cannot be collapsed into one. The selection has to land first
   * — a claim can be refused, and `selectWorktree` reports that — and the pane
   * write has to wait for `layouts[id]` to exist, because the effect that seeds a
   * newly selected worktree's layout reads the **shared store** and only when it
   * finds no entry of its own. Writing a layout here would therefore make that
   * effect skip the read and grow a second set of panes for a worktree another
   * window has, which is the failure the shared store exists to prevent.
   *
   * So the request is recorded and drained once the layout is there. Declared
   * above the yield handler because that handler has to be able to abandon it:
   * see the comment there for the interleaving that made it necessary.
   */
  const [diagnoseFor, setDiagnoseFor] = useState<number | null>(null);
  const diagnoseWorktree = async (w: Worktree) => {
    if (!(await selectWorktree(w))) return;
    setDiagnoseFor(w.id);
  };
  useEffect(() => {
    if (diagnoseFor === null) return;
    // The selection went somewhere else (another click, the worktree was
    // forgotten). A request that waits for a layout it will never see would
    // otherwise fire on some later visit instead.
    if (worktree?.id !== diagnoseFor) {
      setDiagnoseFor(null);
      return;
    }
    const l = layouts[diagnoseFor];
    if (!l) return;
    const next = revealDiagPane(l, "nodes");
    setDiagnoseFor(null);
    if (next === l) return;
    setLayouts((prev) =>
      prev[diagnoseFor] === l ? { ...prev, [diagnoseFor]: next } : prev,
    );
  }, [diagnoseFor, worktree?.id, layouts]);

  useEffect(() => {
    // A detached window's panes are its own (`saveLayouts`); a main window's
    // belong to the worktree and go to the daemon, which is what makes them the
    // same panes in a browser tab and in the app.
    saveLayouts(layoutSlot, layouts, chromeless);
    if (!chromeless) syncLayouts(layouts);
  }, [layouts]);

  /**
   * Give a newly selected worktree a layout. New worktrees inherit the split of
   * one already open, so the proportions the user chose carry across instead of
   * snapping back to 50/50 on every new worktree.
   *
   * **Read from the daemon, at the moment of showing it.** A worktree has one
   * set of panes and the database holds it, so this is where a browser tab picks
   * up the very terminals the desktop app has running rather than inventing a
   * second set beside them. Reading it now rather than at boot is the same
   * discipline the shared store needed and for a stronger reason: another
   * *client* may have been using this worktree since, and there is no longer any
   * local copy that could stand in for what it left.
   *
   * **Keyed on the granted claim, never on the selection.** The selection is
   * restored from `?wt=`/storage the moment the worktree list resolves, while
   * the claim is still waiting out other clients' yields — so keying this on
   * `worktree?.id` fetched and mounted the panes first and learned they belonged
   * to somebody else afterwards, by which time this client had already taken
   * over their shells. On boot with a second client up, that is the common case
   * rather than a race.
   */
  useEffect(() => {
    const id = shownId;
    if (id === null) return;
    // A detached window's panes came with it and are not the worktree's.
    if (chromeless) {
      setLayouts((prev) =>
        prev[id]
          ? prev
          : { ...prev, [id]: defaultLayout(Object.values(prev)[0]?.ratio ?? DEFAULT_RATIO) },
      );
      return;
    }
    // Through the ref, not `layouts`: this effect is keyed on the selection
    // alone, so the render's `layouts` can be a commit behind a layout that has
    // just been adopted — and re-fetching one already on screen would restore an
    // older version over the user's last change.
    if (layoutsRef.current[id]) return;
    let cancelled = false;
    void (async () => {
      let stored: PaneLayout | null = null;
      try {
        stored = await readLayout(id);
      } catch {
        // The daemon is unreachable. Seeding a default is the same answer as
        // "nothing stored", and it cannot destroy anything: the first save
        // presents version 0, loses the check against whatever is really there,
        // and adopts it.
      }
      if (cancelled) return;
      // **Before the panes render.** These shells were expected to be running
      // when this client found them, so a terminal that cannot reattach says so
      // instead of quietly opening a fresh prompt — which is exactly what a
      // browser tab used to do for every terminal the app had open.
      if (stored) noteExpectedResumes(terminalIds(stored));
      setLayouts((prev) => {
        if (prev[id]) return prev;
        if (stored) return { ...prev, [id]: stored };
        return { ...prev, [id]: defaultLayout(Object.values(prev)[0]?.ratio ?? DEFAULT_RATIO) };
      });
    })();
    return () => {
      cancelled = true;
    };
  }, [shownId, chromeless]);

  /**
   * Let go of one worktree's panes, because another window is taking it.
   *
   * The in-memory half of "one set per worktree". A window keeps the panes of
   * worktrees it has visited — mounted and attached, so switching back is
   * instant rather than a reconnect and a scrollback replay — and gives one up
   * only when somebody else claims it. Releasing on every switch instead was
   * the first version, and it made the common case (one window, switching back
   * and forth) pay for a collision that could not happen.
   *
   * `releaseTerminal`, never `disposeTerminal`: letting go is not closing. The
   * shells keep running, the layout stays in the shared store, and the window
   * that claimed the worktree picks up both. Same distinction detach relies on,
   * and the release must happen *before* the layout leaves state, because
   * `pruneTerminals` ends whatever the layouts no longer name.
   *
   * **And the window that asked is waiting.** It does not attach to these
   * terminals until this window acknowledges, so the ack has to mean the release
   * has happened — not that the message arrived. The release runs inside the
   * `setLayouts` updater, i.e. during the render React schedules from here, so
   * the ack is queued for the effect below and sent after that commit.
   */
  const onYield = useRef<(worktreeId: number, ack: () => void) => void>(() => {});
  useEffect(() => {
    if (chromeless) return;
    onYield.current = (worktreeId: number, ack: () => void) => {
      // A pending "reveal node health" request for this worktree can never be
      // satisfied now, and its own guard cannot see that: a yield deletes the
      // layout without touching the *selection*, so `worktree?.id` still equals
      // `diagnoseFor` while the layout it is waiting for is gone for good —
      // nothing re-seeds it, because the seeding effect is keyed on the selection
      // too. Left set, the request would fire on some later visit and open a
      // Nodes pane nobody asked for then.
      setDiagnoseFor((cur) => (cur === worktreeId ? null : cur));
      // It is not granted to this client any more, so nothing may re-seed it —
      // and a reconnect must not ask for it back.
      setShownId((cur) => (cur === worktreeId ? null : cur));
      // Nor may a save composed before the release still land: it would be sent
      // at a version the *claiming* client has not moved yet, be accepted, and
      // replace the panes this handler just handed over.
      cancelPendingWrite(worktreeId);
      setLayouts((prev) => {
        const giving = prev[worktreeId];
        if (!giving) return prev;
        for (const tab of allTabs(giving)) {
          if (tab.kind === "terminal") releaseTerminal(tab.id);
        }
        const next = { ...prev };
        delete next[worktreeId];
        return next;
      });
      // Batched with the update above into one commit, which is what makes the
      // effect below run *after* the release rather than beside it. Queued even
      // when there was no layout to give up: the claimer is waiting either way,
      // and the answer is "nothing to let go of".
      setYieldAcks((q) => [...q, ack]);
    };
    return () => {
      // Back to a handler that releases nothing, which is what an unmounted app
      // can honestly promise. The daemon waits out its acknowledgement timeout
      // and proceeds — the documented fallback.
      onYield.current = () => {};
    };
  }, [chromeless]);

  /**
   * Answer the yields whose release is now on screen.
   *
   * A passive effect, because that is the first moment the `setLayouts` updater
   * above has actually run: acknowledging from inside the listener would promise a
   * release that React has not performed yet, and the claiming window attaches on
   * the strength of that promise.
   */
  useEffect(() => {
    if (yieldAcks.length === 0) return;
    const sent = new Set(yieldAcks);
    for (const ack of sent) ack();
    // **Filtered, never cleared.** React flushes a passive effect in a later task
    // than the commit it belongs to, so a socket frame can append a yield in
    // between — and `setYieldAcks([])` would discard it. That yield is then never
    // acknowledged at all and its claimer waits out the full acknowledgement
    // timeout for a release that has already happened, which is the one thing
    // this channel exists to avoid. Two review angles found this line
    // independently.
    setYieldAcks((q) => q.filter((ack) => !sent.has(ack)));
  }, [yieldAcks]);

  /**
   * Tell the daemon what this client is holding, so it knows who to ask.
   *
   * Only a main window: a detached one holds tabs transferred out of a worktree
   * its origin owns, and must never be asked to yield them — they are already
   * where they belong.
   */
  useEffect(() => {
    if (chromeless) return;
    channel.holds(Object.keys(layouts).map(Number));
  }, [chromeless, layouts]);

  /**
   * Tell the *shell* which worktree is on screen, which is a different question.
   *
   * The daemon arbitrates ownership; Electron only needs this to route a
   * cross-window tab drop at a window those tabs belong in — a question about
   * its own windows, and the one thing it can still answer better than anyone.
   * A detached window never reports: its worktree is fixed at creation and the
   * shell reads it from the window record.
   */
  useEffect(() => {
    if (chromeless) return;
    // The *granted* worktree, not the selected one: a drop routed at a window
    // that has asked for a worktree but not been given it would put tabs into a
    // set of panes it is about to stop showing.
    void desktopWindow?.showsWorktree?.(shownId).catch(() => {});
  }, [chromeless, shownId]);

  /**
   * Take a worktree, or the next free one — the single path by which this client
   * comes to be showing anything.
   *
   * Shared by boot and by every reconnect, and that sharing is the point. It was
   * two code paths, and the second one was wrong in two ways at once: it refused
   * on one worktree without ever hunting, and it set `claimBlocked` — which
   * replaces the whole workspace, rail included — for a single refusal. Waking a
   * laptop whose worktree somebody had opened in a browser meanwhile left the
   * window with no way back except ⌘K.
   *
   * Held in a ref because the socket's handlers are registered once, at boot, and
   * would otherwise close over the first render's state forever.
   */
  const acquireRef = useRef<(preferred: Worktree) => Promise<void>>(async () => {});
  acquireRef.current = (preferred: Worktree) => {
    const gen = ++acquireGenRef.current;
    return acquireWorktree(preferred, {
      // Counted around the claim itself rather than around the whole acquire, so
      // the window between a granted claim and the next question is not counted as
      // one — see `claimsInFlight`.
      claim: async (id, focusHolder) => {
        claimsInFlight.current++;
        try {
          return await channel.claim(id, focusHolder);
        } finally {
          claimsInFlight.current--;
        }
      },
      release: (id, seq) => channel.release(id, seq),
      // **Openable ones only.** This was the repo's whole list, so a hunt could
      // land on a worktree in the trash or one this window had just confirmed the
      // removal of — opening panes, terminals and a browser rooted at a directory
      // on its way off the disk. The rail already refuses to select those on a
      // click; the hunt reaches the same rows without one.
      candidates: () => openableWorktrees(worktreesRef.current, isDeleting),
      show: (target) => {
        grantedRef.current = true;
        setActiveRepoRoot(target.repo_root);
        setActiveWtKey(String(target.id));
        setShownId(target.id);
        setClaimBlocked(false);
      },
      blocked: () => setClaimBlocked(true),
      // Refused, so this client is not showing it — and `preferred` is the
      // worktree it *was* showing whenever a reconnect asks for it back.
      notGranted: (id) => setShownId((cur) => (cur === id ? null : cur)),
      // **A generation, not a condition about the current state.** The first
      // attempt asked "is anything shown yet", and that was wrong in a way worth
      // recording: nothing nulls `shownId` when the *selection* changes without a
      // claim — switching repo, importing one, creating a worktree — so the
      // acquire for the new selection cancelled itself before it started, the
      // worktree was never claimed, and the workspace rendered empty until the
      // user clicked a rail row. "Has something newer started" is the question,
      // and only a counter answers it without depending on what that newer thing
      // has managed to do yet.
      live: () => acquireGenRef.current === gen,
    });
  };

  /**
   * Claim the worktree this window resolved to on its own, without a click.
   *
   * `selectWorktree` covers the rail; this covers boot, a restored `?wt=`, and
   * the fallback that lands on the first repo — all of which put a worktree on
   * screen without anyone choosing it.
   *
   * **Keyed on the selection, never on the worktree list.** `worktrees` gets a
   * new identity on every 5s poll, so having it in the deps re-claimed the same
   * worktree every five seconds for the life of the window — invisible while a
   * claim was synchronous, and not once a claim could be *superseded*, because
   * the poll's claim then overtook a rail click that was waiting on a holder.
   */
  useEffect(() => {
    if (chromeless || !worktree) return;
    // Already granted, so there is nothing to ask for — this effect re-runs on
    // every selection change, including the one `selectWorktree` has just been
    // granted, and asking again costs a second round trip plus a second yield
    // ask to anyone still listing that worktree. A *skip*, not a cancel: it
    // cannot reproduce the self-cancelling predicate this replaced, because it
    // never starts an acquire that then refuses to act.
    if (shownRef.current === worktree.id) return;
    // No cleanup: a selection change re-runs this effect, and starting a new
    // acquire is itself what cancels the old one.
    void acquireRef.current(worktree);
  }, [chromeless, worktree?.id]);

  // Cleared as soon as this window is showing something it owns.
  useEffect(() => {
    if (claimBlocked) setClaimBlocked(false);
  }, [worktree?.id]);

  /**
   * Worktrees no other client is showing — what this window could take.
   *
   * Derived from the claims table rather than asked for: the point is to notice
   * when the answer *changes*, and `freeKey` is what an effect can depend on
   * without re-running on every 5s poll's new object identities.
   */
  const freeWorktrees = openableWorktrees(worktrees, isDeleting, elsewhere);
  const freeKey = worktreeSetKey(freeWorktrees);

  /**
   * Try again when a worktree this window could have appears.
   *
   * **`claimBlocked` was latched.** It is set by a hunt that ran out of
   * candidates, and until this it was cleared only by the selection changing —
   * so a window that opened into a repo whose every worktree was taken sat on
   * the empty state through the other client closing, through a `veld worktree`
   * created in a terminal, and through the row being handed back to it by a
   * yield. The state it reported had stopped being true and nothing was going to
   * look again.
   *
   * Keyed on the free set, which is what makes this terminate: a retry that is
   * refused leaves the set as it was, so this does not re-run until something
   * genuinely changes. A grant clears `claimBlocked` through `show`.
   *
   * **Never while a claim of this window's is unanswered.** The daemon broadcasts
   * the claims table when it *records* a claim, up to `YIELD_ACK` before it
   * answers — so a rail click from this state changes `freeKey` first, and firing
   * here would supersede the click that caused it. The cost of skipping is a
   * retry not taken until the next change, and the claim in flight is already
   * asking the question this effect would ask.
   */
  useEffect(() => {
    if (chromeless || !claimBlocked || claimsInFlight.current > 0) return;
    const target = freeWorktrees[0];
    if (!target) return;
    void acquireRef.current(target);
  }, [chromeless, claimBlocked, freeKey]);

  // Accepts an updater as well as a value: two panes can report a change in the
  // same commit (two browser panes both finishing a navigation), and a value
  // computed from the render's `layout` would silently discard the other write.
  // Kept current on every render, like `dialogRef`. Assigned during render rather
  // than in an effect so a frame arriving between a render and its effects still reads
  // the layouts that render was built from.
  layoutsRef.current = layouts;

  const setLayout = useCallback(
    (next: PaneLayoutUpdate) => {
      if (!worktree) return;
      setLayouts((prev) => {
        const current = prev[worktree.id];
        const resolved = typeof next === "function" ? current && next(current) : next;
        if (!resolved || resolved === current) return prev;
        return { ...prev, [worktree.id]: resolved };
      });
    },
    [worktree?.id],
  );

  // A `target=_blank` inside a browser pane. It becomes a tab in the *same*
  // dock, carrying the pane's profile so the popup keeps the session it was
  // opened from — the shell denies the native window and defers the placement
  // here, because the layout is the only thing that knows where the pane is.
  // Keyed by the view id rather than by the selected worktree: a pane in a
  // worktree the user has since switched away from is still live.
  useEffect(
    () =>
      onBrowserOpenRequest(({ viewId, url, profile }) => {
        setLayouts((prev) => {
          for (const [key, l] of Object.entries(prev)) {
            const dock = dockOf(l, viewId);
            if (dock === null) continue;
            return { ...prev, [Number(key)]: addTab(l, dock, browserTab({ url, profile })) };
          }
          return prev;
        });
      }),
    [],
  );

  // A URL a terminal produced — clicked in its output, or handed to `$BROWSER` by
  // something running in it. It becomes a tab in the dock holding that terminal,
  // for the same reason a `target=_blank` does above: the layout is the only thing
  // that knows where the pane is, and the terminal's session id *is* its tab id.
  //
  // Keyed off the layouts rather than the selected worktree, so a shell in a
  // worktree the user has since switched away from still opens its page in the
  // right place. A session this window does not hold is not ours to answer — the
  // daemon sends the frame only to the socket that is attached, so that case means
  // the tab was released between the request and the frame.
  useEffect(
    () =>
      onTerminalOpenUrl(({ sessionId, url }) => {
        // Decided from the ref **before** touching state, not from a flag set inside
        // the updater. React 19 runs an updater eagerly only when the fiber has no
        // pending work (`dispatchSetStateInternal`); with any update already queued in
        // the same tick — routine here, since terminal status and toasts drive this
        // component — the updater runs during a later render, so the flag would still
        // be false and the URL would open in a pane *and* externally. A double open is
        // the mirror of the dropped-URL defect this fallback exists for.
        const owner = Object.entries(layoutsRef.current).find(
          ([, l]) => dockOf(l, sessionId) !== null,
        );
        if (!owner) {
          // The daemon has already told the caller "this is opening in a pane", so
          // nobody downstream is going to fall back — a tab released between the
          // request and the frame must not become a URL that silently went nowhere.
          // Reported as well as opened: this runs from a socket frame, with no user
          // activation, so a browser tab may block the popup and the toast is then the
          // only way the URL is recoverable.
          notifyRedirect(`Opened ${urlLabel(url)} outside Veld — its terminal pane is gone`);
          openExternally(url);
          return;
        }
        // A URL the daemon accepted that this build's own parser rejects would become a
        // tab with no page and no error (`browserTab` drops it), so it goes out instead.
        if (!normalizeBrowserUrl(url)) {
          notifyRedirect(`Opened ${urlLabel(url)} outside Veld — it is not a page a pane can show`);
          openExternally(url);
          return;
        }
        const [key, layout] = owner;
        const dock = dockOf(layout, sessionId)!;
        setLayouts((prev) => {
          const current = prev[Number(key)];
          // Deliberately **not** re-checking that the dock still holds the terminal.
          // The ref was read a tick earlier, so it could have gone — but a bail here
          // returns `prev` and the URL is lost, which is the very defect this handler's
          // fallback exists to prevent, reintroduced in a narrower window. An updater
          // must stay pure, so it cannot open the URL itself; adding the tab to the dock
          // the terminal was in is both harmless and what the user expects. Only a
          // worktree that has vanished entirely has nowhere to put it.
          if (!current) return prev;
          return { ...prev, [Number(key)]: addTab(current, dock, browserTab({ url })) };
        });
      }),
    [],
  );

  // A focused native view swallows every keystroke, so the shell forwards the
  // palette accelerator back to us (it also moves focus to the page, or the
  // palette would open with the keyboard still pointed at the view). The
  // per-pane "find" accelerator is not handled here — `BrowserPane` itself
  // subscribes and filters on its own `viewId`, since only one pane's find bar
  // should open.
  useEffect(
    () =>
      onBrowserAccelerator(({ accelerator }) => {
        if (accelerator === "palette") openPaletteRef.current();
        // A focused native browser pane swallows every keystroke, so these arrive
        // here instead of through the window's own key handler. Same chords,
        // same meaning — see `browserViews.js`.
        else if (accelerator === "project:toggle") goToPreviousProject();
        else if (accelerator === "tab:next") stepTab(1);
        else if (accelerator === "tab:previous") stepTab(-1);
        else if (accelerator.startsWith("project:")) {
          goToProject(Number(accelerator.slice("project:".length)));
        }
      }),
    [],
  );

  // A shell set its own tab title (OSC 0/2). The host has already gated it on
  // the pane allowing renaming (a plain terminal always may; a config pane only
  // with its flag), so this is a pure write: adopt the title onto the tab the
  // session lives in. Keyed off the layouts so a shell in a worktree the user
  // has switched away from still renames its own tab.
  useEffect(
    () =>
      onTerminalTitleChange(({ sessionId, title }) => {
        setLayouts((prev) => {
          for (const [key, l] of Object.entries(prev)) {
            if (dockOf(l, sessionId) === null) continue;
            const idx = Number(key);
            const next = updateTab(prev[idx], sessionId, { termTitle: title });
            return next === prev[idx] ? prev : { ...prev, [idx]: next };
          }
          return prev;
        });
      }),
    [],
  );

  // A process asked to be noticed (OSC 9). This is the terminal ergonomics
  // surface, not a coding-agent dashboard: the notification names the worktree
  // and pane so it is navigable across several open worktrees, and clicking it
  // focuses the pane. Stateless by design — there is no inbox, nothing is
  // persisted, and the daemon never sees it.
  //
  // Two surfaces, for the two windows it has to reach:
  //  - an in-app toast with a visible "click to focus" hint, click → the pane
  //  - an OS banner — always, because OSC 9 is an explicit "notify me" and the
  //    banner is the half that reaches you across windows and tabs
  //
  // Focus a terminal pane: select its worktree (which raises the window in the
  // desktop app) and activate its tab. Shared by the toast, the browser banner,
  // and the native-notification click below.
  const focusPane = async (wtId: number, sessionId: string) => {
    // **Every project's worktrees, not the selected project's.** An agent hook is
    // relayed to every client whatever it is showing, so the pane a notification
    // names is routinely in a project this window does not have selected — and
    // `selectWorktree` already moves the selection to `w.repo_root`, so switching
    // project falls out of finding the row. Looking it up in the selected project's
    // list is what made a cross-project notification click do nothing.
    const worktree = allWorktreesRef.current.find((w) => w.id === wtId);
    // **Nothing to claim when this window already holds it.** `channel.claim`
    // answers `{ok: false, reason: "offline"}` while the socket is down — for *any*
    // worktree, including the one on screen — so routing an already-granted pane
    // through a claim turned a notification click into a "not connected" notice and
    // no tab change. The pane is local; focusing it needs no round trip.
    //
    // Otherwise: **awaited, and a refusal stops here.** A refused claim means the
    // holder's window has been raised and this one is not showing that worktree, so
    // arming the pane request below would queue it against a selection this client
    // never gets, to fire on some later visit.
    // **Granted AND selected**, not just granted. `shownId` is the daemon's answer
    // and the selection is this window's; they diverge for as long as an acquire is
    // in flight, and after a ⌘1…⌘9 switch (which moves the selection with no claim
    // of its own) `shownId` still names the *previous* project's worktree. Testing
    // only `shownRef` therefore skipped the claim for a worktree this window had
    // already moved away from, found its stale layout still in `layoutsRef`, and
    // activated a tab nobody could see — the very symptom the skip was added to
    // remove, arriving from the other direction.
    const alreadyHere = shownRef.current === wtId && worktreeRef.current?.id === wtId;
    if (worktree && !alreadyHere && !(await selectWorktree(worktree))) {
      return;
    }
    // The layout is normally already here — the worktree was on screen, or its
    // panes were fetched on an earlier visit — and then this is the whole of it.
    if (layoutsRef.current[wtId]) {
      setLayouts((prev) => {
        const cur = prev[wtId];
        if (!cur) return prev;
        const next = activateTab(cur, sessionId);
        return next === cur ? prev : { ...prev, [wtId]: next };
      });
      return;
    }
    // **It is not, whenever the worktree was not already being shown here**, which
    // is now the ordinary case rather than an edge: a worktree in another project
    // has never had its panes read by this window, and they are fetched from the
    // daemon only once the claim is granted (see the seeding effect keyed on
    // `shownId`). The `setLayouts` above ran against an absent layout and dropped
    // the tab activation on the floor, so a notification click landed on the
    // worktree and then on whichever pane happened to be active.
    //
    // Recorded as a *value* rather than fixed by ordering: nothing here can wait
    // for a fetch that has not been issued, and the request stays true until it is
    // either satisfied or provably unsatisfiable. Same shape as `diagnoseFor`.
    setPendingFocusPane({ worktreeId: wtId, sessionId });
  };

  /**
   * A pane to activate as soon as its worktree's panes arrive.
   *
   * Set by `focusPane` when the layout is not here yet; cleared by the effect below
   * on the first render that can act on it — or that can prove it never will.
   */
  const [pendingFocusPane, setPendingFocusPane] = useState<{
    worktreeId: number;
    sessionId: string;
  } | null>(null);
  useEffect(() => {
    const req = pendingFocusPane;
    if (!req) return;
    // **Keyed on the granted claim, not the selection** — the same distinction the
    // seeding effect makes, and for the same reason: the selection can point at a
    // worktree this client has been refused, and a layout would then never arrive.
    // A yield between the click and the fetch lands here too.
    if (shownId !== req.worktreeId) {
      setPendingFocusPane(null);
      return;
    }
    const l = layouts[req.worktreeId];
    // Still being fetched. Not cleared: this effect re-runs when it lands.
    if (!l) return;
    setPendingFocusPane(null);
    const next = activateTab(l, req.sessionId);
    // The pane may have gone (closed in the window that had it) between the
    // notification and the layout arriving; `activateTab` then returns the layout
    // unchanged and there is nothing to focus.
    if (next === l) return;
    setLayouts((prev) =>
      prev[req.worktreeId] === l ? { ...prev, [req.worktreeId]: next } : prev,
    );
  }, [pendingFocusPane, shownId, layouts]);

  // ---------------------------------------------------------------------------
  // The worktree inbox: read-on-focus
  // ---------------------------------------------------------------------------

  // Re-render the rail when an unseen event lands or is read.
  useInbox();
  const activity = activityPrefs(settings ?? {});
  const focus = focusPrefs(settings ?? {});
  // The project column's visibility. Off by default — see the setting's docstring.
  const showProjectColumn = showProjectColumnPref(settings ?? {});

  /**
   * Turn an unseen event into a system notification.
   *
   * Three rules, and each of them is the difference between a useful banner and the
   * reason someone turns notifications off:
   *
   *  - **The event's own row must be ticked** (`activity.notify*`). Four rows rather
   *    than one switch, because "a command finished" and "an agent is waiting for you"
   *    are not the same event; `notifyKey` is the single place that maps one to the other.
   *  - **One channel, chosen by focus, never both.** A focused window gets the in-app
   *    toast; an unfocused one gets the OS banner, which is the only thing that reaches
   *    across windows and applications. An earlier version returned early when the window
   *    was focused, which left the toast branch below unreachable — so a focused window got
   *    nothing at all for an event in a pane it was not watching.
   *  - **The store fires once per event.** `onEvent`, not `subscribe`: a render may run
   *    any number of times for one event, and a banner may not. The store also never
   *    fires for the pane the user is watching, for a read, or for a retraction.
   *
   * Read through a ref so the effect does not re-subscribe on every settings change —
   * re-subscribing is harmless but it would drop and re-add a listener on each keystroke
   * elsewhere in the dialog.
   */
  const notifyPrefsRef = useRef(activity.notify);
  notifyPrefsRef.current = activity.notify;
  // Read through the same ref pattern as `notifyPrefsRef`, and for the same
  // reason. Focus mode only ever gates *this* channel — the toast/banner pair
  // below — never `notifyError`/`notifyDone`/`notifyRedirect`, which are
  // feedback for something the user just clicked themselves.
  const focusPrefsRef = useRef(focus);
  focusPrefsRef.current = focus;
  useEffect(
    () =>
      inbox.onEvent(({ sessionId, worktreeId, unseen }) => {
        if (!notifyPrefsRef.current[notifyKey(unseen)]) return;
        // Every project's, for the reason `focusPane` reads the same list.
        const wt = allWorktreesRef.current.find((w) => w.id === worktreeId);
        // **The project's name too, when the event is not from the one on screen.**
        // Worktree markers and branch names repeat across repos by design — the
        // assigner probes per repo (`markers_may_repeat_across_repos`) and two
        // projects both checked out on `main` is the default case — so "main —
        // waiting for you" names nothing a reader can act on once more than one
        // project is in play. Omitted for the selected project, where it would be on
        // every banner and say nothing.
        const project =
          wt && wt.repo_root !== activeRepoRootRef.current
            ? (reposRef.current.find((r) => r.root === wt.repo_root)?.name ?? "")
            : "";
        const label = wt
          ? project
            ? `${project} · ${worktreeLabel(wt)}`
            : worktreeLabel(wt)
          : "Veld";
        // The pane's name, when this window has that worktree's layout. It may not:
        // an agent hook is relayed to every client, including ones showing something
        // else, so the worktree alone has to be enough on its own.
        const layout = layoutsRef.current[worktreeId];
        const tab = layout ? findTab(layout, sessionId) : null;
        // `paneTabBaseLabel`, never `paneTabLabel` — #272's fix, and shell integration
        // makes it matter more rather than less. The pane's *displayed* label can be the
        // title the process set for itself via OSC 0, and a shell's preexec hook writes
        // the running command there: a banner reading "· sleep 5 && printf '\033]9;…'"
        // names the noise instead of the pane. A notification is read out of the context
        // that would have explained it, so it gets the pane's own name.
        const heading =
          tab && layout
            ? `${label} · ${paneTabBaseLabel(layout, tab)}`
            : label;
        // `void`: focusing a pane is awaited internally (the claim, then the panes
        // arriving) and nothing here has anything to do with the outcome — a
        // refusal has already raised the window that does have the worktree.
        const click = () => void focusPane(worktreeId, sessionId);
        // **One channel, chosen by where the user is.** A focused window gets the toast
        // — right weight for something you can already see — and an unfocused one gets
        // the OS banner, which is the only thing that reaches across windows and apps.
        // Never both: the OSC 9 path used to fire a toast unconditionally *and* a banner
        // when away, so being elsewhere meant two notifications for one event.
        const fp = focusPrefsRef.current;
        if (document.hasFocus()) {
          // Discarded, not queued: collecting these for a summary popup was
          // considered and dropped — the rail's own glyph is already the
          // record of "something happened here", and a second one on top is
          // more likely to be ignored than read.
          if (focusSuppresses(fp, FOCUS_SUPPRESS_TOASTS)) return;
          notifyTerminal({ title: heading, message: unseen.detail, onClick: click });
          return;
        }
        if (focusSuppresses(fp, FOCUS_SUPPRESS_OS_NOTIFICATIONS)) return;
        showSystemNotification({
          // The worktree first: with several open, "Command failed" alone does not say
          // where, and the banner is read out of the context that would have told you.
          title: heading,
          body: unseen.detail,
          // Echoed so Veld Desktop's main process can route the click back here — the
          // native banner is owned there precisely so a click can raise a backgrounded
          // window, which a page cannot do for itself.
          worktreeId,
          sessionId,
          onClick: click,
        });
      }),
    [],
  );

  /**
   * Which pane the user is actually looking at.
   *
   * "Looking at" is three conditions, and dropping any one of them makes the badge
   * wrong in a way that is hard to notice:
   *
   *  - **This window is focused.** A pane visible behind another app is not being
   *    watched, and an event there is exactly the news the inbox exists to keep.
   *  - **This window is the one showing that worktree** (`shownId`, the granted claim —
   *    not the selection, which a refused claim leaves pointing somewhere this window
   *    does not render).
   *  - **It is the active tab of the focused dock.** A split's other pane is on screen
   *    but not where the user is.
   *
   * `document.hasFocus()` is not reactive, so the window's focus/blur events are what
   * re-run this. Without them, blurring the window would leave the last watched pane
   * marked as watched and silently swallow every event in it.
   */
  const [windowFocused, setWindowFocused] = useState(() => document.hasFocus());
  useEffect(() => {
    const focus = () => setWindowFocused(true);
    const blur = () => setWindowFocused(false);
    window.addEventListener("focus", focus);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("focus", focus);
      window.removeEventListener("blur", blur);
    };
  }, []);
  useEffect(() => {
    const shownHere = worktree !== null && shownId === worktree.id;
    const active = shownHere && layout ? activeTab(layout, layout.focused) : null;
    inbox.setWatching(
      windowFocused && active?.kind === "terminal" ? active.id : null,
    );
  }, [windowFocused, worktree, shownId, layout]);

  /* The OSC 9 handler that used to live here is gone, and nothing replaced it.
     An OSC 9 already flows into the inbox (`panes/terminalHost.ts` reports it as a
     `notify` signal), which classifies it as `attention` from a `command` producer and
     sends it through the one path above — so it is now governed by the notification
     table (`activity.notifyNoticed`) like every other event, instead of being the one
     notification with no off switch. Keeping both would have double-notified for the
     same escape sequence. */

  // A native notification (Veld Desktop's main-process banner) was clicked. The
  // shell focused its window; focus the pane it names. Optional: an older shell
  // has no such channel.
  useEffect(
    () =>
      desktopApp?.onNotifyClick?.(({ worktreeId, sessionId }) => {
        void focusPane(worktreeId, sessionId);
      }) ?? (() => {}),
    [],
  );

  /**
   * Forget worktrees that no longer exist — everywhere they are recorded.
   *
   * In memory, so their terminals get collected below; in the layout store,
   * because omission is deliberately *not* deletion there (a client that yields
   * a worktree drops it from `layouts` while its panes go on existing); and in
   * the daemon's claim registry, so no client goes on being recorded as showing
   * a worktree that is gone. The daemon's foreign key collects the layout *row*
   * with the worktree, so that half needs nothing from here; the shell's own
   * display map is cleared by `showsWorktree` when the selection moves off.
   *
   * They matter for the same reason: `worktrees.id` is a plain `INTEGER
   * PRIMARY KEY`, so SQLite reuses the highest free rowid and the *next* worktree
   * created can arrive wearing a deleted one's id — inheriting its panes, and
   * being greyed out in the rail as "open in another window".
   *
   * Derived from the poll rather than hooked to the delete dialogs, which also
   * covers a `git worktree remove` run in a terminal, and cannot race a claim: a
   * worktree only enters this window's `layouts` after this window selected it
   * from a list that had it. Skipped while the repo list is empty — a failed poll
   * must not read as "everything was deleted".
   */
  useEffect(() => {
    // Main windows only, like the other multi-window effects: a detached window
    // shows one dock of a worktree a main window owns, so global ownership and the
    // shared store are not its to edit. The shell refuses it either way.
    if (chromeless || repos.length === 0) return;
    const alive = new Set(repos.flatMap((r) => r.worktrees.map((w) => w.id)));
    const gone = Object.keys(layouts)
      .map(Number)
      .filter((id) => !alive.has(id));
    if (gone.length === 0) return;
    setLayouts((prev) =>
      Object.fromEntries(Object.entries(prev).filter(([id]) => alive.has(Number(id)))),
    );
    // The daemon's foreign key collects the layout row with the worktree, so
    // this is only about the claim registry and this client's cached versions.
    for (const id of gone) dropLayout(id);
    setShownId((cur) => (cur !== null && gone.includes(cur) ? null : cur));
    channel.forget(gone);
    // `layouts` is a dependency on purpose: the ids to forget come from it, and
    // reading them inside the updater would be too late — React runs that during
    // the next render, not here. Converges, because the run after this one finds
    // nothing left to forget.
  }, [chromeless, repos, layouts]);

  // ---- multi-window --------------------------------------------------------
  //
  // Two windows are two documents: separate module instances, separate
  // registries, separate layout slots. Nothing is shared between them except the
  // daemon, so a tab moving between windows is a *transfer* — see
  // `desktop/src/windows.js`. These three effects are this window's half of it.

  /**
   * Tabs handed back by a detached window that just closed.
   *
   * Parsed here rather than trusted: they have been out of the page, through the
   * main process and another renderer, so they go through the same gate a
   * restored layout does.
   */
  useEffect(() => {
    const shell = desktopWindow;
    if (!shell) return;
    const drain = async () => {
      const transfers = await shell.takeAdopted().catch(() => []);
      for (const { worktreeId, tabs } of transfers) {
        const parsed = parseTransferTabs(tabs);
        if (parsed.length === 0) continue;
        setLayouts((prev) => {
          const next = adoptTabs(prev[worktreeId], parsed);
          return next ? { ...prev, [worktreeId]: next } : prev;
        });
      }
    };
    // At mount **and** on the nudge. A detached window can close while this one
    // is still on the waiting page or mid-reload, in which case the nudge lands
    // before this listener exists — the queue in the main process is what makes
    // that survivable, and draining it here is the half that collects it.
    void drain();
    return shell.onAdopt(() => void drain());
  }, []);

  /**
   * Keep the shell's copy of what this window holds current.
   *
   * Only a detached window has anything to hand back, and only it can be closed
   * with tabs still in it that belong somewhere else. Pushed on every layout
   * change because `close` is not a moment a renderer can be asked anything —
   * the answer has to already be in the main process when it fires.
   */
  useEffect(() => {
    if (!chromeless || !desktopWindow || !worktree) return;
    // Only while the resolved worktree is still the one this window was opened
    // for. When it stops existing, `worktree` falls back to another one in the
    // same commit — and this effect is declared before the close effect below,
    // so it would run first and overwrite the last good snapshot with an empty
    // one for the *fallback* worktree. The window then closed having handed back
    // nothing, which is the one thing closing a detached window must never do.
    if (activeWtKey !== "" && String(worktree.id) !== activeWtKey) return;
    void desktopWindow
      .snapshot({ worktreeId: worktree.id, tabs: layout ? allTabs(layout) : [] })
      .catch(() => {
        // The window can still be used; only the hand-back is lost, and the
        // shells it names outlive it either way under the detach grace.
      });
  }, [chromeless, worktree?.id, layout, activeWtKey]);

  /**
   * A detached window's title bar and its lifetime.
   *
   * It has no top bar to say what it holds, so the OS title bar does — and it
   * has no rail, no `+` outside its docks and no empty state worth showing, so a
   * detached window whose last tab was closed closes itself. Guarded on having
   * held something first: the layout is empty for the render or two before the
   * repo list resolves, and closing there would make a detached window flash
   * open and vanish.
   */
  const heldTabs = useRef(false);
  useEffect(() => {
    if (!chromeless || !desktopWindow) return;
    // The worktree this window was opened for is gone (removed, or its repo
    // unregistered). `worktree` resolves by *falling back* — first main, then
    // the first of the first repo — which in a full window is right and here is
    // not: a bare dock has no rail and no top bar, so it would silently become a
    // dock on an unrelated worktree, still wearing the old title, with its own
    // tabs already pruned along with their layout. Close instead.
    if (repoList && activeWtKey !== "" && String(worktree?.id ?? "") !== activeWtKey) {
      // Hand back what this window actually holds *first*. `layout` above is
      // the fallback worktree's, not ours — a restored detached window's real
      // tabs are keyed under the worktree it was opened for, which is the one
      // that just stopped resolving. Without this the window closed having
      // reported nothing, and its shells were left to the detach grace instead
      // of being reaped with the worktree like every other window's are.
      const ownId = Number(activeWtKey);
      const own = Number.isSafeInteger(ownId) ? layouts[ownId] : undefined;
      const tabs = own ? allTabs(own) : [];
      if (tabs.length > 0) {
        void desktopWindow.snapshot({ worktreeId: ownId, tabs }).catch(() => {});
      }
      void desktopWindow.close().catch(() => {});
      return;
    }
    if (layout && allTabs(layout).length > 0) {
      heldTabs.current = true;
      const active = activeTab(layout, layout.focused);
      const label = active ? paneTabLabel(layout, active) : "Veld";
      const title = worktree ? `${label} — ${worktreeLabel(worktree)}` : label;
      void desktopWindow.setTitle(title).catch(() => {});
      return;
    }
    if (heldTabs.current) void desktopWindow.close().catch(() => {});
    // `display_name` alongside `alias`: the title renders `worktreeLabel`, so a
    // rename that only touches the label must still retitle the window.
  }, [
    chromeless,
    layout,
    layouts,
    worktree?.id,
    worktree?.alias,
    worktree?.display_name,
    repoList,
    activeWtKey,
  ]);

  // Terminals live outside React (see panes/terminalHost.ts), so nothing
  // unmounts them. The layouts are the whole record of which should exist;
  // anything else is a shell nobody can see, still holding one of the
  // daemon's session slots. Disposal also tells the daemon to hang the shell
  // up, which closing the socket deliberately does not.
  useEffect(() => {
    const terminals = Object.values(layouts).flatMap(terminalIds);
    pruneTerminals(terminals);
    // Same contract for browser panes: a `WebContentsView` left behind is a
    // renderer process with nothing to paint into.
    pruneBrowsers(Object.values(layouts).flatMap(browserIds));
    // And the same for the inbox, which `pruneTerminals` cannot reach: it only walks
    // *live sessions*, and after a reload the inbox is restored from storage and can name
    // panes that were closed while the page was away. A restored event for a pane that no
    // longer exists is one the user could never read by looking, which is the poisoned
    // badge the design set out to avoid.
    //
    // **Scoped to the worktrees this window actually has a layout for.** A main window
    // gets none from storage (`readLayouts` is explicit about it) and fetches them from
    // the daemon one worktree at a time, so this effect's first run after a reload sees
    // `layouts === {}`. Pruning against that emptied the whole restored inbox — the guard
    // deleting the thing it guards.
    inbox.retain(terminals, Object.keys(layouts).map(Number));
  }, [layouts]);

  // ---- browser sessions ---------------------------------------------------
  //
  // An explicit set per worktree, persisted. Deriving it from which slots panes
  // occupy was the first attempt and it inverted the feature: moving a pane onto
  // a new session vacated its old slot, so adding one appeared to delete the
  // previous. See `SESSIONS_STORAGE_KEY` in panes/model.ts for why localStorage
  // is the right home for this and not the daemon.
  const [sessionsRaw, setSessionsRaw] = usePersisted(SESSIONS_STORAGE_KEY, "{}");
  const sessionSets = useMemo(() => parseSessionSets(sessionsRaw), [sessionsRaw]);
  const sessions = useMemo(
    () => (worktree ? sessionSetFor(sessionSets, worktree.id, layout) : []),
    [sessionSets, worktree?.id, layout],
  );
  /**
   * Mutate one worktree's session set against what is *currently* on disk.
   *
   * The whole operation reads through, not just the merge of the other keys:
   * `usePersisted` reads localStorage in a `useState` initialiser and re-reads only
   * when the key changes, and this key is a constant — so a second `/ide` tab holds
   * a snapshot from its own boot. Merging a stale *slot list* is the same bug one
   * level down: tab B adds `otter`, then stale tab A removes `wombat` and writes
   * its own list back, and `otter` is gone. Sharing across tabs is the reason
   * localStorage was chosen (see `SESSIONS_STORAGE_KEY`), so both the set being
   * edited and the sets beside it have to be as fresh as the write.
   *
   * `mutate` runs synchronously against that fresh list, which is what lets
   * `addSession` below capture the slot it picked.
   */
  const editSessions = (
    worktreeId: number,
    mutate: (current: BrowserProfile[]) => BrowserProfile[],
  ): void => {
    const onDisk = parseSessionSets(window.localStorage.getItem(SESSIONS_STORAGE_KEY));
    const next = mutate(sessionSetFor(onDisk, worktreeId, layout));
    setSessionsRaw(serializeSessionSets({ ...onDisk, [worktreeId]: next }));
  };

  // Whether *anything* can be added, for the menu's disabled state. The slot that
  // actually gets used is chosen inside `editSessions` from the on-disk set, since
  // another tab may have taken this one in the meantime.
  const nextSession = worktree ? nextFreeProfile(new Set(sessions)) : null;
  const addSession = (tabId: string) => {
    if (!worktree || !layout) return;
    let chosen: BrowserProfile | null = null;
    editSessions(worktree.id, (current) => {
      // Taken from the slots this worktree does not already list — not from
      // page-wide occupancy, so two worktrees can each hold the same slot.
      chosen = nextFreeProfile(new Set(current));
      return chosen ? [...current, chosen] : current;
    });
    // Adding is only ever worth doing to put this pane on it.
    if (chosen) setLayout((prev) => updateTab(prev, tabId, { profile: chosen! }));
  };
  // Removing a session returns its panes to the default one rather than being
  // refused: the session you are looking at was otherwise the single one you
  // could never remove. Only this worktree's panes are touched, because the sets
  // are per worktree — another worktree still lists (and holds) its own.
  const removeSession = (profile: BrowserProfile) => {
    if (!worktree || profile === "default") return;
    editSessions(worktree.id, (current) => current.filter((p) => p !== profile));
    setLayout((prev) =>
      allTabs(prev)
        .filter((t) => t.kind === "browser" && (t.profile ?? "default") === profile)
        .reduce((acc, t) => updateTab(acc, t.id, { profile: "default" }), prev),
    );
  };

  // The top bar's globe: a browser pane with nothing in it, which is where the
  // run's URLs live now (`panes/PlaceList.tsx`). An existing blank pane is already
  // showing exactly that, so it gets focused instead of stacking up another one —
  // and the *last* of them, so asking twice lands in the same place rather than
  // cycling.
  const showBlankBrowser = () => {
    if (!layout) return;
    // Updater form, like every other layout mutation here: a browser pane writes
    // the layout on its own schedule (`did-navigate` → `updateTab`), and that URL
    // is what a reload restores — so a value computed from this render would drop
    // a navigation that landed in the same commit.
    setLayout((prev) => {
      const blank = lastBlankBrowserId(prev);
      return blank ? activateTab(prev, blank) : addTabToFocused(prev, browserTab({}));
    });
  };

  // Rail expanded by default; the choice sticks across reloads/windows.
  //
  // Per window, like the width and for the same reason #199 settled for layouts:
  // two windows on two monitors want different rails. `usePersistedPerWindow`
  // writes both the slot-scoped and the unscoped key and reads the unscoped one as
  // a fallback, so a window that has never been sized inherits whatever was last
  // chosen instead of snapping back to the default.
  const [railWideRaw, setRailWideRaw] = usePersistedPerWindow("veld.railWide");
  const railWide = railWideRaw !== "0";
  const setRailWide = (fn: (v: boolean) => boolean) =>
    setRailWideRaw(fn(railWide) ? "1" : "0");
  const [railWidthRaw, setRailWidthRaw] = usePersistedPerWindow("veld.railWidth");
  const railW = railWidth(railWidthRaw);

  /**
   * Move a worktree in the rail: assign its lane, then rewrite the order.
   *
   * Two writes, in that sequence, because they live in different places — the lane
   * is a column on the worktree row and the order is a full-list rewrite. The lane
   * goes first so that if the second call fails the worktree is at least in the
   * group the user dropped it into; the reverse would leave it ordered inside a
   * group it does not belong to.
   */
  const moveWorktreeTo = async (
    path: string,
    toLane: string,
    toIndex: number,
  ) => {
    if (!repo) return;
    const move = moveWorktree(railGroups(worktrees, lanes), path, toLane, toIndex);
    if (!move) return;
    const moved = worktrees.find((w) => w.path === path);
    try {
      if (moved && moved.lane !== move.lane) {
        await api.patchWorktree(moved.id, { lane: move.lane });
      }
      await api.reorderWorktrees(repo.root, move.order);
    } catch (e) {
      notifyError("Could not reorder the rail", e);
    }
    await refresh();
  };

  /**
   * Announce a failed deletion exactly once, across every window.
   *
   * The daemon cannot push, so the failure arrives on the 5s poll and would
   * otherwise toast on every poll forever. Two halves do the work: the row keeps
   * the reason visibly (that is the durable surface), and this announces it once.
   *
   * Acked in `localStorage`, not in a ref: the reason is stored on the row until
   * dismissed, so a page reload would re-announce a failure the user already read —
   * and people reload often. Keyed on path *and* message so a second, different
   * failure on the same worktree is announced again.
   *
   * Deliberately **not** slot-scoped through `selectionKeys`, unlike every other
   * persisted value in this file. Those are preferences, which belong to a window; a
   * removal that failed is a fact about a worktree, and three windows announcing the
   * same failure three times is noise rather than thoroughness. The row keeps the
   * reason visible in all of them regardless, which is the durable surface.
   */
  useEffect(() => {
    const failed = worktrees.filter((w) => w.trash_error);
    if (failed.length === 0) return;
    let acked: string[] = [];
    try {
      acked = JSON.parse(window.localStorage.getItem(TRASH_ACK_KEY) ?? "[]");
    } catch {
      acked = [];
    }
    const keys = failed.map((w) => `${w.path}\u0000${w.trash_error}`);
    const fresh = failed.filter((_, i) => !acked.includes(keys[i]));
    for (const w of fresh) {
      notifyError(
        `Could not delete ${worktreeLabel(w)}`,
        new Error(`${w.trash_error} — it is still in the rail.`),
      );
    }
    if (fresh.length > 0) {
      try {
        // Pruned to the keys still present, so the list cannot grow without
        // bound as worktrees come and go.
        window.localStorage.setItem(TRASH_ACK_KEY, JSON.stringify(keys));
      } catch {
        // Storage unavailable: the cost is a repeated toast, not a lost failure —
        // the reason stays on the row either way.
      }
    }
  }, [worktrees]);

  const dismissTrashError = async (w: Worktree) => {
    try {
      await api.dismissTrashError(w.id);
    } catch (e) {
      notifyError(`Could not dismiss the error on ${worktreeLabel(w)}`, e);
    }
    await refresh();
  };

  /** Enqueue the worker to delete a trashed worktree, optimistically moving it
   *  to the terminal "Deleting" lane (see the comment on the caller). */
  const enqueueDelete = async (w: Worktree) => {
    await api.deleteTrashedWorktree(w.id);
    // Move it to the terminal "Deleting" lane immediately: the daemon only
    // reports `deleting` once the removal is actually running, and the run
    // teardown in between can take a while. From the confirm, the row is
    // committed and must not read as recoverable trash.
    setDeletingIds((cur) => {
      const next = new Set(cur);
      next.add(w.id);
      return next;
    });
    notifyDone(`Deleting ${worktreeLabel(w)}`);
    await refresh();
  };

  const deleteTrashedWorktree = async (w: Worktree) => {
    // Proactively check for a dirty state before enqueueing. `git worktree
    // remove` refuses on uncommitted files, so enqueueing a dirty worktree
    // fails a moment later and the row comes back with a `trash_error` the user
    // has to chase down. If it is dirty, surface the files and ask how to
    // proceed instead — discard, or revert first.
    try {
      const status = await api.worktreeGitStatus(w.id);
      if (status.dirty) {
        setDialog({ kind: "confirm-delete", worktree: w, status });
        return;
      }
    } catch {
      // Status unavailable (git error, checkout gone): fall through to the
      // enqueue and let the worker surface any refusal on the row, as before.
    }
    try {
      await enqueueDelete(w);
    } catch (e) {
      notifyError(`Could not delete ${worktreeLabel(w)}`, e);
      await refresh();
    }
  };

  const emptyTrash = async () => {
    if (!repo) return;
    try {
      const { queued } = await api.emptyTrash(repo.root);
      // Same as a single confirm: every trashed row enters the Deleting lane now.
      setDeletingIds((cur) => {
        const next = new Set(cur);
        for (const w of worktrees) {
          if (w.trashed_at) next.add(w.id);
        }
        return next;
      });
      notifyDone(
        queued === 1 ? "Deleting 1 worktree" : `Deleting ${queued} worktrees`,
      );
    } catch (e) {
      notifyError("Could not empty the trash", e);
    }
    await refresh();
  };

  const restoreWorktree = async (w: Worktree) => {
    try {
      await api.restoreWorktree(w.id);
      notifyDone(`Restored ${worktreeLabel(w)}`);
    } catch (e) {
      // 404 is the expected failure — the worker got there first — and saying so is
      // better than a generic error for a race the design admits to. Anything else
      // (a 500, a dead daemon) must NOT claim the worktree was removed, because it
      // is still there and the user would stop looking for it.
      const gone = e instanceof Error && /not found|already removed/i.test(e.message);
      notifyError(
        gone
          ? `${worktreeLabel(w)} has already been deleted`
          : `Could not restore ${worktreeLabel(w)}`,
        e,
      );
    }
    await refresh();
  };

  /** Drop a rail row onto the trash: bin it (revertible), which is what dragging
   *  a worktree onto the trash intuitively means. Not a lane move — the trash is
   *  a state, not a destination that orders anything — so it goes straight to
   *  the bin endpoint rather than `onMove`. */
  const trashWorktree = async (path: string) => {
    const w = worktrees.find((x) => x.path === path);
    if (!w) return;
    // The main checkout is the repository itself and is never draggable in the
    // first place (the row gates on `!w.is_main`), so a drop can't reach here;
    // this is defence in depth so binning main can never be invoked silently.
    if (w.is_main) return;
    // A dirty worktree can't be deleted later without either discarding or
    // reverting its changes, so a drag-to-trash must not bin it silently —
    // surface the files and let the user choose, the same as the context-menu
    // "Remove worktree…". A clean worktree still bins immediately, as before.
    try {
      const status = await api.worktreeGitStatus(w.id);
      if (status.dirty) {
        setDialog({ kind: "trash", worktree: w });
        return;
      }
    } catch {
      // Status unavailable (git error, checkout gone): bin directly; any
      // refusal surfaces later on the row.
    }
    try {
      await api.deleteWorktree(w.id, false);
    } catch (e) {
      notifyError(`Could not move ${worktreeLabel(w)} to the trash`, e);
    }
    await refresh();
  };

  /** Move every detached checkout to the trash, in one go (revertible).
   *
   *  The Detached lane exists because detached checkouts are usually
   *  throwaways, so this is the action that matches the lane's point: clear them
   *  out without deleting each one by hand. Clean ones bin immediately; a dirty
   *  one (uncommitted changes) refuses, as `git worktree remove` does, and stays
   *  in the lane with the reason on its row. */
  const trashAllDetached = async () => {
    const detached = worktrees.filter((w) => isDetached(w) && !w.trashed_at);
    if (detached.length === 0) return;
    let trashed = 0;
    for (const w of detached) {
      // Never the main checkout: it cannot be detached in the first place (git
      // keeps a repo's main on a branch) and binning it would take the
      // repository with it.
      if (w.is_main) continue;
      try {
        await api.deleteWorktree(w.id, false);
        trashed += 1;
      } catch (e) {
        notifyError(`Could not move ${worktreeLabel(w)} to the trash`, e);
      }
    }
    if (trashed > 0) {
      notifyDone(
        trashed === 1 ? "Moved 1 detached worktree to the trash" : `Moved ${trashed} detached worktrees to the trash`,
      );
    }
    await refresh();
  };

  // Above both bars so the crossfade survives ModeSwitch remounting as it moves
  // between them — see the component's own note. Focus does *not* survive that
  // remount either, which is a real gap the `:focus-visible` rules make more
  // visible; it predates this change and is tracked as a follow-up rather than
  // fixed here, because restoring it means new state on the app's busiest render
  // path and this diff's review budget is spent.
  const modeSwitch = (
    <ModeSwitch mode={mode} onMode={setMode} />
  );

  /**
   * Everything ⌘K can reach. Built on demand (only while the palette is open)
   * rather than memoised: it closes over most of this component's state, and
   * a correct dependency list would be longer than the function.
   */
  const buildPaletteItems = (): PaletteItem[] => {
    const items: PaletteItem[] = [];

    for (const w of worktrees) {
      // Pending removals are omitted: ⌘K exists to *go* somewhere, and there is
      // nowhere to go in a checkout that is being deleted.
      if (w.trashed_at) continue;
      const wtStatus = worktreeStatus(runsForWorktree(envs, w));
      items.push({
        id: `wt:${w.id}`,
        group: "Worktrees",
        label: worktreeLabel(w),
        // The status rides the hint as *text* rather than as a dot beside the
        // marker — the palette has no run control to move the state onto, and the
        // dot was the same two-circles-read-as-one collision the rail had.
        //
        // Only `failed` and `recovering` are carried. That drops two states this
        // surface used to render — `running` (a pulsing green dot) and `partial`
        // (amber) — and both deletions are deliberate: ⌘K is how you *go*
        // somewhere, the rail is on screen while it is open, and a badge on every
        // started or transitioning worktree is noise around the two states worth
        // interrupting a search for. Naming `partial` explicitly because the same
        // argument is weaker for it: a transition is short-lived, so an omission
        // there costs a reader nothing they will not see resolve anyway.
        hint: PALETTE_STATUS[wtStatus]
          ? `${w.branch} · ${PALETTE_STATUS[wtStatus]}`
          : w.branch,
        // The alias joins the branch as an alternate haystack: the label is the
        // display name now, so a worktree named "Hello test" would otherwise be
        // unreachable by typing the `hello-test` its run and hostname use.
        alt: [w.branch, w.alias],
        mark: { emoji: w.emoji, marker_color: w.marker_color },
        run: () => void selectWorktree(w),
      });
    }

    if (worktree && canRunWorktreeNow(worktree)) {
      const w = worktree;
      const running = status !== "stopped";
      // Same guard as the rail and the context menu: an action already in
      // flight must not be offered again a second time.
      const busy = pendingFor(w) !== null;
      if (running && !busy) {
        items.push({
          id: "run:stop",
          group: "Run",
          label: `Stop ${worktreeLabel(w)}`,
          run: () => stopWorktree(w),
        });
        items.push({
          id: "run:restart",
          group: "Run",
          label: `Restart ${worktreeLabel(w)}`,
          run: () => restartWorktree(w),
        });
      } else if (!running && canStartWorktree(w)) {
        items.push({
          id: "run:start",
          group: "Run",
          label: `Start ${worktreeLabel(w)}`,
          run: () => startWorktree(w),
        });
      }
      // Offered whether or not anything is running, and not gated on `busy`:
      // reading or changing a machine value is not a run action and does not
      // conflict with one in flight. The right-click menu was the only way in,
      // which is not enough for something that can *block* a start.
      //
      // Omitted rather than disabled when the project asks for nothing — ⌘K is a
      // search, and a result that cannot be run is a worse answer than no result.
      // That differs from the top bar deliberately: a button in a fixed position
      // teaches that the feature exists, a search hit does not.
      if (w.machine_vars !== 0) {
        items.push({
          id: "run:config-vars",
          group: "Run",
          label: `Values for this machine (${worktreeLabel(w)})`,
          alt: ["vars", "machine", "override", "config set", "configurable"],
          run: () => setDialog({ kind: "config-vars", project: w.path }),
        });
      }
      // Reachable while a run is live, which "Start" is not — ▶ is a toggle. The
      // name is in the label because that is what the command will create.
      if (running && canStartAnother(w)) {
        items.push({
          id: "run:start-another",
          group: "Run",
          label: `Start another run in ${worktreeLabel(w)} (${anotherNameFor(w)})`,
          alt: ["second", "parallel", "new environment"],
          run: () => startWorktree(w, anotherNameFor(w)),
        });
      }
      for (const [name, url] of urls) {
        items.push({
          id: `url:${name}`,
          group: "Run",
          label: `Open ${name}`,
          hint: url,
          alt: [url],
          run: () => window.open(url, "_blank"),
        });
        if (layout) {
          items.push({
            id: `url:pane:${name}`,
            group: "Run",
            label: `Open ${name} in a pane`,
            hint: url,
            alt: [url, "browser", "preview"],
            run: () => setLayout(addTabToFocused(layout, browserTab({ url, title: name }))),
          });
        }
      }
      if (urls.length > 0) {
        items.push({
          id: "url:copy-all",
          group: "Run",
          label: "Copy all run URLs",
          run: () =>
            void navigator.clipboard.writeText(
              urls.map(([, u]) => u).join("\n"),
            ),
        });
      }
      // Sharing, from the keyboard. The same three actions the top bar's Sharing
      // surface offers, gated the same way — sharing needs a live *running* run,
      // which the daemon enforces anyway.
      if (diagRef && diagRun) {
        const ref = diagRef;
        const live = diagRun.status === "running";
        if (live && !runShares.peer) {
          items.push({
            id: "share:start",
            group: "Run",
            label: `Share ${worktreeLabel(w)} privately`,
            hint: "peer to peer — copies the join link",
            alt: ["share", "invite", "join", "peer", "private"],
            run: () =>
              shareAction("share", async () => {
                const r = await api.startShare(ref);
                // Not awaited into the share's own error path — see ShareControls:
                // the share is already live, so a refused clipboard write must not
                // report that sharing failed.
                // Not an error when it is refused: WebKit only allows a write in
                // the same task as the gesture, and this follows a round-trip. The
                // Sharing surface's Copy link button is the fallback.
                if (r?.join_url) void navigator.clipboard.writeText(r.join_url).catch(() => {});
              }),
          });
        }
        if (runShares.peer) {
          const share = runShares.peer;
          items.push({
            id: "share:stop",
            group: "Run",
            label: `Stop the private share of ${worktreeLabel(w)}`,
            run: () => shareAction("stop sharing", () => api.stopShare(share.id)),
          });
        }
        if (live && runShares.web.length === 0) {
          items.push({
            id: "share:web",
            group: "Run",
            label: `Share ${worktreeLabel(w)} to the web`,
            alt: ["public", "gateway", "tunnel"],
            run: () => shareAction("web share", () => api.startShare(ref, { web: true })),
          });
        }
        for (const web of runShares.web) {
          items.push({
            id: `share:web-stop:${web.id}`,
            group: "Run",
            label: `Stop the public web share of ${worktreeLabel(w)}`,
            hint: web.public_urls[0]?.public_url,
            run: () => shareAction("stop web share", () => api.stopShare(web.id)),
          });
        }
      }
    }

    if (repo) {
      items.push({
        id: "wt:new",
        group: "Worktree",
        label: "New worktree…",
        // Ungrouped: the palette has no lane in hand, and inventing one would
        // file a checkout somewhere the user never pointed at.
        run: () => setDialog({ kind: "new-worktree", lane: "" }),
      });
    }
    if (worktree) {
      const w = worktree;
      items.push({
        id: "wt:rename",
        group: "Worktree",
        label: `Rename ${worktreeLabel(w)}…`,
        run: () => setDialog({ kind: "rename", worktree: w }),
      });
      items.push({
        id: "wt:marker",
        group: "Worktree",
        label: `Change marker for ${worktreeLabel(w)}…`,
        hint: w.emoji,
        run: () => setDialog({ kind: "marker", worktree: w }),
      });
      items.push({
        id: "wt:copy-path",
        group: "Worktree",
        label: `Copy path of ${worktreeLabel(w)}`,
        hint: w.path,
        alt: [w.path],
        run: () => void navigator.clipboard.writeText(w.path),
      });
      if (!w.is_main) {
        items.push({
          id: "wt:remove",
          group: "Worktree",
          label: `Remove worktree ${worktreeLabel(w)}…`,
          run: () => setDialog({ kind: "trash", worktree: w }),
        });
      }
    }

    // **Every other project's worktrees are reachable from here.** The "Worktrees"
    // group above is the selected project's, which is the whole of what ⌘K could
    // reach before — so the one surface built for "go somewhere" could not go to the
    // other half of a two-project day, and the only route was switching project
    // first and searching again. They sit in "Projects" rather than in "Worktrees"
    // because `PaletteGroup` is a closed set whose order sorts the idle list: a group
    // per project would have to open that set, and the sorted idle list is what makes
    // an unfiltered ⌘K predictable.
    //
    // Each project's own "Switch to" entry leads its worktrees, so the idle list
    // reads as one block per project rather than as two interleaved lists.
    for (const r of repos) {
      if (r.root === repo?.root) continue;
      items.push({
        id: `repo:${r.root}`,
        group: "Projects",
        label: `Switch to ${r.name}`,
        hint: r.available ? r.root : "unavailable",
        alt: [r.root],
        run: () => {
          setActiveRepoRoot(r.root);
          setActiveWtKey("");
        },
      });
      for (const w of r.worktrees) {
        // Same omission as the "Worktrees" group: there is nowhere to go in a
        // checkout that is being deleted.
        if (w.trashed_at) continue;
        items.push({
          id: `wt:${w.id}`,
          group: "Projects",
          // Project-qualified, because that is the whole difference from the group
          // above: markers and branch names repeat across repos by design, so
          // "main" on its own names two rows in the same list.
          label: `${r.name} · ${worktreeLabel(w)}`,
          hint: w.branch,
          // The project name is already in the label; the branch and alias join it
          // as haystacks for the same reason they do above.
          alt: [w.branch, w.alias, r.name],
          mark: { emoji: w.emoji, marker_color: w.marker_color },
          // Cross-project selection needs nothing special: `selectWorktree` claims
          // the row and then moves the selection to `w.repo_root`.
          run: () => void selectWorktree(w),
        });
      }
    }
    // One entry per project, the selected one included: opening a second window is
    // how two projects are worked at once, and it is the action with no keyboard
    // route otherwise (the menu is the only other way in). Desktop only — a browser
    // tab has no shell to open a window with.
    if (desktopWindow) {
      for (const r of repos) {
        items.push({
          id: `repo:window:${r.root}`,
          group: "Projects",
          label: `Open ${r.name} in a new window`,
          alt: [r.root],
          run: () => void openProjectWindow(r.root),
        });
      }
    }
    items.push({
      id: "repo:import",
      group: "Projects",
      label: "Import repository…",
      run: () => setDialog({ kind: "import" }),
    });
    if (repo) {
      const r = repo;
      items.push({
        id: "repo:remove",
        group: "Projects",
        label: `Remove project ${r.name}…`,
        run: () => setDialog({ kind: "remove-repo", repo: r }),
      });
    }

    // ---- Panes -----------------------------------------------------------
    if (layout) {
      items.push({
        id: "pane:new-terminal",
        group: "Panes",
        label: "New terminal",
        alt: ["shell", "pty"],
        hint: worktree ? worktreeLabel(worktree) : undefined,
        run: () =>
          setLayout(
            addTabToFocused(layout, {
              id: newTabId(),
              kind: "terminal",
              title: "terminal",
            }),
          ),
      });
      for (const spec of worktree?.ide.panes ?? []) {
        if (!spec.available) continue;
        items.push({
          id: `pane:new-${spec.id}`,
          group: "Panes",
          label: `New ${spec.label} pane`,
          alt: [spec.id],
          hint:
            spec.description ??
            (worktree ? worktreeLabel(worktree) : undefined),
          run: () => setLayout(addTabToFocused(layout, configPaneTab(spec))),
        });
      }
      items.push({
        id: "pane:new-browser",
        group: "Panes",
        label: "New browser pane",
        alt: ["preview", "webview", "url"],
        run: () => setLayout(addTabToFocused(layout, browserTab({}))),
      });
      items.push({
        id: "pane:logs",
        group: "Panes",
        label: "Run logs in a pane",
        alt: ["logs", "output", "stderr", "diagnostics"],
        hint: diagRun?.name,
        run: () => setLayout(addTabToFocused(layout, diagTab("logs"))),
      });
      items.push({
        id: "pane:nodes",
        group: "Panes",
        label: "Node health in a pane",
        alt: ["nodes", "services", "health", "cpu", "memory", "diagnostics"],
        hint: diagRun?.name,
        run: () => setLayout(addTabToFocused(layout, diagTab("nodes"))),
      });
      const focused = activeTab(layout, layout.focused);
      if (focused) {
        items.push({
          id: "pane:close",
          group: "Panes",
          label: `Close the ${focused.title} pane`,
          run: () => setLayout(closeTab(layout, focused.id)),
        });
      }
      if (focused?.kind === "terminal") {
        items.push({
          id: "pane:restart-terminal",
          group: "Panes",
          label: "Restart this terminal",
          hint: "keeps the scrollback",
          run: () => restartTerminal(focused.id),
        });
      }
      if (focused?.kind === "browser") {
        items.push({
          id: "pane:reload-browser",
          group: "Panes",
          label: "Reload this page",
          hint: focused.url,
          run: () => reloadBrowser(focused.id),
        });
      }
      items.push({
        id: "pane:veld-links",
        group: "Panes",
        label: "Open the run's URLs in a pane",
        alt: ["urls", "services", "links"],
        run: showBlankBrowser,
      });
    }

    items.push({
      id: "view:mode",
      group: "View",
      label: "Switch to Runs",
      run: () => setMode("runs"),
    });
    items.push({
      id: "view:rail",
      group: "View",
      label: railWide ? "Collapse the worktree rail" : "Expand the worktree rail",
      run: () => setRailWide((v) => !v),
    });
    items.push({
      id: "view:theme",
      group: "View",
      label: "Cycle theme",
      hint: themePref,
      run: onCycleTheme,
    });
    return items;
  };

  // ---- render -------------------------------------------------------------

  /**
   * The end of the top bar — search, keep-awake, focus mode — built once and
   * threaded into **both** modes, the way `overflowMenu` already is.
   *
   * All three used to be IDE-only, on the reasoning that Runs mode's bar had no
   * search icon for them to sit beside. Keep-awake is what made that wrong
   * rather than merely inconsistent: a live share now arms it by itself, and
   * Runs mode is a screen on which sharing is started — so the one mode with no
   * keep-awake control was also a mode where the machine could be held awake
   * with nothing on screen saying so, and nothing to switch it off with. Sharing
   * that cluster is the fix; see `TopBarControls`.
   *
   * Search is handed a mode-aware opener because what the palette finds —
   * worktrees, panes, run actions — only exists in the IDE. Switching first
   * means every palette item behaves exactly as it always has.
   */
  /**
   * One owner for "open the command palette", shared by the top-bar button, ⌘K
   * and Veld Desktop's accelerator.
   *
   * The mode switch is the reason it has to be one: what the palette finds —
   * worktrees, panes, run actions — only exists in the IDE, so opening it over
   * Runs mode lists things whose selection changes nothing visible. Switching
   * first means every palette item behaves exactly as it always has, with no
   * per-item special case. Three call sites applying that rule independently is
   * how two of them end up not applying it.
   */
  const openPalette = () => {
    if (mode !== "ide") setMode("ide");
    setDialog({ kind: "search" });
  };
  openPaletteRef.current = openPalette;

  const topBarControls = (
    <TopBarControls
      settings={settings ?? {}}
      onSetting={(patch) => void saveSettings(patch)}
      onSearch={() => openPalette()}
    />
  );

  /** One owner for "flip the project column", shared by the top-bar button and
   *  ⌘B — two call sites writing the same setting is how they end up disagreeing
   *  about which way is on. Read through a ref by the key handler, which is
   *  registered once at boot. */
  const toggleProjectColumn = () => {
    void saveSettings({ "ui.showProjectColumn": !showProjectColumnRef.current });
  };
  const showProjectColumnRef = useRef(showProjectColumn);
  showProjectColumnRef.current = showProjectColumn;
  toggleProjectColumnRef.current = toggleProjectColumn;

  /**
   * Show or hide the project column.
   *
   * **In the bar, not only in Settings**, and that placement is the whole reason
   * the column can ship off by default: a surface nobody can find is the same
   * thing as a surface that does not exist. It sits between the mode switch and
   * the project selector because that is where the column it governs is — left
   * of everything, next to the thing it appears beside.
   *
   * `IconStack2`/`IconStack` rather than one glyph in two colours: a single icon
   * reused for both states of a toggle is indistinguishable at 14px, which is the
   * mistake `focus.enabled` shipped once and now avoids with its own pair.
   */
  const projectColumnButton = (
    <Tooltip
      label={
        showProjectColumn ? "Hide the project column (⌘B)" : "Show the project column (⌘B)"
      }
    >
      <ActionIcon
        size="md"
        // **Flat in both states, deliberately.** A filled/green "on" would be the
        // bar's loudest control reporting something the user can see for
        // themselves — the column is either beside them or it is not. `aria-pressed`
        // still carries the state, because a screen reader cannot look.
        variant="default"
        aria-label="Project column"
        aria-pressed={showProjectColumn}
        onClick={toggleProjectColumn}
      >
        {/* `IconStack2` in both states, at the maintainer's pick. The pair-of-glyphs
            rule this repo learned from `IconFocus2` is about a toggle whose two
            states are otherwise *indistinguishable*; here the filled/outline
            variant and `aria-pressed` carry the state, and swapping the glyph as
            well made the button look like two different controls. */}
        <IconStack2 size={14} />
      </ActionIcon>
    </Tooltip>
  );

  /**
   * The project's machine-overridable vars.
   *
   * Deliberately **not** folded into the ⋯ menu beside it. That dialog is
   * veld's own preferences — global, yours, the same whatever you have open.
   * These are values *this project declared* and this machine answers, so they
   * change with the selected worktree and are meaningless without one. The
   * sliders icon rather than a second settings glyph for the same reason: two would
   * read as two ways into one thing.
   *
   * **Disabled, not hidden, when the project asks for nothing.** A control that
   * vanishes teaches nobody it exists; one that is greyed out with a reason does.
   * `machine_vars === 0` is that case — and `null` (an unreadable config) is
   * deliberately *not*, because opening the dialog is how the reader finds out
   * why it cannot be read.
   *
   * The `<span>` is load-bearing: a disabled Mantine control has
   * `pointer-events: none`, so the tooltip explaining *why* it is disabled would
   * never open — the #205 trap, where a disabled button's tooltip is exactly the
   * thing the user needs and exactly the thing they cannot get. The wrapper
   * still receives the hover.
   */
  const configVarsNone = worktree?.machine_vars === 0;
  // Hidden when the project asks for nothing and `ui.hideDisabledActions` is on;
  // otherwise shown, greyed, with the tooltip explaining why. A small badge
  // appears when this machine has answered at least one of the vars.
  const configVarsButton =
    worktree &&
    canRunWorktreeNow(worktree) &&
    !(configVarsNone && hideDisabled) && (
      <Tooltip
        label={
          configVarsNone
            ? `Worktree “${worktreeLabel(worktree)}” doesn’t ask you for any values`
            : `Values for this machine — worktree “${worktreeLabel(worktree)}”`
        }
      >
        {/* The `<span>` is load-bearing even now the button can be hidden: when
            hide-disabled is off and the project asks for nothing, the disabled
            control must still show its *why* — a disabled Mantine control has
            `pointer-events: none`, so the wrapper is what lets the tooltip open
            (the #205 trap). `overridden` classes the badge below. */}
        <span className={`vars-btn${configVarsOverridden ? " overridden" : ""}`}>
          <ActionIcon
            size="md"
            variant="default"
            aria-label="Values for this machine"
            disabled={configVarsNone}
            onClick={() =>
              setDialog({ kind: "config-vars", project: worktree.path })
            }
          >
            <IconAdjustments size={14} />
          </ActionIcon>
          {configVarsOverridden && (
            <span className="vars-badge" aria-hidden="true" />
          )}
        </span>
      </Tooltip>
    );

  /**
   * The bar's overflow menu — the last control in both top bars.
   *
   * Everything here belongs to **Veld** rather than to a project or a run: the
   * theme, what's new, and settings. That split is the point of the restructure —
   * project actions moved into the project menu at the start of the bar, and these
   * three stopped being three separate icons competing with them for the same row.
   *
   * The unread-news dot moved here with the entry it belongs to. It is the only
   * reason this button ever asks for attention, and a menu nobody opens is a
   * channel nobody reads — which is why the dot is on the button and not only on
   * the item inside.
   *
   * Theme is a **submenu with the three states named**, replacing a one-button
   * cycle. A cycle makes you press a control up to three times to reach a value it
   * never showed you, and "system" in particular is impossible to *ask* for — you
   * arrive at it. This dropdown has no `max-height`, which is what lets that
   * submenu position itself; see `ProjectMenu` for the failure that rule comes from.
   */
  // No unread dot while the start screen is up. Zero projects means the whole window
  // is one instruction — "import something" — and a second thing on the bar asking to
  // be clicked is the only competition it has. The news keeps; the menu still carries
  // it the moment there is a project. (This gate lived on `TopBar`'s own `unreadNews`
  // before the bar was restructured, and moved here with the dot.)
  const unreadNews = repos.length === 0 ? 0 : promotions.unread;
  const overflowMenu = (
    <Menu position="bottom-end" width={220}>
      <Menu.Target>
        <ActionIcon
          size="md"
          variant="default"
          className="project-actions"
          aria-label={unreadNews > 0 ? `More – ${unreadNews} unread` : "More"}
          title={unreadNews > 0 ? `More – ${unreadNews} unread` : "More"}
        >
          <IconDots size={14} />
          {unreadNews > 0 && <span className="project-actions-dot" aria-hidden="true" />}
        </ActionIcon>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Sub>
          <Menu.Sub.Target>
            <Menu.Sub.Item
              leftSection={
                themePref === "auto" ? (
                  <IconDeviceDesktop size={14} />
                ) : themePref === "light" ? (
                  <IconSun size={14} />
                ) : (
                  <IconMoon size={14} />
                )
              }
            >
              <span className="project-menu-row">
                <span className="project-menu-name">Theme</span>
                <span className="project-menu-away">
                  {themePref === "auto" ? `system (${theme})` : themePref}
                </span>
              </span>
            </Menu.Sub.Item>
          </Menu.Sub.Target>
          <Menu.Sub.Dropdown>
            {THEME_CHOICES.map((choice) => (
              <Menu.Item
                key={choice.value}
                leftSection={<choice.icon size={14} />}
                // The chosen one is marked rather than disabled: a disabled row in a
                // list of three reads as unavailable, not as current.
                rightSection={themePref === choice.value ? <IconCheck size={13} /> : undefined}
                onClick={() => onSetTheme(choice.value)}
              >
                {choice.label}
              </Menu.Item>
            ))}
          </Menu.Sub.Dropdown>
        </Menu.Sub>
        {promotions.any && (
          <Menu.Item
            leftSection={<IconSparkles size={14} />}
            rightSection={
              unreadNews > 0 ? (
                <Badge size="xs" circle variant="filled">
                  {unreadNews}
                </Badge>
              ) : undefined
            }
            onClick={promotions.browse}
          >
            What's new…
          </Menu.Item>
        )}
        <Menu.Item
          leftSection={<IconKeyboard size={14} />}
          onClick={() => setDialog({ kind: "shortcuts" })}
        >
          Shortcuts…
        </Menu.Item>
        <Menu.Divider />
        <Menu.Item
          leftSection={<IconSettings size={14} />}
          onClick={() => setDialog({ kind: "settings" })}
        >
          Settings
        </Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );

  /**
   * Settings, as an element rather than inline in the dialog list.
   *
   * Both modes render it. Runs mode returns early below — before the dialog list —
   * so an inline `dialog.kind === "settings"` there was unreachable: the ⋯ menu and
   * ⌘, both set the state and nothing appeared. Every other dialog in the list
   * belongs to worktree mode's own surfaces; this is the one that is reachable from
   * both, so this is the one that has to be hoisted.
   */
  const settingsDialog = dialog.kind === "settings" && (
    <SettingsDialog
      settings={settings}
      saving={savingSettings}
      error={settingsError}
      onSave={saveSettings}
      onClose={closeDialog}
    />
  );

  /** Hoisted for the same reason as `settingsDialog` — reachable from the ⋯
   *  menu in both view modes, so it has to render before runs mode's early
   *  return. */
  const shortcutsDialog = dialog.kind === "shortcuts" && (
    <ShortcutsDialog onClose={closeDialog} />
  );

  /**
   * Hoisted for the same reason as `settingsDialog`, with a sharper version of
   * it: this one can open *itself*. Runs mode returns early before the dialog
   * list, so a promotion that came up while the user was over there would set
   * the state, render nothing, and never be marked — leaving a panel
   * permanently "open" that they can neither see nor dismiss.
   */
  const whatsNewDialog = promotions.open && (
    <WhatsNewDialog
      cards={promotions.open.cards}
      automatic={promotions.open.automatic}
      projectName={promotions.projectName}
      onRead={promotions.markRead}
      onDismiss={promotions.dismiss}
    />
  );

  /**
   * Hoisted for the same reason as `settingsDialog`: a start can be held back
   * from either mode, so the dialog that unblocks it has to render in both.
   */
  const configVarsDialog = dialog.kind === "config-vars" && (
    <ConfigVarsDialog
      opened
      project={dialog.project}
      onRetry={dialog.retry}
      // Closing re-reads whether any var is now overridden — the dialog is the
      // only surface that changes that, so it is the only moment worth the read.
      onClose={() => {
        closeDialog();
        setVarsTick((n) => n + 1);
      }}
    />
  );

  /** Fetch + fast-forward the main checkout, then re-read the repo so the
   *  staleness badge clears in the same breath. Shared by the direct click
   *  and the dirty-confirm dialog's "revert, then update" button. */
  const doUpdateMain = async () => {
    if (!repo) return;
    setUpdatingMain(true);
    try {
      await api.updateMain(repo.root);
      await refresh();
    } catch (e) {
      notifyError("Could not update main", e);
      await refresh();
    } finally {
      setUpdatingMain(false);
    }
  };

  /**
   * The one-click "update main". Mirrors the trash flow: check the repo
   * root's dirty state up front (same `git status` the daemon's own refusal
   * would otherwise surface a moment later as a bare toast) and, if dirty,
   * turn it into a choice — revert first, or cancel — instead of failing
   * blind. A clean tree skips straight to the fast-forward as before.
   */
  const updateMain = async () => {
    if (!repo || updatingMain) return;
    const main = worktrees.find((w) => w.is_main);
    if (main) {
      try {
        const status = await api.worktreeGitStatus(main.id);
        if (status.dirty) {
          setDialog({ kind: "update-main-dirty", root: repo.root, status });
          return;
        }
      } catch {
        // Status unavailable (git error, checkout gone): fall through and let
        // the daemon's own dirty check surface the refusal, as before.
      }
    }
    await doUpdateMain();
  };

  // ---- keyboard shortcuts: refs for the [] keydown effect above -----------
  //
  // Everything the effect declares near the top of this component and calls
  // through by name is mirrored into a ref here, the same idiom `layoutsRef`
  // and `worktreeRef` use. **Declared above both of this component's early
  // returns (`mode === "runs"`, `chromeless`) on purpose** — a hook has to run
  // on every render regardless of which branch that render takes, and a
  // `useRef` reached only on the "ide, not chromeless" path is exactly the
  // conditional-hook shape that corrupts every hook after it. The assignments
  // right below each one are plain statements, not hooks, and running only on
  // that one path would just mean these shortcuts see a stale value while the
  // window is chromeless or in Runs view — which does not happen today because
  // this sits before both branches, but would be the failure mode of moving it
  // back down without moving the branches too.
  const lanesRef = useRef(lanes);
  lanesRef.current = lanes;
  const selectWorktreeRef = useRef(selectWorktree);
  selectWorktreeRef.current = selectWorktree;
  const runsRef = useRef(runs);
  runsRef.current = runs;
  const setSelectedRunNameRef = useRef(setSelectedRunName);
  setSelectedRunNameRef.current = setSelectedRunName;
  const boundRunRef = useRef(run);
  boundRunRef.current = run;
  const runningRef = useRef(false);
  runningRef.current = !!run?.live && status !== "stopped";
  // Mirrors `TopBar`'s own `starting`/`pending`/`canStart` exactly (App.tsx's
  // `TopBar` component, the ▶/■/⟳ buttons) so the keyboard equivalents below
  // are gated on the same conditions those buttons disable on — a keyboard
  // start/stop/restart must not fire where the click it stands in for
  // wouldn't have.
  const pendingActionRef = useRef(pendingForRun(worktree, run?.name));
  pendingActionRef.current = pendingForRun(worktree, run?.name);
  const startingRef = useRef(false);
  startingRef.current = spinnerAction(pendingActionRef.current, run) === "start";
  const canStartRef = useRef(false);
  canStartRef.current = worktree ? canStartWorktree(worktree) : false;
  const canRunWorktreeNowRef = useRef(canRunWorktreeNow);
  canRunWorktreeNowRef.current = canRunWorktreeNow;
  const startOrOpenPickerRef = useRef(startOrOpenPicker);
  startOrOpenPickerRef.current = startOrOpenPicker;
  const stopWorktreeRef = useRef(stopWorktree);
  stopWorktreeRef.current = stopWorktree;
  const restartWorktreeRef = useRef(restartWorktree);
  restartWorktreeRef.current = restartWorktree;
  const modeRef = useRef(mode);
  modeRef.current = mode;
  const setModeRef = useRef(setMode);
  setModeRef.current = setMode;
  const saveSettingsRef = useRef(saveSettings);
  saveSettingsRef.current = saveSettings;
  const updateMainRef = useRef(updateMain);
  updateMainRef.current = updateMain;

  /** Move the rail's selection up or down, in the order it renders — the
   *  flattened `railGroups`, not raw `worktrees` — wrapping at both ends like
   *  tab-cycling below: past the last worktree goes to the first and vice
   *  versa, rather than stopping dead at either end of the rail. */
  function stepWorktree(delta: number) {
    const wt = worktreeRef.current;
    if (!wt) return;
    const order = railGroups(worktreesRef.current, lanesRef.current).flatMap((g) => g.worktrees);
    const idx = order.findIndex((w) => w.id === wt.id);
    const next = order[nextIndex(idx, delta, order.length)];
    if (next) void selectWorktreeRef.current(next);
  }

  /**
   * Focus the next/previous tab, continuing into this worktree's own detached
   * windows once the docked ones run out, and wrapping at both ends — unlike
   * `stepWorktree`, a small fixed tab strip is the case every tabbed app wraps.
   * Works the same way in a chromeless (detached) window, where the one
   * "other" entry is `originWindowId` — the window this one came from — so
   * cycling out of a detached dock has somewhere to go rather than only ever
   * wrapping inside it.
   *
   * A detached tab is not in any `Dock` any more (see `panes/model.ts`), so
   * landing on one means asking the shell to raise that *other* window rather
   * than calling `activateTab`. `focus` resolving `false` — or its promise
   * rejecting, which `ipcRenderer.invoke` does when the main process throws —
   * means that window has since closed with no way this one could have heard
   * about it: the id is forgotten and the same press retries the next
   * candidate, which is the promise `shell.ts`'s and `windows.js`'s own
   * docstrings for `focus` make. `tried` is what makes that retry terminate:
   * it is *this call's* local record of ids already found stale, filtered out
   * of `detachedWindowsRef`'s copy before recomputing the order, rather than
   * relying on the state update `forgetDetachedWindow` schedules — which is
   * not visible on `detachedWindowsRef.current` until the next render, well
   * after a synchronous retry would need it to be. In a chromeless window
   * `originWindowId` is not tracked in that state at all (there is nothing to
   * forget), so a failed origin is simply excluded from `tried` on the next
   * loop rather than ever retried a second time.
   */
  function stepTab(delta: number, tried: ReadonlySet<string> = new Set()) {
    const wt = worktreeRef.current;
    if (!wt) return;
    const layout = layoutsRef.current[wt.id];
    const docked = layout ? allTabs(layout).map((t) => t.id) : [];
    const detachedAll = chromeless
      ? (() => {
          const origin = getOriginWindowId();
          return origin ? [origin] : [];
        })()
      : (detachedWindowsRef.current[wt.id] ?? []);
    const detached = detachedAll.filter((id) => !tried.has(id));
    const order = [...docked, ...detached];
    if (order.length === 0) return;
    const currentId = layout ? layout.docks[layout.focused].activeId : null;
    const idx = currentId ? order.indexOf(currentId) : -1;
    const id = order[nextIndex(idx, delta, order.length)];
    if (docked.includes(id)) {
      setLayouts((prev) => {
        const current = prev[wt.id];
        return current ? { ...prev, [wt.id]: activateTab(current, id) } : prev;
      });
      // `activateTab` only updates layout state — it does not move DOM focus,
      // and the green "focused pane" border is driven by real `:focus-within`
      // (see `PaneArea.tsx`), not by that state. A click gets this for free
      // (focusing the button IS the click); a keyboard press has no click to
      // piggyback on, so without this the border silently stops tracking
      // cycling and keeps pointing at wherever focus last was. The button
      // already exists in the DOM regardless of which tab is selected, so
      // this does not need to wait for the state update above to commit.
      document.getElementById(tabElementId(id))?.focus();
      return;
    }
    if (!desktopWindow || !desktopWindow.focus) return;
    const retry = () => {
      if (!chromeless) forgetDetachedWindow(wt.id, id);
      stepTab(delta, new Set([...tried, id]));
    };
    desktopWindow
      .focus(id)
      .then((ok) => {
        if (!ok) retry();
      })
      .catch(retry);
  }

  /** Step the run selector to the next entry — "select preset" from the
   *  keyboard. Live runs only, the same default the selector itself opens
   *  with before "show ended" is clicked. Forward only: the one caller
   *  (⌘⇧O) has no reverse chord, so a `delta` parameter would be dead. */
  function stepPreset() {
    const list = sortRunsForDisplay(
      selectorRuns(runsRef.current, selectedRunRef.current || undefined, false).runs,
    );
    if (list.length === 0) return;
    const idx = list.findIndex((r) => r.name === selectedRunRef.current);
    setSelectedRunNameRef.current(list[nextIndex(idx, 1, list.length)].name);
  }

  if (mode === "runs") {
    return (
      <div className="frame">
        <RunsMode
          modeSwitch={modeSwitch}
          controls={topBarControls}
          overflowMenu={overflowMenu}
          historyDays={historyDays}
          logsTz={logsTz}
        />
        {settingsDialog}
        {configVarsDialog}
        {whatsNewDialog}
        {shortcutsDialog}
      </div>
    );
  }

  /**
   * The top-level Sharing surface (#152: one surface, not a relay-details dump).
   *
   * A **modal**, not the popover this started as. The popover was 430px of wrapping
   * buttons: a share of three services put six identically-shaped controls in a bag
   * and the QR had nowhere to go but underneath everything. The panel's content is a
   * table — controls, then a row per service — and a table needs width. Being a
   * modal also means `overlayGuard` hides the embedded browser panes while it is
   * open, which the popover only got by being portalled.
   */
  const shareEmptyHint = !worktree
    ? "Select a worktree."
    : !worktree.has_veld_config
      ? "This worktree has no veld.json, so it has no run to share."
      : !diagRun
        ? "Start the run to share it."
        : "This run has nothing shared yet.";
  // Shown while the worktree *could* run something, and also whenever a share is
  // live: a repo whose directory has gone missing still has a share to stop, and
  // gating on startability was the one path that hid the only control that ends it.
  // Disabled when there is nothing to share (no live run to share, and no share
  // to stop) — the panel would otherwise offer to share a run that is not running.
  const shareDisabled = !sharingActive && diagRun?.status !== "running";
  const sharingSurface =
    worktree &&
    (canRunWorktreeNow(worktree) || sharingActive) &&
    // Same hide-vs-disable rule as the other inapplicable actions: hidden when
    // `ui.hideDisabledActions` is on, shown greyed (with the reason in its
    // tooltip) when off.
    !(shareDisabled && hideDisabled) ? (
      <Tooltip
        label={
          sharingActive
            ? "This run is shared right now — open for links, QR codes and connections"
            : shareDisabled
              ? "Start the run to share it"
              : "Share this run privately with a peer, or publish it to the web"
        }
      >
        {/* The wrapper lets the tooltip open while the button is disabled — the
            #205 trap: a disabled ActionIcon has `pointer-events: none`, and the
            *why* is exactly what a greyed share button needs. */}
        <span className="bar-hover-slot">
          <ActionIcon
            size="md"
            /* One action, two readings: outline `IconShare` when nothing is
               shared, filled green `IconBuildingBroadcastTower` when it is —
               "on air" is what the icon says, and colour plus fill carry it
               without a word widening the bar. */
            variant={sharingActive ? "filled" : "default"}
            color={sharingActive ? "green" : undefined}
            disabled={shareDisabled}
            aria-label={
              sharingActive
                ? "Sharing live — open the sharing panel"
                : "Share this run"
            }
            onClick={() => setDialog({ kind: "sharing" })}
          >
            {sharingActive ? <IconBuildingBroadcastTower size={14} /> : <IconShare size={14} />}
          </ActionIcon>
        </span>
      </Tooltip>
    ) : null;

  const sharingDialog = dialog.kind === "sharing" && (
    <Modal title="Sharing" onClose={closeDialog}>
      <RunSharePanel
        run={diagRef}
        runId={diagRun?.run_id ?? null}
        running={diagRun?.status === "running"}
        shares={shares?.shares ?? []}
        unknown={shares === null || sharesStale}
        otherRuns={otherRunShares}
        emptyHint={shareEmptyHint}
        onChanged={() => void refresh()}
      />
    </Modal>
  );

  /**
   * A detached window: the dock and nothing else.
   *
   * Same component, same data, no chrome — a rail and a worktree switcher in a
   * window holding one terminal would be the app's furniture around a single
   * pane. The selection still comes from `?repo=&wt=` (the shell puts the origin
   * window's there when it opens this one), so everything below the top bar
   * resolves exactly as it does in a full window.
   */
  if (chromeless) {
    return (
      <div className="frame chromeless">
        {worktree && layout && (
          <PaneArea
            layout={layout}
            onLayout={setLayout}
            worktreeId={worktree.id}
            repoRoot={worktree.repo_root}
            serviceUrls={urls}
            quicklinks={worktree.ide.quicklinks}
            panes={worktree.ide.panes}
            paneSessions={paneSessions}
            urlsEmptyHint={
              worktree.has_veld_config
                ? "Start the run and its services appear here."
                : "This worktree has no veld.json, so there is nothing to run."
            }
            sessions={sessions}
            onAddSession={nextSession ? addSession : undefined}
            onRemoveSession={removeSession}
            quickSwitches={quickSwitches}
            showWorking={activity.showWorking}
            runCtx={runCtx}
            searchUrl={searchTemplate}
          />
        )}
      </div>
    );
  }

  return (
    <div className="frame">
      <TopBar
        modeSwitch={modeSwitch}
        repos={repos}
        repo={repo}
        worktree={worktree}
        gitStatus={repo?.git ?? null}
        updateMain={updateMain}
        updatingMain={updatingMain}
        startConfig={
          worktree && canRunWorktreeNow(worktree) ? (
            <StartConfig
              worktree={worktree}
              value={storedStart}
              opened={startConfigOpen}
              onOpen={() => setStartConfigOpen(true)}
              onClose={() => {
                setStartConfigOpen(false);
                startAfterDone.current = false;
              }}
              onChange={(sel) => setStartRaw(JSON.stringify(sel))}
              onDone={() => {
                setStartConfigOpen(false);
                const shouldStart = startAfterDone.current;
                startAfterDone.current = false;
                if (shouldStart && worktree) startWorktree(worktree);
              }}
            />
          ) : null
        }
        canStart={worktree ? canStartWorktree(worktree) : false}
        // A bound run that has ended is not "running": ■ would have nothing to
        // stop, and ▶ starts that same environment again (`startRunName`).
        running={!!run?.live && status !== "stopped"}
        pending={pendingForRun(worktree, run?.name)}
        spinner={spinnerAction(pendingForRun(worktree, run?.name), run)}
        run={run}
        runSelect={
          worktree && canRunWorktreeNow(worktree) ? (
            <RunSelect
              // Remounts per worktree, which is what actually enforces the
              // "reveal ended runs for this opening only" rule: React reconciles
              // this element by position, so without a key the component keeps its
              // `showEnded` state across menu closes *and* worktree switches, and
              // one click leaked the reveal to every later worktree in the window.
              key={worktree.path}
              runs={runs}
              selected={run}
              missing={pick.missing}
              // Keyed on the *chosen* name, which is the one that may not exist
              // yet — `run` is the fallback, and its marker says nothing about the
              // start this window just fired.
              pending={pendingForRun(worktree, selectedRunName || run?.name)}
              siblingStatus={siblingStatus}
              // Straight through, `null` included: `null` means the config could
              // not be read, which `startOriginLabel` renders as "cannot compare"
              // rather than as "the preset was deleted".
              presets={worktree.presets}
              startName={anotherNameFor(worktree)}
              startLabel={startSelectionLabel(effectiveStart, worktree)}
              canStartAnother={canStartAnother(worktree)}
              onStartAnother={() =>
                startWorktree(worktree, anotherNameFor(worktree))
              }
              onSelect={setSelectedRunName}
              // Disabled when the selector has nothing to offer: fewer than two
              // runs *and* no other run to start. A single **running** run still
              // stays enabled when `canStartAnother` — a coding agent's run under
              // a non-auto name is exactly that case — so "start another run"
              // (the auto/alias name) stays reachable from the menu.
              disabled={runs.length < 2 && !canStartAnother(worktree)}
            />
          ) : null
        }
        runSelectDisabled={
          !worktree ||
          !canRunWorktreeNow(worktree) ||
          (runs.length < 2 && !canStartAnother(worktree))
        }
        urls={urls}
        sharing={sharingSurface}
        onShowBlankBrowser={layout && showBlankBrowser}
        extensions={worktree?.ide.extensions ?? []}
        onOpenInPane={layout ? openUrlInPane : undefined}
        showWorking={activity.showWorking}
        elsewhere={elsewhere}
        canOpenWindow={canOpenSecondView}
        secondViewLabel={secondViewLabel}
        projectColumnButton={projectColumnButton}
        onSelectRepo={(root) => {
          setActiveRepoRoot(root);
          setActiveWtKey("");
        }}
        onOpenProjectWindow={(root) => void openProjectWindow(root)}
        onImport={() => setDialog({ kind: "import" })}
        onRemoveRepo={(r) => setDialog({ kind: "remove-repo", repo: r })}
        onMoveProject={reorderProjectsTo}
        // The bound run, named explicitly: the top bar is the run-level surface,
        // and ■ here must end the run whose name the selector is showing — never
        // whichever one `activeRun` would have picked.
        onStart={() => worktree && startOrOpenPicker(worktree)}
        onStop={() => worktree && stopWorktree(worktree, run)}
        onRestart={() => worktree && restartWorktree(worktree, run)}
        controls={topBarControls}
        overflowMenu={overflowMenu}
        configVarsButton={configVarsButton}
        nodeActions={nodeActionsButton}
        hideDisabled={hideDisabled}
      />

      {offline && (
        <div
          style={{
            padding: "6px 14px",
            background: "var(--warn-bg)",
            color: "var(--warn)",
            fontSize: 12,
            flex: "none",
          }}
        >
          Can&apos;t reach the veld daemon — is it running? Retrying…
        </div>
      )}
      {/* The daemon is reachable over HTTP but its control socket is not, which
          is a state the page can otherwise only report to the devtools console.
          Suppressed while `offline` is up: a daemon that is not there explains
          both, and two banners about one outage is worse than either. */}
      {channelDown && !offline && (
        <div
          style={{
            padding: "6px 14px",
            background: "var(--warn-bg)",
            color: "var(--warn)",
            fontSize: 12,
            flex: "none",
          }}
        >
          Not connected to Veld&apos;s worktree channel — opening a worktree and
          attaching a terminal will not work until it is back. Retrying…
        </div>
      )}
      {/* Join requests are a prompt, so they go where they are visible without
          opening anything — someone is sitting on the other end waiting for an
          answer, and a badge on a popover is not that. Every share's requests,
          not the selected worktree's, each naming the run it is for. */}
      {pendingJoins.length > 0 && (
        <div className="join-banner">
          {pendingJoins.map((p) => (
            <JoinRequestRow
              key={p.id}
              pending={p}
              runLabel={runOfShare(shares?.shares ?? [], p.share_id)}
              onChanged={() => void refresh()}
            />
          ))}
        </div>
      )}

      {repoList === null ? (
        // First load: don't flash the empty-state CTA before data arrives.
        <div className="center-page">
          <Loader size="sm" aria-label="Loading" />
        </div>
      ) : repos.length === 0 ? (
        <StartScreen onImport={() => setDialog({ kind: "import" })} />
      ) : (
        <div className="workspace">
          {showProjectColumn && (
            <ProjectColumn
              repos={repos}
              activeRoot={repo?.root ?? null}
              showWorking={activity.showWorking}
              elsewhere={elsewhere}
              onSelect={(root) => {
                setActiveRepoRoot(root);
                setActiveWtKey("");
              }}
              onReorder={reorderProjectsTo}
              onMenu={(e, r) => projectMenu(r)(e)}
              onImport={() => setDialog({ kind: "import" })}
            />
          )}
          <Rail
            worktrees={worktrees}
            lanes={lanes}
            active={worktree}
            envs={envs}
            settings={settings}
            wide={railWide}
            width={railW}
            canRun={canRunWorktreeNow}
            canStart={canStartWorktree}
            pendingFor={pendingFor}
            elsewhere={elsewhere}
            onToggle={() => setRailWide((v) => !v)}
            onWidth={(w) => setRailWidthRaw(String(w))}
            onSelect={(w) => void selectWorktree(w)}
            onAdd={(lane) => setDialog({ kind: "new-worktree", lane })}
            onMenu={(e, w) => worktreeMenu(w)(e)}
            onStart={startOrOpenPicker}
            onStop={stopWorktree}
            onDiagnose={diagnoseWorktree}
            showWorking={activity.showWorking}
            onAddLane={() => setDialog({ kind: "new-lane" })}
            onLaneMenu={(e, lane) => laneMenu(lane)(e)}
            onMove={moveWorktreeTo}
            onMoveLane={(lane, onto) => void moveLaneTo(lane, onto)}
            onRestore={restoreWorktree}
            onEmptyTrash={emptyTrash}
            onTrashAllDetached={trashAllDetached}
            onTrashDrop={trashWorktree}
            deleting={deletingIds}
          />
          {/* **Beside the rail, never instead of it.** This used to replace the
              whole workspace, which took away the one control that resolves the
              state it is reporting: the rail is where another worktree is
              picked, where a "＋" per lane creates one, and where the rows this
              window cannot have are visibly marked as belonging to somebody. A
              window with one worktree in the repo therefore lost its rail on
              open and got it back only by ⌘K.

              **`claimBlocked` still outranks the panes**, which is the ordering
              the old branch had by construction and this one has to keep on
              purpose. `layout` is `layouts[worktree.id]`, and a worktree this
              window was refused can still have an entry there — it held that
              worktree until a moment ago. Rendering the panes then attaches to
              PTY sessions another client is driving, and an attach *takes a
              session over*: the exact failure the arbitration exists to
              prevent. */}
          {claimBlocked ? (
            // Every worktree is on screen in another client. Saying so beats the
            // alternative above — rendering a set of panes that belongs to
            // another window, and taking its shells on the way.
            <div className="center-page">
              <p>Every worktree is already open somewhere else.</p>
              <Button
                size="md"
                variant="default"
                onClick={() => setDialog({ kind: "new-worktree", lane: "" })}
              >
                Create a worktree
              </Button>
            </div>
          ) : worktree && layout ? (
            <PaneArea
              layout={layout}
              onLayout={setLayout}
              worktreeId={worktree.id}
              repoRoot={worktree.repo_root}
              serviceUrls={urls}
              quicklinks={worktree.ide.quicklinks}
              panes={worktree.ide.panes}
              paneSessions={paneSessions}
              urlsEmptyHint={
                worktree.has_veld_config
                  ? "Start the run and its services appear here."
                  : "This worktree has no veld.json, so there is nothing to run."
              }
              sessions={sessions}
              onAddSession={nextSession ? addSession : undefined}
              onRemoveSession={removeSession}
              quickSwitches={quickSwitches}
              showWorking={activity.showWorking}
              runCtx={runCtx}
              searchUrl={searchTemplate}
              onDetachedWindow={onWorktreeDetachedWindow}
            />
          ) : null}
        </div>
      )}

      {dialog.kind === "import" && (
        <ImportRepoDialog
          onClose={closeDialog}
          onImport={async (path) => {
            const imported = await api.importRepo(path);
            await refresh();
            setActiveRepoRoot(imported.root);
            setActiveWtKey("");
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "new-worktree" && repo && (
        <NewWorktreeDialog
          onClose={closeDialog}
          takenAliases={worktrees.map((w) => w.alias)}
          lane={dialog.lane}
          usedBy={markerUsedBy.emoji}
          colorUsedBy={markerUsedBy.color}
          markerStyle={markerStyle(settings ?? {})}
          onStyleChange={(style) => void saveSettings({ "worktree.markerStyle": style })}
          createFrom={gitCreateFrom(settings ?? {})}
          onCreate={async (body) => {
            let created: Worktree;
            try {
              created = await api.createWorktree({
                repo_root: repo.root,
                lane: dialog.lane,
                ...body,
              });
            } catch (e) {
              // A create can fail *after* `git worktree add` has succeeded — the
              // alias rename losing a race, or the lane vanishing mid-flight.
              // The dialog reports the error and stays open, but without this the
              // row it did create stayed invisible until the next 5s poll, and
              // pressing Create again hit "<path> already exists" with no way
              // forward. Refresh first, so what exists is on screen while the
              // user reads why the rest did not.
              await refresh();
              throw e;
            }
            // Newest first, in the section it was created into. Unplaced rows sort
            // last (`WT_ORDER`), so a new checkout used to appear at the bottom of
            // a long lane — furthest from the "＋" that was just clicked, and
            // furthest from the row the user is about to work in. The order is
            // computed from the list *plus* the created row rather than after a
            // refresh, because `worktrees` here is this render's list and would
            // still be the pre-create one.
            //
            // Only the rows that were **already hand-placed**, plus this one.
            // `moveWorktree` returns the whole repo's order, and writing that
            // would give a `sort_position` to every unplaced row in every
            // section — one click of any "＋" silently freezing the label sort
            // repo-wide, and falsifying the promise that what you have not
            // placed stays alphabetical. `reorder_worktrees` clears the
            // positions it is not given, so the omitted rows stay unplaced,
            // which is exactly the state they should keep.
            const placed = new Set(
              worktrees.filter((w) => w.sort_position !== null).map((w) => w.path),
            );
            const order = moveWorktree(
              railGroups([...worktrees, created], lanes),
              created.path,
              dialog.lane,
              0,
            );
            if (order) {
              try {
                await api.reorderWorktrees(
                  repo.root,
                  order.order.filter((p) => p === created.path || placed.has(p)),
                );
              } catch (e) {
                // The worktree exists and is usable; only its position is wrong.
                // Reported, not thrown — throwing would keep the create dialog
                // open over a create that succeeded.
                notifyError("Could not place the new worktree", e);
              }
            }
            await refresh();
            setActiveWtKey(String(created.id));
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "remove-repo" && (
        <RemoveRepoDialog
          repo={dialog.repo}
          onClose={closeDialog}
          onRemove={async () => {
            const removed = dialog.repo.root;
            await api.removeRepo(removed);
            // **Only when the removed project is the one on screen.** This used to
            // be unconditional and correct, because the sole entry point was a menu
            // bound to the *selected* repo. Removal is now reachable for any project
            // (the column's context menu, ⌘K, the project submenu), and clearing
            // regardless threw the window off a project the user was working in —
            // dropping the worktree key and starting a fresh acquire hunt — because
            // they removed some other one.
            if (removed === activeRepoRootRef.current) {
              setActiveRepoRoot("");
              setActiveWtKey("");
            }
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "rename" && (
        <RenameWorktreeDialog
          currentAlias={dialog.worktree.alias}
          /* `?? ""` because the field can genuinely be absent: `api.ts` types it
             as the current daemon's contract, but `just dev-ui` proxies /api to
             a locally *installed* daemon, which may predate v13 and send no such
             key. Without this the dialog crashed on `undefined.trim()` at save
             time — `worktreeLabel` already tolerates the same skew. */
          currentName={dialog.worktree.display_name ?? ""}
          onClose={closeDialog}
          onRename={async (patch) => {
            await api.patchWorktree(dialog.worktree.id, patch);
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "trash" && (
        <TrashWorktreeDialog
          /* Read off the LIVE row, not the one captured when the dialog opened: a
             background removal can fail while it is open, and the force affordance
             has to appear when the refusal arrives. */
          trashError={
            worktrees.find((w) => w.id === dialog.worktree.id)?.trash_error ?? ""
          }
          worktreeId={dialog.worktree.id}
          onStatus={(id) => api.worktreeGitStatus(id)}
          onRevert={(id) => api.revertWorktree(id)}
          onClose={closeDialog}
          onTrash={async (force) => {
            await api.deleteWorktree(dialog.worktree.id, force);
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "confirm-delete" && (
        <ConfirmDeleteWorktreeDialog
          label={worktreeLabel(dialog.worktree)}
          status={dialog.status}
          onClose={closeDialog}
          onDeleteDiscard={async () => {
            await api.deleteWorktree(dialog.worktree.id, true);
            await refresh();
            closeDialog();
          }}
          onRevertThenDelete={async () => {
            await api.revertWorktree(dialog.worktree.id);
            await enqueueDelete(dialog.worktree);
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "update-main-dirty" && (
        <UpdateMainDirtyDialog
          status={dialog.status}
          onClose={closeDialog}
          onRevertThenUpdate={async () => {
            await api.revertRepoRoot(dialog.root);
            await doUpdateMain();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "marker" && (
        <ChangeMarkerDialog
          current={dialog.worktree.emoji}
          currentColor={dialog.worktree.marker_color}
          label={worktreeLabel(dialog.worktree)}
          worktreeId={dialog.worktree.id}
          usedBy={markerUsedBy.emoji}
          colorUsedBy={markerUsedBy.color}
          style={markerStyle(settings ?? {})}
          onStyleChange={(style) => void saveSettings({ "worktree.markerStyle": style })}
          onClose={closeDialog}
          onPick={async (patch) => {
            await api.patchWorktree(dialog.worktree.id, patch);
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "new-lane" && (
        <LaneNameDialog
          title="New group"
          confirmLabel="Create group"
          initial=""
          taken={lanes.map((l) => l.name)}
          onClose={closeDialog}
          onSubmit={async (name) => {
            if (!repo) return;
            await api.createLane(repo.root, name);
            // Creating a lane from a worktree's own menu is one gesture, so it
            // finishes the gesture: an empty lane the user then has to drag into
            // is not what they asked for.
            const target = dialog.kind === "new-lane" ? dialog.worktree : undefined;
            if (target) await api.patchWorktree(target.id, { lane: name });
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "rename-lane" && (
        <LaneNameDialog
          title="Rename group"
          confirmLabel="Rename"
          initial={dialog.lane}
          taken={lanes
            .map((l) => l.name)
            .filter((n) => n !== (dialog.kind === "rename-lane" ? dialog.lane : ""))}
          onClose={closeDialog}
          onSubmit={async (name) => {
            if (!repo || dialog.kind !== "rename-lane") return;
            await api.renameLane(repo.root, dialog.lane, name);
            await refresh();
            closeDialog();
          }}
        />
      )}
      {settingsDialog}
      {configVarsDialog}
      {whatsNewDialog}
      {shortcutsDialog}
      {sharingDialog}
      {dialog.kind === "search" && (
        <CommandPalette
          project={repo?.name ?? ""}
          items={buildPaletteItems()}
          settings={settings}
          onClose={closeDialog}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Top bar
// ---------------------------------------------------------------------------

/**
 * Which run the top bar — and every surface bound to it — is showing.
 *
 * This replaces the bare status dot rather than sitting next to it, because the
 * two answer one question: *which* run, and how is it. The dot alone was the
 * defect's other half — it reported a run the app had picked by sort order while
 * the start control beside it named a preset from local storage, and the pair read
 * as "preset X is running".
 *
 * Three properties are deliberate, not decoration:
 *
 * - **The closed control shows that siblings exist and whether one is sick.** A
 *   dropdown is a mode, and a mode is invisible while you are not looking at it;
 *   a selector that hid the second run behind a click would be a tidier version
 *   of the bug it fixes. So the counter is always rendered when there is more than
 *   one run, and it takes the worst sibling's colour.
 * - **A vanished choice is stated, never silently replaced.** `missing` means the
 *   stored name has no environment any more; the control says so instead of
 *   presenting a fallback under the name the user last chose.
 * - **The name ▶ will create is shown before it is pressed.** A run appearing
 *   under a name nobody typed is what made two runs in one directory confusing in
 *   the first place.
 */
function RunSelect(props: {
  runs: RunInfo[];
  selected: RunInfo | null;
  missing: string | null;
  /**
   * The action this window has in flight against the *bound name*.
   *
   * Only `"start"` changes what is rendered, and only together with `missing`: it
   * separates "the environment you chose is gone" from "the environment you just
   * asked for has not been listed yet".
   */
  pending: PendingAction | null;
  siblingStatus: WorktreeStatus;
  /** `null` when this worktree's config could not be parsed — see `startOriginLabel`. */
  presets: Preset[] | null;
  /** The environment name a fresh start would create — shown before it happens. */
  startName: string;
  /** What that start would run (the stored preset/selection label). */
  startLabel: string;
  canStartAnother: boolean;
  onStartAnother: () => void;
  onSelect: (name: string) => void;
  /** No runs to choose from — the button is shown greyed (or hidden, see
   *  `ui.hideDisabledActions`) and the menu cannot open. */
  disabled: boolean;
}) {
  const { selected, missing } = props;
  /**
   * A chosen name with no environment *yet*, because this window just started it.
   *
   * Without this the honest-but-wrong reading wins for a poll or two: the click
   * binds the window to `dev-2`, the daemon has not listed it, and the control
   * announces "no environment named dev-2 here" about a run it is in the middle of
   * creating. A pending **start** against that exact name is the difference between
   * "gone" and "not there yet".
   */
  const awaiting = missing !== null && props.pending === "start" ? missing : null;
  // Ended environments are hidden behind a disclosure rather than listed: the
  // control answers "which of the things happening here", and history is Runs
  // mode's job. `showEnded` is local and per-opening — a preference for this
  // would be a persisted answer to a question that has a right one.
  const [showEnded, setShowEnded] = useState(false);
  const { runs, hidden } = selectorRuns(props.runs, selected?.name, showEnded);
  // Listed newest-first within each group (live by start, ended by stop) so
  // "the run I last touched" leads the picker instead of sitting by name.
  const orderedRuns = sortRunsForDisplay(runs);
  // Position counted over the *sorted* list so the counter matches the order
  // the menu actually shows (an unordered findIndex would label "1/3" a run
  // rendered third).
  const position = selected
    ? orderedRuns.findIndex((r) => r.name === selected.name) + 1
    : 0;
  // Counted over what is *listed*, so the counter and the dropdown cannot
  // disagree — "1/4" above a two-row list is worse than no counter.
  const siblingAlert = runs.length > 1 && needsAttention(props.siblingStatus);
  const origin = startOriginLabel(selected?.started_from, props.presets);
  const tooltip = [
    awaiting
      ? `Run ${awaiting}: starting…`
      : selected
        ? // The pending action, not just the observed status. The status dot this
          // control replaced announced `Run dev: stop…` while an action was in
          // flight, and dropping that left a screen reader with nothing but a
          // spinner on a neighbouring icon button.
          `Run ${selected.name}: ${props.pending ? `${props.pending}…` : selected.status}`
        : "No run selected",
    origin && !awaiting ? `Started from ${origin}` : null,
    // Neutral about *why* there is no such environment: it may have been removed,
    // or a start fired against that name may never have produced one (the CLI died
    // on startup). Both are "there is nothing here called that".
    missing && !awaiting ? `No environment named ${missing} here` : null,
    runs.length > 1
      ? `${runs.length} runs in this worktree${siblingAlert ? " — another needs attention" : ""}`
      : null,
    hidden > 0 ? `${hidden} ended, hidden` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <Menu position="bottom-start" width={300}>
      <Menu.Target>
        {/* The run's name is deliberately **not** the visible label. The dot
            answers "is something running, and how", and the `x/x` counter
            answers "which of several" — both without the name, which is one
            hover (the `title` above) away. A name here made the control as
            wide as the longest environment in the worktree and read as if the
            bar were about that run rather than about the actions on it. The
            dot is the button's only content, so it centres in a square cell;
            the counter rides beside it when there is more than one run. */}
        <Button
          size="compact-sm"
          variant="default"
          className="run-select"
          title={tooltip}
          disabled={props.disabled}
        >
          <span
            // `partial` while awaiting: the same amber a `starting` run gets,
            // because that is exactly what this is — the daemon simply has not
            // listed it yet.
            className={`dot ${awaiting ? "partial" : runStatus(selected)}`}
            role="img"
            aria-label={
              awaiting
                ? `Run ${awaiting}: starting`
                : selected
                  ? `Run ${selected.name}: ${props.pending ?? selected.status}`
                  : "No run selected"
            }
          />
          {/* Hidden while awaiting: `position` indexes the listed runs, and the
              environment being started is not among them yet, so the counter would
              read `dev-2  1/2` with the `1` pointing at a different run. */}
          {runs.length > 1 && !awaiting && (
            <span className={`run-count${siblingAlert ? " alert" : ""}`}>
              {position}/{runs.length}
            </span>
          )}
        </Button>
      </Menu.Target>
      {/* `closeOnItemClick={false}` would swallow a selection; the disclosure
          instead stops propagation itself, so revealing the ended entries does
          not close the menu you wanted to read. */}
      <Menu.Dropdown>
        {/* "Live" only when something is actually being held back. A header
            reading "Live runs" above three stopped ones — the state when the whole
            directory is down and the filter has nothing to do — describes the rule
            rather than the list. */}
        <Menu.Label>
          {hidden > 0 ? "Live runs in this worktree" : "Runs in this worktree"}
        </Menu.Label>
        {awaiting && <Menu.Label>{awaiting} · starting…</Menu.Label>}
        {missing && !awaiting && (
          <Menu.Label c="var(--warn)">
            no environment named {missing} here — showing {selected?.name ?? "nothing"}
          </Menu.Label>
        )}
        {orderedRuns.map((r) => {
          const from = startOriginLabel(r.started_from, props.presets);
          return (
            <Menu.Item
              key={r.name}
              onClick={() => props.onSelect(r.name)}
              leftSection={<span className={`dot ${runStatus(r)}`} />}
            >
              <div className="run-item-name">
                {r.name}
                {r.name === selected?.name ? " ·" : ""}
              </div>
              <div className="run-item-meta">
                {r.status}
                {from ? ` · from ${from}` : ""}
              </div>
            </Menu.Item>
          );
        })}
        {hidden > 0 && (
          <Menu.Item
            leftSection={<IconHistory size={13} />}
            closeMenuOnClick={false}
            onClick={(e) => {
              e.stopPropagation();
              setShowEnded(true);
            }}
          >
            <div className="run-item-meta">
              Show {hidden} ended environment{hidden === 1 ? "" : "s"}
            </div>
          </Menu.Item>
        )}
        {/* The only surface that can start a SECOND environment: ▶ is a toggle, so
            it shows ■ whenever the bound run is live. It names the environment it
            will create before creating it — a run appearing under a name nobody
            typed is the confusion this whole control exists to remove.

            Absent rather than disabled when it does not apply (nothing live, or
            nothing startable): with the directory stopped, ▶ *is* the start
            affordance, and a greyed second one only invites the question of how it
            differs. */}
        {props.canStartAnother && (
          <>
            <Menu.Divider />
            <Menu.Item
              leftSection={<IconPlayerPlayFilled size={12} />}
              onClick={props.onStartAnother}
            >
              <div className="run-item-name">Start another run</div>
              <div className="run-item-meta">
                named {props.startName} · {props.startLabel}
              </div>
            </Menu.Item>
          </>
        )}
      </Menu.Dropdown>
    </Menu>
  );
}

/**
 * The top-bar door to the running run's node actions.
 *
 * A menu (the bar's one compact icon) rather than inline buttons: the bar is
 * the densest row in the app, and node actions belong to the run's nodes, which
 * may be several. The menu's content is `NodeActions`, and since the new-pane
 * chooser stopped embedding those buttons this is the only place they live.
 */
function NodeActionsButton(props: {
  run: RunRef | null;
  nodes: NodeRow[];
  onChanged: () => void;
  /** No run to act on — shown greyed (or hidden, see `ui.hideDisabledActions`). */
  disabled: boolean;
}) {
  if (props.disabled) {
    // The `<span>` is load-bearing: a disabled ActionIcon has `pointer-events:
    // none`, so the tooltip that explains *why* it is disabled would never open
    // (the #205 trap). The wrapper still receives the hover.
    return (
      <Tooltip label="Start the run to act on its nodes">
        <span style={{ display: "inline-flex" }}>
          <ActionIcon size="md" variant="default" aria-label="Node actions" disabled>
            <IconTools size={14} />
          </ActionIcon>
        </span>
      </Tooltip>
    );
  }
  // Enabled implies a run is set (`disabled === !nodeActionProps`), so the guard
  // below is defensive rather than reachable — it exists to keep the cast-free
  // path through `NodeActions` rather than a `run as RunRef` that a future
  // caller could trip.
  if (!props.run) return null;
  return (
    <Menu position="bottom-start" width={280} withinPortal>
      <Menu.Target>
        <Tooltip label="Node actions">
          <ActionIcon size="md" variant="default" aria-label="Node actions">
            <IconTools size={14} />
          </ActionIcon>
        </Tooltip>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Label>Actions on the running run</Menu.Label>
        <div className="node-actions-menu">
          <NodeActions run={props.run} nodes={props.nodes} onChanged={props.onChanged} />
        </div>
      </Menu.Dropdown>
    </Menu>
  );
}

/**
 * The project menu: which project this window is in, what the others have to say,
 * and every action that belongs to a project.
 *
 * # Shape
 *
 * *Import* on top, then one row per project. A project row is a `Menu.Sub` whose
 * submenu holds that project's actions — open, open in a second view, move up,
 * move down, remove. That is the whole project surface in one control; the old
 * separate `⋯` menu beside it is gone, its *Import* moved here, its *Remove* moved
 * into the per-project submenu, and its *What's new* moved to the bar's own
 * overflow menu, which is where things that belong to Veld rather than to a
 * project now live.
 *
 * **Move up / move down are the keyboard's reorder**, and the column's drag is the
 * mouse's. Both write the same daemon-held order (`repos.sort_position`), which is
 * what ⌘1…⌘9 address — so a reorder from either one moves the shortcuts too, in
 * every window. The rail's lane menu carries the identical pair for the identical
 * reason.
 *
 * # `Menu.Sub` and the scroll cap cannot coexist here
 *
 * This dropdown deliberately has **no** `max-height`/`overflow`. A submenu is
 * positioned and clipped by its scrolling ancestor, so an earlier version of this
 * menu rendered every submenu on top of its own parent with a scrollbar on each.
 * The cost is that a machine with a great many projects gets a tall menu; the
 * column, ⌘1…⌘9 and ⌘K all scale past that, and a broken submenu does not.
 *
 * A project row does not itself switch projects, because `Menu.Sub.Item` has no
 * `onClick` — Mantine's, clicking it opens the submenu. *Open* is the first entry
 * inside, where it reads as one action among the project's others.
 */
function ProjectMenu(props: {
  repos: Repo[];
  repo: Repo | null;
  /** `activity.showWorking` — the rail's setting, honoured here so one glyph
   *  vocabulary means one thing in both places. */
  showWorking: boolean;
  /** The daemon's claims table, minus this client's own rows. */
  elsewhere: ReadonlyMap<number, ClientInfo>;
  /** Whether a second view can be opened at all. */
  canOpenWindow: boolean;
  /** "Open in a new window" (Veld Desktop) or "Open in a new tab" (a browser). */
  secondViewLabel: string;
  onSelectRepo: (root: string) => void;
  onOpenProjectWindow: (root: string) => void;
  onMoveProject: (from: number, to: number) => void;
  onRemoveRepo: (repo: Repo) => void;
  onImport: () => void;
}) {
  const { repo } = props;
  const label = repo
    ? repo.available
      ? repo.name
      : `${repo.name} (unavailable)`
    : "Switch project";
  /**
   * News in a project that is not the one on screen.
   *
   * The bar's answer to "how would I know my other project needs me" when the
   * column is off — which is the default. `isProjectNews` is what keeps it from
   * being lit permanently; see its docstring for why `working` does not count here
   * while it does on the rows inside.
   */
  const elsewhereNews = isProjectNews(
    inbox.groupState(
      otherProjectWorktreeIds(props.repos, repo?.root ?? null),
      props.showWorking,
    ).state,
  );
  return (
    <Menu position="bottom-start" width={250} trigger="click">
      <Menu.Target>
        <Button
          size="xs"
          variant="default"
          className="project-select-btn"
          rightSection={<IconChevronDown size={13} />}
          title={
            elsewhereNews ? `${label} – another project has news` : label || "Switch project"
          }
        >
          <span className="project-select-name">{label}</span>
          {elsewhereNews && <span className="project-actions-dot" aria-hidden="true" />}
        </Button>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Item leftSection={<IconFolderPlus size={14} />} onClick={props.onImport}>
          Import repository…
        </Menu.Item>
        <Menu.Divider />
        {props.repos.map((r, index) => {
          const summary = inbox.groupState(projectWorktreeIds(r), props.showWorking);
          const holder = projectHolder(r, props.elsewhere);
          const digit = index < MAX_PROJECT_SHORTCUTS ? index + 1 : null;
          return (
            <Menu.Sub key={r.root}>
              <Menu.Sub.Target>
                <Menu.Sub.Item
                  className={r.root === repo?.root ? "project-menu-current" : undefined}
                  // Which project this window is in, for a screen reader. Font weight
                  // is the visual signal and carries nothing to assistive tech; the
                  // `Select` this replaced got `aria-selected` from its options for
                  // free. `aria-current`, not `aria-selected`: the row is not part of
                  // a selection widget, and this is "the one you are on".
                  aria-current={r.root === repo?.root ? true : undefined}
                  // **`undefined`, not an element that renders null.** Mantine
                  // gates the section on `leftSection &&` (MenuItem.mjs:93), and a
                  // React element is truthy even when the component returns null —
                  // so passing `<InboxIcon/>` unconditionally rendered an empty
                  // gutter div with its own margin on every quiet row.
                  leftSection={
                    summary.state ? <InboxIcon summary={summary} label={r.name} /> : undefined
                  }
                >
                  <span className="project-menu-row">
                    <span className="project-menu-name">
                      {r.available ? r.name : `${r.name} (unavailable)`}
                    </span>
                    {/* Where the project already is, or which key goes there. The
                        away note wins: it changes what the click will do, where the
                        digit only says there is a faster way to do it. */}
                    {holder ? (
                      <span className="project-menu-away">{awayNote(holder)}</span>
                    ) : (
                      digit && <span className="project-menu-key">⌘{digit}</span>
                    )}
                  </span>
                </Menu.Sub.Item>
              </Menu.Sub.Target>
              <Menu.Sub.Dropdown>
                <Menu.Item
                  leftSection={<IconArrowsExchange size={14} />}
                  onClick={() => props.onSelectRepo(r.root)}
                >
                  Open
                </Menu.Item>
                {props.canOpenWindow && (
                  <Menu.Item
                    // The same glyph the rail's own "Open in a new window" uses —
                    // one action, one icon, wherever it is offered from.
                    leftSection={<IconExternalLink size={14} />}
                    onClick={() => props.onOpenProjectWindow(r.root)}
                  >
                    {props.secondViewLabel}
                  </Menu.Item>
                )}
                <Menu.Divider />
                {/* Disabled at the ends rather than hidden, so the pair keeps the
                    same two positions in every project's submenu — a menu whose
                    items move depending on which row you opened is one you have to
                    read every time. */}
                <Menu.Item
                  leftSection={<IconChevronUp size={14} />}
                  disabled={index === 0}
                  onClick={() => props.onMoveProject(index, index - 1)}
                >
                  Move up
                </Menu.Item>
                <Menu.Item
                  leftSection={<IconChevronDown size={14} />}
                  disabled={index === props.repos.length - 1}
                  onClick={() => props.onMoveProject(index, index + 1)}
                >
                  Move down
                </Menu.Item>
                <Menu.Divider />
                <Menu.Item
                  color="red"
                  leftSection={<IconTrash size={14} />}
                  onClick={() => props.onRemoveRepo(r)}
                >
                  Remove project…
                </Menu.Item>
              </Menu.Sub.Dropdown>
            </Menu.Sub>
          );
        })}
      </Menu.Dropdown>
    </Menu>
  );
}

function TopBar(props: {
  modeSwitch: React.ReactNode;
  repos: Repo[];
  repo: Repo | null;
  worktree: Worktree | null;
  /** The repo's main-checkout staleness, or `null` before the first CSRF-gated
   *  refresh computed it. */
  gitStatus: RepoGitStatus | null;
  /** One-click "update main": fetch + fast-forward (human-initiated only). */
  updateMain: () => void;
  updatingMain: boolean;
  startConfig: React.ReactNode;
  canStart: boolean;
  running: boolean;
  pending: PendingAction | null;
  /** What the play/stop control spins for — `pending` widened by the observed
   *  transition, so this button and the rail's row control cannot disagree.
   *  `pending` stays separate because `disabled` and the restart button key on it:
   *  only an action *this* window fired may lock the controls. */
  spinner: PendingAction | null;
  run: { name: string; status: string } | null;
  /**
   * The run selector, or `null` when this worktree has no run controls at all.
   *
   * Built by the app because it needs the worktree's presets and the whole run
   * list. It *replaces* the old status dot — it carries one — so exactly one
   * control in the bar answers "which run, and how is it".
   */
  runSelect: React.ReactNode;
  /** Whether the run selector has no runs to offer — hidden (or greyed per
   *  `ui.hideDisabledActions`). */
  runSelectDisabled: boolean;
  urls: Array<[string, string]>;
  /** The Sharing surface, built by the app (it owns the shares poll). */
  sharing: React.ReactNode;
  /** Open a pane on the run's URLs. Absent when there is no layout to open into. */
  onShowBlankBrowser: (() => void) | undefined;
  /** `activity.showWorking`, passed through to the selector's activity glyphs. */
  showWorking: boolean;
  /** The daemon's claims table minus this client's rows — what the selector says
   *  about a project some other window already has a checkout of. */
  elsewhere: ReadonlyMap<number, ClientInfo>;
  /** Whether a second view of a project can be opened — a window in Veld Desktop,
   *  a tab in a browser. */
  canOpenWindow: boolean;
  /** What to call that second view, since the two environments differ. */
  secondViewLabel: string;
  /** The show/hide toggle for the project column, built by the app (it owns the
   *  settings write). Rendered between the mode switch and the selector. */
  projectColumnButton: React.ReactNode;
  onSelectRepo: (root: string) => void;
  onOpenProjectWindow: (root: string) => void;
  /** Move a project one place in the daemon-held order — the keyboard's half of
   *  the column's drag. */
  onMoveProject: (from: number, to: number) => void;
  onRemoveRepo: (repo: Repo) => void;
  onImport: () => void;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
  /** Search, keep-awake and focus mode — see `TopBarControls`, mounted by both
   *  modes so the two bars cannot drift in what they offer. */
  controls: React.ReactNode;
  /** Theme, what's new and settings, as one menu at the end of the bar. */
  overflowMenu: React.ReactNode;
  configVarsButton: React.ReactNode;
  /** Node actions for the currently-running run, or `null` when none can fire. */
  nodeActions: React.ReactNode;
  /** `ui.hideDisabledActions` — hide an inapplicable action, or show it greyed. */
  hideDisabled: boolean;
  /** The project's `ide.extensions` declarations for this worktree, commands
   *  already stripped by the daemon. */
  extensions: ExtensionSpec[];
  /** Open a URL in a browser pane — for a status badge whose link asks for
   *  `open_in: "pane"`. Absent when there is no layout to open into. */
  onOpenInPane: ((url: string) => void) | undefined;
}) {
  const { worktree, run } = props;
  // One poll for the whole slot, not one per cluster: this component renders
  // `TopBarExtensions` twice (start and end) and the response covers every badge
  // in the worktree either way. See `useExtensionStatus`.
  const extensionStatus = useExtensionStatus(worktree?.id ?? null, props.extensions);
  const repoAvailable = props.repo?.available ?? false;
  // Whether the play/stop control can *abort* a start it is in the middle of —
  // see the button below. Hover-owned so a fresh mount does not replay the
  // transition, the same reason the mode switch owns its own hover state.
  const [startHover, setStartHover] = useState(false);
  // A start in flight: the play/stop button spins red and offers a stop on hover.
  const starting = props.spinner === "start";
  // The project selector shrinks to its current name rather than occupying a fixed
  // cap. It is a `Button` now (see `ProjectMenu`), so that is the intrinsic width
  // with a `max-width` backstop in CSS — the hidden mirror span this used to
  // measure went with the `Select` it was sizing.
  // No run controls for a repo we can't see on disk — git/veld actions would
  // only fail later with a worse error.
  const canRun = !!worktree?.has_veld_config && repoAvailable;
  return (
    <div className={topbarClass}>
      {props.modeSwitch}
      {/* Only with something to show. At zero projects the bar is one instruction
          ("import something") and a toggle for an empty column is a second thing
          asking to be understood. */}
      {props.repos.length > 0 && props.projectColumnButton}
      {props.repos.length === 0 ? (
        // Nothing to select between, so the bar offers the only move there is.
        // The selector is *absent* at zero projects rather than empty, which left
        // the import affordance buried in the neighbouring "…" menu — the one
        // control a first-time user has no reason to open.
        //
        // Neutral, not the primary green: the start screen below carries the real
        // call to action, and two green buttons on one screen make the wrong one
        // look like the point.
        <Button
          size="xs"
          variant="default"
          leftSection={<IconFolderPlus size={14} />}
          onClick={props.onImport}
        >
          Import first project
        </Button>
      ) : (
        <ProjectMenu
          repos={props.repos}
          repo={props.repo}
          showWorking={props.showWorking}
          elsewhere={props.elsewhere}
          canOpenWindow={props.canOpenWindow}
          secondViewLabel={props.secondViewLabel}
          onSelectRepo={props.onSelectRepo}
          onOpenProjectWindow={props.onOpenProjectWindow}
          onMoveProject={props.onMoveProject}
          onRemoveRepo={props.onRemoveRepo}
          onImport={props.onImport}
        />
      )}
      {worktree && (
        <>
          <div className="sep" />
          {/* The repo's staleness + one-click update. Beside the run controls,
              not over with search/settings: it is a *project* action, and the
              bar reads "what this project needs, what I'll run, run it".

              Shown only when the daemon could compute a count — a repo with no
              remote has no origin refs, and a permanently-greyed "update" is
              noise. Behind: enabled with the count visible. Up to date: an
              inapplicable action, hidden per `ui.hideDisabledActions` or shown
              greyed with a reason when that is off — exactly like restart.

              The `<span>` wrapper is load-bearing: a disabled Mantine control
              has `pointer-events: none`, so the tooltip explaining *why* it is
              disabled would never open — the #205 trap. */}
          {props.gitStatus &&
            props.gitStatus.behind !== null &&
            (props.gitStatus.behind > 0 || !props.hideDisabled) && (
              <span className="git-sync-btn">
                <Tooltip
                  label={
                    props.updatingMain
                      ? "Updating…"
                      : props.gitStatus.behind > 0
                        ? `${props.gitStatus.default_branch ?? "main"} is ${
                            props.gitStatus.behind
                          } ${
                            props.gitStatus.behind === 1 ? "commit" : "commits"
                          } behind origin — click to fast-forward`
                        : "Up to date"
                  }
                >
                  <ActionIcon
                    size="md"
                    variant="default"
                    loading={props.updatingMain}
                    disabled={props.gitStatus.behind === 0 || props.updatingMain}
                    onClick={props.updateMain}
                    aria-label="Update main"
                  >
                    <IconRefreshDot size={14} />
                  </ActionIcon>
                </Tooltip>
                {/* A sibling of the button, not a child: the ActionIcon clips its
                    own overflow, and a pill that straddles the button's corner was
                    cut off by its border. Positioned against the `.git-sync-btn`
                    wrapper instead, so it is fully visible. Colour is the severity
                    curve (green→orange→red) from how far and how long the main
                    checkout has drifted. */}
                {props.gitStatus.behind > 0 && (
                  <span
                    className="git-sync-badge"
                    aria-hidden="true"
                    style={{
                      background: `hsl(${stalenessHue(
                        props.gitStatus.behind,
                        props.gitStatus.latest_commit != null
                          ? Math.max(0, Date.now() / 1000 - props.gitStatus.latest_commit)
                          : 0,
                        // The selected worktree's project config tunes how
                        // sensitive the colouring is (`ide.git.stalenessSensitivity`).
                        props.worktree?.ide.staleness_sensitivity ?? 1,
                      )} 70% 45%)`,
                    }}
                  >
                    {props.gitStatus.behind > 99 ? "99+" : props.gitStatus.behind}
                  </span>
                )}
              </span>
            )}
          {canRun && props.startConfig}
          {/* The runs button sits between the preset picker and the start control
              — the bar reads "what I'll run, which run, run it". Disabled (or
              hidden, per `ui.hideDisabledActions`) when the worktree has no runs
              to choose from. */}
          {(!props.hideDisabled || !props.runSelectDisabled) && props.runSelect}
          {canRun && (
            <>
              {/* The spinner belongs on the button that was pressed. Putting
                  it on play/stop for every action made a restart look like a
                  stop in progress. */}
              {/* The run's name is in the label, not only in a tooltip on the
                  selector: ■ ends processes, and which run it ends has to be
                  unmistakable at the instant it is clicked, not one hover away. */}
              <Tooltip
                label={
                  starting
                    ? "Starting… click to abort"
                    : props.running
                      ? `Stop ${props.run?.name ?? "run"}`
                      : "Start run"
                }
              >
                {/* The pointer listeners sit on a wrapper, not the ActionIcon:
                    Mantine's Tooltip merges child handlers through floating-ui,
                    and the `starting` swap needs them even while the loader shows.
                    The wrapper is what makes a *starting* run's stop-on-hover
                    reachable without the button being enabled at rest. */}
                <span
                  className="bar-hover-slot"
                  onMouseEnter={() => setStartHover(true)}
                  onMouseLeave={() => setStartHover(false)}
                >
                  <ActionIcon
                    size="md"
                    variant="light"
                    color={starting ? "red" : props.running ? "red" : "green"}
                    // `spinner`, not `pending`: the rail's row control spins for a
                    // transition it merely *observed* (one started from the CLI or
                    // another window), and this button showing a static glyph for
                    // the same worktree at the same moment is two surfaces
                    // disagreeing about whether anything is happening.
                    //
                    // Still filtered to start/stop rather than truthiness, which is
                    // what keeps the comment above true: a locally-fired restart
                    // spins the restart button alone. An externally-fired one is
                    // indistinguishable from a stop-then-start on the wire, so it
                    // legitimately lands here instead.
                    //
                    // A *starting* run spins red and, on hover, offers a stop — so
                    // while a start is in flight this control is deliberately NOT
                    // disabled: the hover swap replaces the loader with a stop
                    // glyph, and clicking it aborts the start.
                    loading={
                      (props.spinner === "start" || props.spinner === "stop") &&
                      !(starting && startHover)
                    }
                    disabled={
                      (props.pending !== null && !starting) ||
                      (!props.running && !props.canStart && !starting)
                    }
                    onClick={props.running || starting ? props.onStop : props.onStart}
                  >
                    {starting && startHover ? (
                      <IconX size={14} />
                    ) : props.running ? (
                      <IconPlayerStopFilled size={13} />
                    ) : (
                      <IconPlayerPlayFilled size={13} />
                    )}
                  </ActionIcon>
                </span>
              </Tooltip>
              {/* A restart only makes sense while something is live. Hidden (or,
                  with `ui.hideDisabledActions` off, shown greyed) when nothing
                  is — a refresh glyph beside a stopped ▶ reads as a second start. */}
              {(!props.hideDisabled || props.running) && (
                <Tooltip label={`Restart ${props.run?.name ?? "run"}`}>
                  <ActionIcon
                    size="md"
                    variant="default"
                    loading={props.pending === "restart"}
                    disabled={!props.running || props.pending !== null}
                    onClick={props.onRestart}
                  >
                    <IconReload size={13} />
                  </ActionIcon>
                </Tooltip>
              )}
              {/* Beside the start controls, not over with search/settings/theme.
                  The bar has two clusters — what this *project* does on the left,
                  what the *app* does on the right — and a machine var is squarely
                  the first: it belongs to the selected worktree and changes what
                  ▶ will actually run. Parked on the right it read as a second
                  settings entry, which is the exact confusion the `{}` glyph was
                  chosen to avoid. */}
              {props.configVarsButton}
              {/* The running run's node actions, one click from the surface that
                  is always up — see shared/NodeActions.tsx. Hidden when nothing
                  can fire and `ui.hideDisabledActions` is on; shown greyed with a
                  reason when off. */}
              {props.nodeActions}
              {run && (props.hideDisabled ? props.urls.length > 0 : true) && (
                // Opens a browser pane on the run's URLs, not an overlay of its
                // own: the URLs live in whichever pane is about to need them, and
                // a modal listing them was a second, inconsistent surface that
                // also covered the panes it was talking about.
                //
                // Icon-only: the count used to sit in the button, but it changed
                // the bar's width every time the run gained a URL, and the bar is
                // the densest row in the app. The URLs themselves are one click
                // away, and the tooltip names what they open.
                <Tooltip label="Open the run's URLs in a pane">
                  <ActionIcon
                    size="md"
                    variant="default"
                    aria-label="Open the run's URLs in a pane"
                    disabled={!props.onShowBlankBrowser || props.urls.length === 0}
                    onClick={props.onShowBlankBrowser}
                  >
                    <IconWorld size={14} />
                  </ActionIcon>
                </Tooltip>
              )}
              {props.sharing}
              {/* The project's own badges and buttons, last in the left cluster —
                  after every veld-owned control, sharing included, so a project
                  cannot push veld's own tools out of their fixed order as more
                  extensions are declared. `align: "end"` moves an entry over to
                  the app cluster instead — see the second instance at the end of
                  the bar. */}
              <TopBarExtensions
                extensions={props.extensions}
                align="start"
                worktreeId={worktree?.id ?? null}
                status={extensionStatus}
                onOpenInPane={props.onOpenInPane}
              />
            </>
          )}
          {!canRun && (
            <span
              className="chip"
              style={!repoAvailable ? { color: "var(--warn)" } : undefined}
              title={
                repoAvailable
                  ? "No veld.json in this worktree"
                  : "Repository directory not found on disk — showing last known state"
              }
            >
              {repoAvailable ? "no veld config" : "repository unavailable"}
            </span>
          )}
        </>
      )}
      <div style={{ flex: 1 }} />
      {worktree && (
        <TopBarExtensions
          extensions={props.extensions}
          align="end"
          worktreeId={worktree.id}
          status={extensionStatus}
          onOpenInPane={props.onOpenInPane}
        />
      )}
      {/* Search, keep-awake and focus mode, as one component both modes mount —
          see `TopBarControls`. They used to be three IDE-only controls here;
          keep-awake being armable by a live share is what forced the change,
          since Runs mode is a screen where sharing happens and would otherwise
          hold the machine awake with nothing on it saying so. */}
      {props.controls}
      {/* Last, and the only thing after focus mode: everything Veld-level (theme,
          what's new, settings) is inside it. Project actions live in the project
          menu at the *start* of the bar, which is what this split bought. */}
      {props.overflowMenu}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Worktree rail
// ---------------------------------------------------------------------------

/**
 * Mantine colour for an in-flight action: what it is doing, not where it ends
 * up — "stop" is red while it runs, restart reads as a (re)start.
 *
 * Exhaustive on purpose. Widening [`PendingAction`] (to cover share actions,
 * say) must fail the build here rather than silently fall through to green —
 * the same class of bug as the literal comparisons in `TopBar`.
 */
function actionColor(label: PendingAction): string {
  switch (label) {
    case "stop":
      return "red";
    case "start":
    case "restart":
      return "green";
    default: {
      const exhaustive: never = label;
      return exhaustive;
    }
  }
}

/**
 * Rail width bounds, in px.
 *
 * The minimum is well clear of the collapsed rail's 64px on purpose: the
 * collapsed rail is a **mode** (it hides the alias, the branch and the inline run
 * control), not a narrow width, so dragging must never slide into it. Crossing
 * that line by drag would silently drop three columns without saying so, and the
 * user would have no way back except finding the chevron.
 */
const RAIL_MIN_WIDTH = 180;
const RAIL_MAX_WIDTH = 480;
const RAIL_DEFAULT_WIDTH = 236;

/** Parse a stored rail width, clamped; the default for anything unusable. */
function railWidth(raw: string): number {
  const n = Number.parseInt(raw, 10);
  if (!Number.isFinite(n)) return RAIL_DEFAULT_WIDTH;
  return Math.max(RAIL_MIN_WIDTH, Math.min(RAIL_MAX_WIDTH, n));
}

/**
 * The rail's drag-to-resize edge.
 *
 * Pointer events with capture rather than mouse events: capture keeps the drag
 * alive when the pointer crosses a pane's native `WebContentsView`, which does
 * not forward mouse events to the page above it (#188). Without it a drag that
 * strayed right froze at the pane boundary.
 */
function RailResizer(props: {
  width: number;
  onWidth: (w: number) => void;
  /** Told when a drag starts and ends, so the rail can suspend its width
   *  transition — see `.rail.resizing`. */
  onDragging: (active: boolean) => void;
}) {
  const drag = useRef<{ startX: number; startWidth: number } | null>(null);
  return (
    <div
      className="rail-resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the worktree rail"
      aria-valuenow={props.width}
      aria-valuemin={RAIL_MIN_WIDTH}
      aria-valuemax={RAIL_MAX_WIDTH}
      tabIndex={0}
      onPointerDown={(e) => {
        drag.current = { startX: e.clientX, startWidth: props.width };
        props.onDragging(true);
        e.currentTarget.setPointerCapture(e.pointerId);
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d) return;
        props.onWidth(
          Math.max(
            RAIL_MIN_WIDTH,
            Math.min(RAIL_MAX_WIDTH, d.startWidth + (e.clientX - d.startX)),
          ),
        );
      }}
      onPointerUp={(e) => {
        drag.current = null;
        props.onDragging(false);
        e.currentTarget.releasePointerCapture(e.pointerId);
      }}
      // A capture that ends without a pointerup (the pointer is cancelled by the
      // OS, another element steals capture) must still clear the flag, or the rail
      // keeps its transition suppressed until the next drag.
      onPointerCancel={() => {
        drag.current = null;
        props.onDragging(false);
      }}
      onLostPointerCapture={() => {
        drag.current = null;
        props.onDragging(false);
      }}
      // Keyboard resize, because a separator that only responds to a pointer is
      // unreachable for anyone who cannot make a 200px drag.
      onKeyDown={(e) => {
        const step = e.shiftKey ? 32 : 8;
        if (e.key === "ArrowLeft") {
          e.preventDefault();
          props.onWidth(Math.max(RAIL_MIN_WIDTH, props.width - step));
        } else if (e.key === "ArrowRight") {
          e.preventDefault();
          props.onWidth(Math.min(RAIL_MAX_WIDTH, props.width + step));
        }
      }}
    />
  );
}

/** Whether a drag is over the lower half of the row it is on. */
function below(e: React.DragEvent): boolean {
  const box = e.currentTarget.getBoundingClientRect();
  return e.clientY > box.top + box.height / 2;
}

/**
 * The rail's insertion caret — the same 3px glowing bar the pane tab strip uses,
 * turned horizontal.
 *
 * **A sibling element, not a pseudo-element on the row.** `.wt-row` carries
 * `overflow: hidden` with an 8px `border-radius`, and overflow clips pseudo-elements
 * too — so a caret drawn on the row was clipped to the row's rounded rect, which is
 * exactly what it looked like: a highlighted row border rather than a gap opening
 * between two rows. Rendering it as its own element puts it outside every row's
 * clipping context, and makes "after the last row" the same code path as any other
 * position instead of a second special case.
 *
 * Contributes no height: `height: 0` plus an absolutely positioned bar. The
 * negative margin cancels the one extra flex `gap` that inserting an element into
 * the column would otherwise add — the list must not shift under the pointer
 * mid-drag.
 */
function RailCaret() {
  return <div className="rail-caret" aria-hidden />;
}

/** The rail's drop indicator, sized for the project column's squares. Same
 *  accent bar and same glow — one drag vocabulary for both columns. */
function ProjectCaret() {
  return <div className="project-caret" aria-hidden />;
}

/**
 * The project column: every project as one square, beside the worktree rail.
 *
 * # Why a column and not more menu
 *
 * A menu answers a question you already knew to ask. The thing this feature exists
 * for is the question you *didn't* — an agent waiting in the project you are not
 * looking at — and that only works if the answer is on screen without a click. So
 * the activity glyph the rail puts on a worktree is aggregated here per project and
 * rendered permanently, and switching costs one click instead of two.
 *
 * **Off by default** (`ui.showProjectColumn`), because most installs have one
 * project and a column of one square answers nothing. The top-bar toggle is the
 * discovery path that costs us; see the setting's own docstring.
 *
 * # Identity without a migration
 *
 * Projects have no marker of their own and are not getting one: the square is
 * `projectInitials(name)` and nothing else, so an import needs no picker and there is
 * no per-project marker column to add, migrate, or keep in step with a rename.
 *
 * **Greyscale, deliberately.** An earlier version filled each square with a hue
 * derived from the repo root. It was louder than anything else on screen and it
 * competed with the worktree markers two pixels to its right — the one place in this
 * UI where colour already carries a specific meaning. Selection is fill-vs-outline,
 * the same signal a rail row uses, and the only colour left in the column is the
 * activity badge. See the matching note in `styles.css`.
 *
 * # Order
 *
 * Dragging a square rewrites the order for **every** window, because it is the
 * daemon's (`repos.sort_position`, schema v16) rather than this page's. That is not
 * incidental: ⌘1…⌘9 address a *position*, so a per-window order would make one chord
 * mean two projects. The drag mirrors the rail's own lane drag — same dataTransfer
 * discipline, same "drop on a row, land at its index" gesture.
 */
function ProjectColumn(props: {
  repos: Repo[];
  activeRoot: string | null;
  showWorking: boolean;
  elsewhere: ReadonlyMap<number, ClientInfo>;
  onSelect: (root: string) => void;
  onReorder: (from: number, to: number) => void;
  onMenu: (e: React.MouseEvent, repo: Repo) => void;
  onImport: () => void;
}) {
  // Which square is being dragged, and where the caret is. Both are this
  // component's alone — the rail's two drags are keyed on their own state for the
  // reason its `onDragStart` explains, and a third one must not join that
  // arrangement by sharing any of it.
  //
  // `dropAt` is a caret position (`0…length`, "insert before this index"), not the
  // index of a square — see `dropTargetIndex` for why the two are not the same
  // number.
  const [dragRoot, setDragRoot] = useState<string | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null);
  const roots = props.repos.map((r) => r.root);
  const endDrag = () => {
    setDragRoot(null);
    setDropAt(null);
  };
  /** Commit whatever the caret is pointing at. Shared by every square's `onDrop`
   *  and the column's own, so a release in the padding below the last square lands
   *  the same way a release on a square does. */
  const commitDrop = () => {
    const from = roots.indexOf(dragRoot ?? "");
    const at = dropAt;
    endDrag();
    if (from === -1 || at === null) return;
    const to = dropTargetIndex(from, at);
    if (to === null) return;
    props.onReorder(from, to);
  };
  return (
    <div
      className="project-col"
      role="tablist"
      aria-label="Projects"
      aria-orientation="vertical"
      // The column itself accepts the drop, so a release in the padding under the
      // last square is a drop at the end rather than a cancelled drag. Without it
      // the only way to reach the last position is to hit the bottom half of the
      // final square exactly.
      onDragOver={(e) => {
        if (!dragRoot) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
      }}
      onDrop={(e) => {
        if (!dragRoot) return;
        e.preventDefault();
        commitDrop();
      }}
    >
      {props.repos.map((r, index) => {
        const summary = inbox.groupState(projectWorktreeIds(r), props.showWorking);
        const holder = projectHolder(r, props.elsewhere);
        const active = r.root === props.activeRoot;
        // Only the first nine are addressable; the tooltip must not promise a chord
        // that does nothing. See `MAX_PROJECT_SHORTCUTS`.
        const digit = index < MAX_PROJECT_SHORTCUTS ? index + 1 : null;
        const note = [
          r.available ? null : "unavailable",
          holder ? awayNote(holder) : null,
          digit ? `⌘${digit}` : null,
        ]
          .filter(Boolean)
          .join(" · ");
        return (
          <Fragment key={r.root}>
            {dropAt === index && <ProjectCaret />}
            <Tooltip
              label={note ? `${r.name} – ${note}` : r.name}
              position="right"
              withArrow
            >
            {/* A div with role=tab, not a <button>, and the reason is the drag: a
                native button consumes the mousedown for its own activation
                behaviour, so `draggable` on one never starts a drag in Chromium.
                The rail's own rows are divs for a related reason (nested controls)
                — see the note there. `onKeyDown` restores the Enter/Space a real
                button would have had. */}
            <div
              role="tab"
              tabIndex={0}
              aria-selected={active}
              className={`project-sq${active ? " active" : ""}${
                r.available ? "" : " unavailable"
              }${dragRoot === r.root ? " dragging" : ""}`}
              onKeyDown={(e) => {
                if (e.key !== "Enter" && e.key !== " ") return;
                e.preventDefault();
                props.onSelect(r.root);
              }}
              draggable
              onDragStart={(e) => {
                setDragRoot(r.root);
                e.dataTransfer.effectAllowed = "move";
                // Firefox ignores a drag with no payload. Prefixed like the rail's
                // lane drag, so anything outside this column that reads the plain
                // text can tell what it is being offered.
                e.dataTransfer.setData("text/plain", `project:${r.root}`);
              }}
              onDragOver={(e) => {
                if (!dragRoot) return;
                // Only for a drag this column started. Without the guard a worktree
                // or a lane dragged out of the rail would paint a drop indicator on
                // a target that cannot accept it.
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                // Above the midpoint inserts before this square, below it after —
                // the gesture the rail already uses, and the reason the indicator
                // is a caret between squares rather than a ring around one: a ring
                // cannot say which side of the target you are about to land on.
                const box = e.currentTarget.getBoundingClientRect();
                setDropAt(e.clientY < box.top + box.height / 2 ? index : index + 1);
              }}
              onDrop={(e) => {
                e.preventDefault();
                // The column below also accepts drops (so a release in the padding
                // lands at the end), and without this the event bubbled into it
                // with the pre-`endDrag` closure still live — every drop on a square
                // fired `commitDrop` twice, i.e. two identical POSTs and two polls
                // per drag.
                e.stopPropagation();
                commitDrop();
              }}
              onDragEnd={endDrag}
              onClick={() => props.onSelect(r.root)}
              onContextMenu={(e) => props.onMenu(e, r)}
            >
              <span className="project-sq-initials" aria-hidden="true">
                {projectInitials(r.name)}
              </span>
              {/* The accessible name, since the initials are decorative and the
                  tooltip is not read out. */}
              <span className="visually-hidden">{r.name}</span>
              {summary.state && (
                <span className={`project-sq-badge ${summary.state}`} aria-hidden="true" />
              )}
              {holder && <span className="project-sq-away" aria-hidden="true" />}
            </div>
            </Tooltip>
          </Fragment>
        );
      })}
      {/* The caret past the last square — "drop at the end". */}
      {dropAt === props.repos.length && <ProjectCaret />}
      <Tooltip label="Import repository…" position="right" withArrow>
        <button
          type="button"
          className="project-sq project-sq-add"
          onClick={props.onImport}
          aria-label="Import repository"
        >
          <IconPlus size={14} />
        </button>
      </Tooltip>
    </div>
  );
}

function Rail(props: {
  worktrees: Worktree[];
  lanes: Lane[];
  active: Worktree | null;
  envs: EnvironmentList | null;
  /** Drives which marker face the rows render; `null` before the first read. */
  settings: SettingsDoc | null;
  wide: boolean;
  width: number;
  canRun: (w: Worktree) => boolean;
  canStart: (w: Worktree) => boolean;
  pendingFor: (w: Worktree) => PendingAction | null;
  /** Worktrees another client is showing, and which one has each. Clicking one
   *  goes there instead of opening it here, so it is marked — and named, because
   *  a desktop window is somewhere the click can take you and a browser tab is
   *  not. */
  elsewhere: Map<number, ClientInfo>;
  onToggle: () => void;
  onWidth: (w: number) => void;
  onSelect: (w: Worktree) => void;
  /** Open the create dialog, filing the result into `lane` (`""` = ungrouped). */
  onAdd: (lane: string) => void;
  onMenu: (e: React.MouseEvent, w: Worktree) => void;
  onStart: (w: Worktree) => void;
  onStop: (w: Worktree) => void;
  /** Go to this worktree and show its node health — the attention affordance on a
   *  failed or recovering row. Selects first, so it can be refused like any other
   *  switch when another window holds the worktree. */
  onDiagnose: (w: Worktree) => Promise<void>;
  /** `activity.showWorking` — whether a worktree with something merely *running* in
   *  it gets a glyph, as opposed to only one with unseen news. */
  showWorking: boolean;
  onAddLane: () => void;
  onLaneMenu: (e: React.MouseEvent, lane: string) => void;
  onMove: (path: string, toLane: string, toIndex: number) => void;
  /** Reorder whole lanes by dragging their headers: `lane` takes the place
   *  `onto` holds. Two names, never an index — see `moveLane`. */
  onMoveLane: (lane: string, onto: string) => void;
  onRestore: (w: Worktree) => void;
  onEmptyTrash: () => void;
  /** Move every detached checkout to the trash (revertible) — the Detached
   *  lane's one batch action. */
  onTrashAllDetached: () => void;
  /** Dropping a dragged worktree onto the trash — bins it (revertible), which is
   *  not a lane move. Receives the dragged worktree's path. */
  onTrashDrop: (path: string) => void;
  /** Worktrees this window has optimistically confirmed for permanent deletion
   *  (see `deletingIds`). Folds into the rendering exactly like the daemon's own
   *  `worktree.deleting` flag, so the two sources can't disagree visually. */
  deleting: ReadonlySet<number>;
}) {
  // The daemon's `deleting` flag starts only once `git worktree remove` is
  // running; a row this window confirmed moments ago is folded in here so it
  // enters the terminal lane on the click. Kept separate from `props.worktrees`
  // (which other consumers read) by decorating a copy, not mutating.
  const worktrees = props.worktrees.map((w) =>
    props.deleting.has(w.id) ? { ...w, deleting: true } : w,
  );
  const groups = railGroups(worktrees, props.lanes);
  // The trash and the terminal deleting lane are pinned to the bottom of the
  // rail and never scroll with the rows; everything above them does. Splitting
  // here keeps one render for both halves — `renderGroup` below.
  const dockedKeys = new Set([DELETING_LANE, TRASH_LANE]);
  const scroll = groups.filter((g) => !dockedKeys.has(g.key));
  const docked = groups.filter((g) => dockedKeys.has(g.key));
  // The dock always renders in wide mode — the trash header is the point of an
  // always-visible trash. Collapsed, a group has no header, so an empty dock is a
  // bare bordered strip; only show it there when it actually holds a row.
  const dockVisible = props.wide || docked.some((g) => g.worktrees.length > 0);
  // Drag state is local: it is transient pointer feedback, and lifting it would
  // re-render the pane area on every dragover.
  const [dragPath, setDragPath] = useState<string | null>(null);
  const [dropAt, setDropAt] = useState<{ key: string; index: number } | null>(
    null,
  );
  // The second drag kind: a whole lane by its header. Kept in its own pair of
  // states rather than folded into `dragPath`/`dropAt` with a discriminant,
  // because the two drags have different drop targets and different feedback —
  // and a single "what is being dragged" value made every handler on both sides
  // ask what kind it was before doing anything.
  const [dragLane, setDragLane] = useState<string | null>(null);
  const [laneDropAt, setLaneDropAt] = useState<number | null>(null);
  // Whether the dock, rather than a section in the list, is the thing under the
  // pointer. It resolves to the same last lane either way; what differs is where
  // the bar can be seen, since the last lane's own bar is inside the scroller and
  // the dock is used precisely when that lane is scrolled out of view.
  const [onDock, setOnDock] = useState(false);
  // `dragend` fires on the source node, so the rail's own `onDragEnd` only ever
  // sees a drag whose source is still mounted. A lane renamed in ANOTHER window
  // changes the section's key, React unmounts it, and the event then fires on a
  // detached node and reaches nothing — leaving `dragLane` set for good. That is
  // not cosmetic: the list keeps a live drop zone, so the next unrelated drag
  // over the rail (a file from Finder) would be accepted and reorder a lane
  // nobody grabbed. The window always hears it.
  useEffect(() => {
    if (dragLane === null) return;
    const done = () => endDrag();
    window.addEventListener("dragend", done);
    window.addEventListener("drop", done);
    return () => {
      window.removeEventListener("dragend", done);
      window.removeEventListener("drop", done);
    };
  }, [dragLane]);
  // Positions of the lane sections, by lane name.
  const laneIndex = new Map(props.lanes.map((l, i) => [l.name, i]));
  /**
   * This section's place in the lane order, or `undefined` for a section that
   * holds none — the ungrouped section, the main checkout, and the two pending
   * -removal lanes are neither draggable nor lane drop targets.
   *
   * Keyed on `lane` behind `editable`, never on `key`. The main checkout's key is
   * the literal `"main"` and `"main"` is a legal lane name (`valid_lane_name`
   * rejects only empty, over-long, control characters, `.` and `..`), so keying
   * on it gave that pinned section the position of a lane called `main`: every
   * drop over the top of the rail resolved there instead of to the first lane,
   * and dragging that lane faded the main checkout row as if it were the one
   * being carried. Same trap the header's `aria-label` already documents for
   * `UNGROUPED_LABEL`.
   */
  const laneAtOf = (g: RailGroup) =>
    g.editable ? laneIndex.get(g.lane) : undefined;
  const listRef = useRef<HTMLDivElement>(null);

  /**
   * The lane a pointer at `clientY` is aiming at, or `null` when the rail holds
   * no lanes.
   *
   * Measured against the sections' geometry rather than resolved from the
   * element under the pointer, and that is the whole point. Per-element hit
   * testing made the gesture directional: the 9px gutters, the list's padding
   * and everything below the last lane belonged to no section at all, so "pull
   * it to the bottom and let go" landed on nothing, and a downward drag
   * registered only where a section happened to be under the pointer.
   *
   * What this does **not** remove is the travel a downward move costs. The
   * dragged lane keeps its place and its height while it is carried, so the
   * pointer has to clear its own section's bottom before the lane below is the
   * answer, while the lane above is one pixel past the header it was grabbed by.
   * That is the price of not reflowing the rail under a pointer that is aiming
   * at it; the bar tracks the whole way, so it is visible rather than silent.
   *
   * The DOM read lives here and the choice lives in `laneDropTarget`, which has
   * the tests: this mapping is the part that was wrong in every attempt at the
   * feature, and it is the part a rendered component cannot pin.
   *
   * `data-lane-index` rather than refs because the sections are rendered by a
   * plain map and the count changes as lanes come and go; the query runs on
   * dragover, over a handful of elements.
   */
  const laneTargetAt = (clientY: number): number | null => {
    const list = listRef.current;
    if (!list) return null;
    const sections = [
      ...list.querySelectorAll<HTMLElement>("[data-lane-index]"),
    ].map((el) => ({
      index: Number(el.dataset.laneIndex),
      bottom: el.getBoundingClientRect().bottom,
    }));
    return laneDropTarget(sections, clientY);
  };
  // Suppresses the rail's width transition for the duration of a resize drag. The
  // transition exists for the collapse/expand toggle, where 236px→64px should
  // animate; during a drag it re-animates on every pointer move, so the edge
  // visibly lags behind the cursor instead of tracking it.
  const [resizing, setResizing] = useState(false);
  const endDrag = () => {
    setDragPath(null);
    setDropAt(null);
    setDragLane(null);
    setLaneDropAt(null);
    setOnDock(false);
  };
  // Dropping is disabled while the rail is collapsed. A 64px row shows only a
  // marker, so there is no way to see *where* a drop would land — and a reorder
  // whose result you cannot see is a reorder you did not mean.
  const canDrag = props.wide;

  /** Whether this section accepts a dropped worktree. Pinned sections are
   *  normally not drop targets (they take no part in ordering) — the **trash is
   *  the exception**: it is a destination in its own right, where a drop bins the
   *  dragged worktree rather than positioning it. */
  const canDropOn = (group: RailGroup) => !group.pinned || group.key === TRASH_LANE;

  /**
   * Drop handlers for a section, resolving to an insertion index.
   *
   * `half` splits a row into its top and bottom halves so the gap *below* the last
   * row is reachable — without it, appending to a group was impossible: the row's
   * own handler stops propagation before the section's `index = length` handler
   * runs, and a flex column has no blank space under its last child to aim at.
   *
   * The trash ignores the insertion index: dropping there is a bin, not a
   * position.
   */
  const dropZone = (group: RailGroup, index: number, half = false) => ({
    onDragOver: (e: React.DragEvent) => {
      if (!dragPath || !canDropOn(group)) return;
      // Both required: preventDefault marks the element a valid drop target, and
      // without stopPropagation the enclosing section's own zone also fires and the
      // indicator flickers between the two.
      e.preventDefault();
      e.stopPropagation();
      setDropAt({ key: group.key, index: index + (half && below(e) ? 1 : 0) });
    },
    onDrop: (e: React.DragEvent) => {
      if (!dragPath || !canDropOn(group)) return;
      e.preventDefault();
      e.stopPropagation();
      if (group.key === TRASH_LANE) {
        props.onTrashDrop(dragPath);
      } else {
        props.onMove(dragPath, group.key, index + (half && below(e) ? 1 : 0));
      }
      endDrag();
    },
  });

  /**
   * The dock's own lane drop: always the last lane, never the geometry.
   *
   * The dock sits *outside* the scroller, and `getBoundingClientRect` is layout,
   * not clipping — a section below the fold has a bottom below the dock's own Y,
   * so running the dock's pointer through `laneTargetAt` picks whichever section
   * happens to overhang rather than the last lane. That is wrong exactly when the
   * dock target is useful: a rail long enough to scroll, scrolled up, where the
   * last lane is off-screen. The dock means "the bottom", so it says so directly
   * — its whole area, the Trash header included, because a dock that answered
   * differently depending on which of its two sections you were over would be a
   * distinction nothing on screen makes.
   *
   * It draws its own bar, too (`onDock`): the last lane's bar lives inside the
   * scroller, so in the very case this exists for it is scrolled out of sight and
   * the dock would accept a drop while showing nothing.
   */
  const laneDockDrop = (() => {
    const last = props.lanes.at(-1);
    if (dragLane === null || !last) return null;
    const take = (e: React.DragEvent) => {
      if (e.defaultPrevented) return false;
      e.preventDefault();
      return true;
    };
    return {
      onDragOver: (e: React.DragEvent) => {
        if (!take(e)) return;
        setLaneDropAt(props.lanes.length - 1);
        setOnDock(true);
      },
      onDrop: (e: React.DragEvent) => {
        if (!take(e)) return;
        props.onMoveLane(dragLane, last.name);
        endDrag();
      },
    };
  })();

  /**
   * The lane drag's drop zone inside the scroller: the list as a whole, not the
   * sections.
   *
   * One owner within the column. Every section and row bails out of its own
   * handlers while a lane is in flight (each drop zone answers only to its own
   * drag), so the event reaches here from anywhere in the list and
   * [`laneTargetAt`] decides what it means. A dragged lane always has somewhere
   * to land, which is what per-section targets could not promise; below the
   * scroller, `laneDockDrop` above owns the same gesture.
   *
   * Dropping a lane means displacement — it takes the target lane's place — so
   * there is no midpoint to consult: a lane is a block, and unlike a row it has
   * no "above me" and "below me" halves. Which side the bar is drawn on is a
   * rendering question, answered from the travel direction in `renderGroup`.
   */
  const laneDrop = dragLane === null
    ? null
    : {
        onDragOver: (e: React.DragEvent) => {
          if (e.defaultPrevented) return;
          const to = laneTargetAt(e.clientY);
          if (to === null) return;
          e.preventDefault();
          setLaneDropAt(to);
          setOnDock(false);
        },
        onDrop: (e: React.DragEvent) => {
          // A nested handler that already claimed this drop wins, and says so
          // by having called `preventDefault`. Without this the container is a
          // second, invisible consumer of the same event: a future "drop a lane
          // on the trash" would delete the lane *and* reorder one, because
          // `stopPropagation` is the only other way to stop this and nothing
          // here would remind its author to call it.
          if (e.defaultPrevented) return;
          const to = laneTargetAt(e.clientY);
          const onto = to === null ? undefined : props.lanes[to];
          if (!onto) return;
          e.preventDefault();
          props.onMoveLane(dragLane, onto.name);
          endDrag();
        },
      };

  /**
   * Render one rail section — a lane, a user lane, or one of the two
   * pinned bottom lanes (trash / deleting). Shared by the scrollable list
   * and the bottom dock, so a lane and the trash render identically; they
   * differ only in where they live, not in how a row looks.
   */
  const renderGroup = (group: RailGroup) => {
    const laneAt = laneAtOf(group);
    // Where the dragged lane would land, drawn in the gutter beside the hovered
    // section. Which side is the travel direction: carrying a lane *up* onto this
    // one puts it above, carrying it *down* puts it below. Exactly one section
    // ever draws a bar, so two of them cannot render the same gutter twice — and
    // hovering the dragged lane itself draws none, which is honest, because
    // dropping there is the one move that does nothing.
    const from = dragLane === null ? undefined : laneIndex.get(dragLane);
    let laneDropSide: "before" | "after" | null = null;
    // While the dock owns the hover it draws the bar itself, so the last lane
    // must not draw a second one for the same target.
    if (!onDock && laneAt !== undefined && from !== undefined && laneDropAt === laneAt) {
      if (from > laneAt) laneDropSide = "before";
      else if (from < laneAt) laneDropSide = "after";
    }
    return (
          <div
            key={group.key}
            className={`rail-group${group.key === TRASH_LANE ? " trash" : ""}${group.key === DELETING_LANE ? " deleting" : ""}${
              // Lit while a drag is over this section, so the target reads as a
              // whole area and not only as a caret between two rows. This is the
              // only feedback an EMPTY lane can give.
              dragPath && canDropOn(group) && dropAt?.key === group.key
                ? " drop-in"
                : ""
            }${group.editable && dragLane === group.lane ? " lane-dragging" : ""}${laneDropSide === "before" ? " lane-drop-before" : ""}${laneDropSide === "after" ? " lane-drop-after" : ""}`}
            // The section itself is the fallback target, and it resolves to its
            // FIRST position rather than its last. What actually reaches this
            // handler is the header and the padding above it — the rows stop
            // propagation — and both sit at the *top* of the section, so
            // appending here contradicted where the pointer was: dragging down
            // into a lane crossed its header and the caret jumped to the bottom.
            // Appending is still reachable, and unambiguously so: it is the lower
            // half of the last row.
            {...dropZone(group, 0)}
            /* What `laneTargetAt` measures. Only a real lane carries one, so the
               ungrouped section and the pinned lanes are not lane targets — a
               pointer over them resolves to the nearest lane instead. */
            data-lane-index={laneAt}
          >
            {group.label !== null && props.wide && (
              <div
                className={`lane-head${group.editable && canDrag ? " grab" : ""}`}
                /* The header IS the handle — the whole bar, not a grip icon
                   beside the name. A lane's header is already the thing that
                   stands for the lane (it is where the menu and the ＋ live), and
                   a rail this narrow cannot spare a third control per section.
                   The nested buttons drag it too, which is harmless: they act on
                   click, and a drag that starts on one is still a drag of the
                   lane it belongs to.

                   Only a real lane, and only expanded: the ungrouped section and
                   the trash hold no place in the lane order, and a collapsed rail
                   renders no headers at all. */
                draggable={group.editable && canDrag}
                onDragStart={(e) => {
                  setDragLane(group.lane);
                  // The two drags are exclusive, and this is what makes that
                  // true rather than conventional: every drop zone answers only
                  // to its own drag, so a `dragend` that never arrived (the
                  // source unmounted by the 5s poll, say) would otherwise leave
                  // both live and both sets of handlers armed at once.
                  setDragPath(null);
                  setDropAt(null);
                  e.dataTransfer.effectAllowed = "move";
                  // Firefox ignores a drag with no payload. Prefixed because a
                  // worktree drag puts a bare path here and something outside the
                  // rail may yet read it — the rail itself keys off the state.
                  e.dataTransfer.setData("text/plain", `lane:${group.lane}`);
                }}
                onContextMenu={
                  group.editable
                    ? (e) => props.onLaneMenu(e, group.lane)
                    : undefined
                }
              >
                <span className="lane-name">{group.label}</span>
                {/* Creating happens *into a lane*, and this is the whole of the
                    rail's create affordance: the single "+" that used to sit in
                    the toolbar could only ever mean "ungrouped", so filing a new
                    checkout was always create-then-drag. One button per section
                    that can hold a worktree removes the second step and makes the
                    destination the thing you clicked.

                    Always visible for the same two reasons `.lane-edit` is (see
                    its note): a hover-revealed control discovers nothing, and it
                    cannot be reached by keyboard. */}
                {group.addable && (
                  <Tooltip
                    label={
                      group.lane === ""
                        ? "New worktree, not in a group"
                        : `New worktree in ${group.lane}`
                    }
                  >
                    <button
                      type="button"
                      className="lane-edit"
                      /* Described by the *lane*, not by the header text, because
                         `UNGROUPED_LABEL` is itself a legal lane name — a repo with
                         a lane called "Worktrees" would otherwise give two sections
                         byte-identical accessible names, which a screen reader
                         cannot tell apart at all. */
                      aria-label={
                        group.lane === ""
                          ? "New worktree, not in a group"
                          : `New worktree in group ${group.lane}`
                      }
                      onClick={(e) => {
                        e.stopPropagation();
                        props.onAdd(group.lane);
                      }}
                    >
                      <IconPlus size={12} />
                    </button>
                  </Tooltip>
                )}
                {/* The trash's own action, in the place a lane keeps its menu:
                    emptying it is the only thing the section as a whole can do. */}
                {group.key === TRASH_LANE && (
                  <Tooltip label="Delete everything in the trash">
                    <button
                      type="button"
                      className="lane-edit"
                      aria-label="Empty the trash"
                      onClick={(e) => {
                        e.stopPropagation();
                        props.onEmptyTrash();
                      }}
                    >
                      <IconTrash size={12} />
                    </button>
                  </Tooltip>
                )}
                {/* The Detached lane's own header actions: a question mark that
                    says what the lane is, and the batch trash that matches its
                    point (detached checkouts are usually throwaways). The trash
                    button sits where a lane keeps its menu — like the trash lane,
                    this section has no real menu, so its one action lives there. */}
                {group.key === DETACHED_LANE && (
                  <>
                    <Tooltip
                      label="Detached: these checkouts are not on any branch (a detached HEAD). They can’t be pulled or committed to a branch until one is checked out — usually they’re throwaway, so you can clear them all at once."
                      maw={260}
                    >
                      <span
                        className="lane-edit lane-help"
                        role="img"
                        aria-label="What are detached worktrees?"
                      >
                        <IconHelp size={12} />
                      </span>
                    </Tooltip>
                    <Tooltip label="Move all detached worktrees to the trash">
                      <button
                        type="button"
                        className="lane-edit"
                        aria-label="Move all detached worktrees to the trash"
                        onClick={(e) => {
                          e.stopPropagation();
                          props.onTrashAllDetached();
                        }}
                      >
                        <IconTrash size={12} />
                      </button>
                    </Tooltip>
                  </>
                )}
                {/* Right-click alone is not an affordance — nothing on screen says
                    the header has a menu. The same ⋮ the rows carry, so the two read
                    as the same gesture. Only on a real lane: the trash and the
                    ungrouped section have nothing to rename or delete. */}
                {group.editable && (
                  <Tooltip label={`Menu for group ${group.label}`}>
                    <button
                      type="button"
                      className="lane-edit"
                      aria-label={`Menu for group ${group.label}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        props.onLaneMenu(e, group.lane);
                      }}
                    >
                      <IconDotsVertical size={12} />
                    </button>
                  </Tooltip>
                )}
              </div>
            )}
            {/* An empty lane needs a target you can see and hit. Without this the
                only droppable area was the header's few pixels of margin, so a lane
                you had just made looked like it refused worktrees. The trash is a
                drop target too — dropping a worktree on it bins it — so an empty
                trash needs the same affordance, saying plainly that worktrees can
                be dropped here. */}
            {props.wide && canDropOn(group) && group.worktrees.length === 0 && (
              group.addable ? (
                /* The addable empty lane is the lane's own "＋": clicking the
                   whole placeholder files into the same lane a header "＋" would,
                   so the two ways in are one target. The trash's empty state
                   (below) is a drop target only — there is no "＋" in the trash. */
                <button
                  type="button"
                  className="lane-empty"
                  onClick={(e) => {
                    e.stopPropagation();
                    props.onAdd(group.lane);
                  }}
                  aria-label={
                    group.lane === ""
                      ? "New worktree, not in a group"
                      : `New worktree in group ${group.lane}`
                  }
                >
                  {dragPath
                    ? "Drop here"
                    : // Both ways in, named. An empty lane is exactly where
                      // someone reaches for "＋", and the placeholder used to
                      // offer only the drag — now the whole thing is the button.
                      "Empty — drag one in or click to add one"}
                </button>
              ) : (
                <div className="lane-empty">
                  {dragPath ? "Drop here" : "Empty — drag a worktree in"}
                </div>
              )
            )}
            {group.worktrees.map((w, index) => {
              const caretHere =
                dropAt !== null &&
                dropAt.key === group.key &&
                dropAt.index === index;
              // The append caret rides on the last row, because a caret after the
              // final element has no following sibling to attach to.
              const caretAfter =
                dropAt !== null &&
                dropAt.key === group.key &&
                index === group.worktrees.length - 1 &&
                dropAt.index >= group.worktrees.length;
              const runs = runsForWorktree(props.envs, w);
              const status = worktreeStatus(runs);
              const running = status !== "stopped";
              const pending = props.pendingFor(w);
              const live = activeRun(runs);
              // The whole reason the run status dot could be deleted: `pending` is
              // only set by a click in THIS window, so a run coming up from the
              // CLI, from another window, or already starting when the window
              // opened had no transition signal on the *control* at all. Shared
              // with the top bar, which had the same gap.
              const spinner = spinnerAction(pending, live);
              // Every live run of the worktree, not only the one `activeRun`
              // picks. A directory can hold several environments at once — an
              // agent's alongside a human's — and `status` reports the healthiest
              // of them, so a sibling that failed or is stuck recovering had no
              // affordance here at all while the picked run stayed green.
              const liveAll = liveRuns(runs);
              const worst = worstStatus(liveAll);
              const attention = needsAttention(status) || needsAttention(worst);
              // Which status the alert affordance describes: the thing that
              // actually needs looking at, which may not be the picked run.
              const alertStatus = needsAttention(status) ? status : worst;
              // The run's own status, verbatim rather than through a table, so
              // this cannot drift from what the daemon reports. `activeRun` never
              // returns a stopped run, so a worktree with nothing up says nothing
              // — which is the state the deleted dot spent a grey circle on.
              //
              // This is where run state lives for the **collapsed** rail, whose
              // rows have no run control to carry it. In wide mode it is redundant
              // with the control, deliberately: a tooltip that says something
              // different depending on the rail's width is the worse surprise.
              //
              // With more than one live run it names them all, because "which of
              // the two is that dot about" is the question this row could not
              // answer before — and a single status here would have to pick one
              // silently, which is the defect, not the layout.
              const stateNote = pending
                ? ` · ${pending}…`
                : liveAll.length > 1
                  ? ` · ${liveAll.map((r) => `${r.name}: ${r.status}`).join(", ")}`
                  : live
                    ? ` · ${live.status}`
                    : "";
              const trashed = w.trashed_at !== "";
              // Terminal removal — distinct from recoverable trash: rendered in the
              // Deleting lane, not revertible, actively coming off the disk.
              const deletingRow = group.key === DELETING_LANE;
              // Inline controls are wide-only — a 64px collapsed row has no space
              // for them. Right-click reaches the same actions in either mode.
              // A worktree on its way out gets none: it cannot be started, and a
              // run control on it would be a button that only ever fails.
              const showRunControl = props.wide && !trashed && props.canRun(w);
              const holder = props.elsewhere.get(w.id);
              const away = holder !== undefined;
              // One pass over this worktree's sessions, here rather than inside the
              // icon: the summary is needed twice (the glyph and the row's
              // `aria-description`), and a rail holds every worktree of a monorepo.
              const inboxSummary = inbox.rowState(w.id, props.showWorking);
              return (
                /* A Fragment so the carets are the row's SIBLINGS. Drawn on the row
                   they were clipped by its `overflow: hidden` and rounded by its
                   `border-radius`, which is why an insertion point looked like a
                   selected row rather than a gap opening between two rows. */
                <Fragment key={w.id}>
                  {caretHere && <RailCaret />}
                  {/* A div with role=button, not a <button>: the row carries nested
                      controls of its own, and a <button> inside a <button> violates
                      the content model (the HTML parser closes the OUTER button when
                      it meets the inner start tag; React's createElement path builds
                      the invalid tree instead). Cost of the workaround: role=button
                      takes presentational children, so the nested ▶ and ⋮ are not
                      exposed to assistive tech. The honest fix is role=listbox on
                      .rail-list with role=option rows — deferred, see issue #167. */}
                  <div
                  role="button"
                  tabIndex={0}
                  /* Selection is "which one am I looking at", not a toggle that
                     can be un-pressed — aria-current, not aria-pressed. */
                  aria-current={props.active?.id === w.id ? true : undefined}
                  /* The activity state, for a screen reader. `aria-description` and
                     not the glyph's own label: the row is a `role=button` whose
                     accessible NAME is built from its content, so a status sentence
                     inside it would be announced ahead of the alias it describes —
                     the same reason the away icon is `aria-hidden`. */
                  aria-description={inboxDescription(inboxSummary)}
                  className={`wt-row${props.active?.id === w.id ? " active" : ""}${props.wide ? "" : " slim"}${away ? " away" : ""}${trashed ? " trashed" : ""}${deletingRow ? " deleting" : ""}${w.trash_error ? " failed-remove" : ""}${dragPath === w.path ? " dragging" : ""}`}
                  title={
                    deletingRow
                      ? `${worktreeLabel(w)} — being deleted, cannot be restored`
                      : trashed
                        ? `${worktreeLabel(w)} — in the trash, still on disk`
                        : w.trash_error
                          ? `${worktreeLabel(w)} — could not be deleted: ${w.trash_error}`
                          : away
                            ? `${worktreeLabel(w)} — ${w.branch}${stateNote} (${awayNote(holder)})`
                            : `${worktreeLabel(w)} — ${w.branch}${stateNote}`
                  }
                  /* Pending removals are not draggable: they are leaving, so a
                     position for them means nothing. */
                  /* The main checkout is never dragged, even when the user has put
                     it in a lane (which un-pins its section). `WT_ORDER` sorts
                     `is_main DESC` ahead of `sort_position`, so a position given to
                     main is ignored and the row visibly snaps back to the top of its
                     group — a drag that appears to do nothing. It leads its lane
                     instead, which is the same rule it follows ungrouped. */
                  draggable={canDrag && !trashed && !group.pinned && !w.is_main}
                  onDragStart={(e) => {
                    setDragPath(w.path);
                    // Exclusive with the lane drag — see the lane header's own
                    // `onDragStart`.
                    setDragLane(null);
                    setLaneDropAt(null);
                    e.dataTransfer.effectAllowed = "move";
                    // Firefox ignores a drag with no payload, and the path is the
                    // key everything downstream uses anyway.
                    e.dataTransfer.setData("text/plain", w.path);
                  }}
                  {...dropZone(group, index, true)}
                  /* A pending removal is not selectable: selecting it would open
                     panes, terminals and a browser rooted at a directory that is
                     about to stop existing. The restore control and the context
                     menu are still live. */
                  onClick={() => {
                    if (!trashed) props.onSelect(w);
                  }}
                  onKeyDown={(e) => {
                    // Only the row's OWN key events. Keydown bubbles from the
                    // nested buttons, and preventDefault() here would cancel
                    // their native activation — Enter on ▶ would silently select
                    // the row instead of starting the run.
                    if (e.target !== e.currentTarget) return;
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      if (!trashed) props.onSelect(w);
                    }
                  }}
                  onContextMenu={(e) => props.onMenu(e, w)}
                >
                  {/* Leads the row, and rendered in the collapsed rail too — which
                      is exactly where a greyed row alone would be too subtle to
                      read. It used to sit beside the run-status dot; with that dot
                      gone this is the row's first glyph. */}
                  {away && (
                    <IconExternalLink
                      size={11}
                      className="wt-away"
                      /* Decorative, and it has to be. The row is a `role=button`
                         whose accessible name comes from its content, so a label
                         here would be folded into that name *before* the alias —
                         "open in another window Sonnet feat/x" — and the `title`
                         that already says it would be demoted to the description,
                         announcing it a second time. The title carries it. */
                      aria-hidden
                    />
                  )}
                  <WorktreeMark settings={props.settings} worktree={w} />
                  {/* The name the user typed, verbatim — spaces, capitals and all.
                      The slug they never chose is still one line down, because the
                      branch is derived from the same input and usually *is* it. */}
                  {props.wide && (
                    <span className="wt-alias">{worktreeLabel(w)}</span>
                  )}
                  {/* The branch column carries the removal failure — the one thing
                      about a row that is not the branch any more. Branch names and
                      the trash/deleting states are deliberately not shown: the rail
                      is narrow, the alias the user typed is the row's name, and a
                      row in the Trash (or Deleting) lane is already visibly there,
                      so restating it per row is noise. Only a *failure* gets a
                      column, because that is the one thing the lane does not say. */}
                  {props.wide && w.trash_error && (
                    <span className="wt-branch">{w.trash_error}</span>
                  )}
                  {/* Beside the run control, and not on the marker. The marker is
                      the row's identity — #204 made its *colour* the identifier —
                      so tinting it for a failure overwrites the one channel that
                      answers "which worktree is this", which is the collision this
                      change exists to remove. An icon of its own is also what makes
                      the state reachable rather than merely reported: it opens the
                      nodes view. Rendered in the collapsed rail too, where the run
                      control cannot go, so the signal that asks to be acted on
                      survives the mode.

                      After the alias, not before it: the row is a `role=button`
                      whose accessible name comes from its content, and a nested
                      control's `aria-label` is folded into that name — placed
                      first, it would announce "Node health for chk" ahead of the
                      alias. Same reason the away icon beside the alias is
                      `aria-hidden`. */}
                  {/* More than one live run in one directory: the count is
                      rendered, not implied. The rail's dot and control can only
                      speak for one of them, so a row that showed nothing here left
                      the second environment with no representation anywhere in the
                      rail — which is how an agent-started run stayed invisible
                      until it broke something. */}
                  {props.wide && !trashed && liveAll.length > 1 && (
                    <span
                      className={`run-count${needsAttention(worst) ? " alert" : ""}`}
                      title={`${liveAll.length} runs live here: ${liveAll
                        .map((r) => `${r.name} (${r.status})`)
                        .join(", ")}`}
                    >
                      {liveAll.length}
                    </span>
                  )}
                  {!trashed && attention && (
                    <button
                      type="button"
                      className={`wt-alert ${alertStatus}`}
                      title={
                        alertStatus === "recovering"
                          ? `${worktreeLabel(w)} — veld is restarting a node that keeps failing its liveness probe. Open node health.`
                          : `${worktreeLabel(w)} — the run failed. Open node health.`
                      }
                      aria-label={`Node health for ${worktreeLabel(w)}`}
                      onClick={(e) => {
                        // The row selects on click; without this the affordance
                        // would fire the row's plain selection as well.
                        e.stopPropagation();
                        void props.onDiagnose(w);
                      }}
                    >
                      <IconAlertTriangleFilled size={11} />
                    </button>
                  )}
                  {/* This worktree's terminal activity, immediately left of the run
                      control — the far right of the row, where the eye lands last.
                      AFTER the node-health alert: that one is about a *run* failing its
                      probe and is clickable, this one is about *you* and is not.
                      Rendered in the slim rail too, which the run control is not: it is
                      the one indicator whose whole purpose is to be seen while you are
                      looking somewhere else. Trashed rows are excluded — their panes are
                      gone, so an event there could never be read by looking. */}
                  {!trashed && (
                    <InboxIcon summary={inboxSummary} label={worktreeLabel(w)} />
                  )}
                  {showRunControl && (
                    <Tooltip
                      label={
                        pending
                          ? `${pending}…`
                          : running
                            ? `Stop ${worktreeLabel(w)}`
                            : `Start ${worktreeLabel(w)}`
                      }
                    >
                      <button
                        type="button"
                        className={`wt-run${running ? " on" : ""}`}
                        aria-label={running ? `Stop ${worktreeLabel(w)}` : `Start ${worktreeLabel(w)}`}
                        // Mirrors the context menu and the palette. Without the
                        // start guard the button looked live but its click hit a
                        // no-op for a worktree with no presets and no nodes.
                        //
                        // Deliberately keyed on `pending`, not on `spinner`: a
                        // spinner is a state *display*, and a run that some other
                        // surface started is still legitimately stoppable while it
                        // comes up. Only an action this window fired and has not
                        // seen land disables the control, which is what stops a
                        // double fire.
                        disabled={
                          pending !== null || (!running && !props.canStart(w))
                        }
                        onClick={(e) => {
                          // The row is clickable too; without this, starting a run
                          // would also switch the selection out from under the user.
                          e.stopPropagation();
                          if (running) props.onStop(w);
                          else props.onStart(w);
                        }}
                      >
                        {spinner ? (
                          // The spinner carries the action's colour, so a row that
                          // is stopping reads as stopping and not as starting. That
                          // held only for locally-fired actions before `spinner`
                          // took the observed transition into account too.
                          <Loader size={10} color={actionColor(spinner)} />
                        ) : running ? (
                          <IconPlayerStopFilled size={10} />
                        ) : (
                          <IconPlayerPlayFilled size={10} />
                        )}
                      </button>
                    </Tooltip>
                  )}
                  {props.wide && !trashed && (
                    <button
                      type="button"
                      className="wt-edit"
                      title="Worktree menu"
                      aria-label={`Menu for ${worktreeLabel(w)}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        props.onMenu(e, w);
                      }}
                    >
                      <IconDotsVertical size={12} />
                    </button>
                  )}
                  {props.wide && trashed && !deletingRow && (
                    <Tooltip label={`Restore ${worktreeLabel(w)}`}>
                      <button
                        type="button"
                        className="wt-edit"
                        aria-label={`Restore ${worktreeLabel(w)}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          props.onRestore(w);
                        }}
                      >
                        <IconArrowBackUp size={12} />
                      </button>
                    </Tooltip>
                  )}
                  {/* A terminal removal has no restore — it is committed — so the
                      trailing slot that would hold Restore carries a spinner
                      instead, making the in-progress state audible as motion. */}
                  {props.wide && deletingRow && (
                    <Loader
                      size={12}
                      className="wt-deleting-spinner"
                      color="var(--danger)"
                    />
                  )}
                  </div>
                  {caretAfter && <RailCaret />}
                </Fragment>
              );
            })}
          </div>
    );
  };
  return (
    <div
      className={`rail${props.wide ? " wide" : ""}${resizing ? " resizing" : ""}`}
      style={props.wide ? { width: props.width } : undefined}
    >
      <div className="rail-head">
        <Tooltip label={props.wide ? "Collapse the worktree rail" : "Expand the worktree rail"}>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label={props.wide ? "Collapse the worktree rail" : "Expand the worktree rail"}
            onClick={props.onToggle}
          >
            {props.wide ? <IconChevronLeft size={13} /> : <IconChevronRight size={13} />}
          </ActionIcon>
        </Tooltip>
        {/* Push the create actions to the right edge of the head — collapse stays
            put on the left, and the two creates sit together at the far end. */}
        <div style={{ flex: 1 }} />
        {props.wide && (
          <>
            <Tooltip label="New group">
              <ActionIcon
                size="sm"
                variant="subtle"
                color="gray"
                aria-label="New group"
                onClick={props.onAddLane}
              >
                <IconFolderPlus size={14} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="New worktree">
              <ActionIcon
                size="sm"
                variant="subtle"
                color="gray"
                aria-label="New worktree"
                onClick={() => props.onAdd("")}
              >
                <IconPlus size={14} />
              </ActionIcon>
            </Tooltip>
          </>
        )}
        {/* Collapsed only, mirroring "New lane" above it — and for the same
            reason, inverted. Expanded, every section that can hold a worktree
            carries its own "＋" and a toolbar button would be a second way to do
            the same thing whose destination is not visible. Collapsed, there are
            no headers at all, so this is the only place a create can live; it
            files into the ungrouped section, which is where the old toolbar
            button always put things anyway. */}
        {!props.wide && (
          <Tooltip label="New worktree">
            <ActionIcon
              size="sm"
              variant="subtle"
              color="gray"
              aria-label="New worktree"
              onClick={() => props.onAdd("")}
            >
              <IconPlus size={14} />
            </ActionIcon>
          </Tooltip>
        )}
      </div>
      <div
        className="rail-list"
        ref={listRef}
        onDragEnd={endDrag}
        {...(laneDrop ?? {})}
      >
        {scroll.map((group) => renderGroup(group))}
      </div>
      {/* The dock takes a lane drop too, though it holds no lane. It is the
          natural overshoot for "pull this lane to the bottom", and it is the
          strip immediately under the edge you have to reach to get there —
          refusing there made the last position the one place the gesture could
          miss. Its own handler, not the list's: see `laneDockDrop`. */}
      {dockVisible && (
        <div
          className={`rail-dock${
            // Not while the carried lane is already the last one: that drop is a
            // no-op, and a bar promising a move that will not happen is worse
            // than none.
            onDock && dragLane !== null && props.lanes.at(-1)?.name !== dragLane
              ? " lane-drop-into"
              : ""
          }`}
          onDragEnd={endDrag}
          {...(laneDockDrop ?? {})}
        >
          {docked.map((group) => renderGroup(group))}
        </div>
      )}
      {/* Only when expanded: collapsed is a mode with a fixed width, so there is
          nothing to drag. */}
      {props.wide && (
        <RailResizer
          width={props.width}
          onWidth={props.onWidth}
          onDragging={setResizing}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Command palette
// ---------------------------------------------------------------------------

/**
 * The worktree statuses ⌘K spells out beside a branch, and the wording.
 *
 * A total map rather than a conditional so adding a [`WorktreeStatus`] member
 * fails the build here instead of silently rendering nothing — the empty string
 * is how a state opts out, which is a decision someone has to write down.
 */
const PALETTE_STATUS: Record<WorktreeStatus, string> = {
  running: "",
  partial: "",
  recovering: "recovering",
  failed: "failed",
  stopped: "",
};

/** Header order for the idle (no-query) list. Also the grouping key. */
const PALETTE_GROUPS = ["Worktrees", "Run", "Panes", "Worktree", "Projects", "View"] as const;
type PaletteGroup = (typeof PALETTE_GROUPS)[number];

/** One ⌘K entry: a worktree to jump to, or an action to run. */
interface PaletteItem {
  id: string;
  /** Must be one of PALETTE_GROUPS — the idle list is sorted by that order,
   *  so items need not be declared contiguously by group. */
  group: PaletteGroup;
  label: string;
  /** Dim right-hand detail (branch, URL, path). */
  hint?: string;
  /** Extra haystacks the query may match, beyond the label. */
  alt?: string[];
  /** The worktree this row stands for, when it stands for one — so the row can
   *  render the same marker face the rail does rather than hardcoding a glyph. */
  mark?: { emoji: string; marker_color: string };
  run: () => void;
}

/** The label with the fuzzy-matched characters marked. */
function Highlighted(props: { text: string; positions: number[] }) {
  if (props.positions.length === 0) return <>{props.text}</>;
  const hit = new Set(props.positions);
  const parts: React.ReactNode[] = [];
  let run = "";
  let runIsHit = hit.has(0);
  const flush = (key: number) => {
    if (!run) return;
    parts.push(runIsHit ? <mark key={key}>{run}</mark> : run);
    run = "";
  };
  for (let i = 0; i < props.text.length; i++) {
    const isHit = hit.has(i);
    if (isHit !== runIsHit) {
      flush(i);
      runIsHit = isHit;
    }
    run += props.text[i];
  }
  flush(props.text.length);
  return <>{parts}</>;
}

/**
 * ⌘K palette: fuzzy search over worktrees *and* commands.
 *
 * With no query the items stay in their declared order under group headers —
 * that reads as a menu of what's available. Once the user types, grouping is
 * dropped for a single score-ordered list (the group becomes the right-hand
 * hint), because a global ranking is what "I know what I want" wants.
 */
function CommandPalette(props: {
  project: string;
  items: PaletteItem[];
  settings: SettingsDoc | null;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  // Tracked by item id, not index: `items` is rebuilt on every 5s poll, and a
  // run transition inserts or removes the Stop/Restart entries — an index
  // would silently slide onto a different command between the user reading
  // the row and pressing Enter. `null` = "first match".
  const [cursorId, setCursorId] = useState<string | null>(null);
  // Only arrow keys should scroll the list. Following the pointer as well
  // would yank the view out from under a user who is merely moving the mouse
  // across it.
  const scrollToCursor = useRef(false);

  const searching = query.trim().length > 0;
  const matches = searching
    ? props.items
        .map((item) => ({
          item,
          match: bestFuzzyMatch([item.label, ...(item.alt ?? [])], query),
          // Highlight only what matched in the label itself; a hit that came
          // from a branch or URL has no positions to mark here.
          label: fuzzyMatch(item.label, query),
        }))
        .filter((r) => r.match !== null)
        .sort((a, b) => b.match!.score - a.match!.score)
    : // Group the idle list explicitly rather than trusting declaration
      // order: the single `lastGroup` cursor below would emit a duplicate
      // header for any item appended out of group order.
      PALETTE_GROUPS.flatMap((g) =>
        props.items
          .filter((item) => item.group === g)
          .map((item) => ({ item, match: null, label: null })),
      );

  // Resolve the id back to a position; an id that filtered out falls back to
  // the top match, which is what a user who just typed expects.
  const found = matches.findIndex((m) => m.item.id === cursorId);
  const active = found === -1 ? 0 : found;

  const choose = (item: PaletteItem) => {
    props.onClose();
    item.run();
  };

  const move = (delta: number) => {
    if (matches.length === 0) return;
    scrollToCursor.current = true;
    const next = (active + delta + matches.length) % matches.length;
    setCursorId(matches[next].item.id);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (matches.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      move(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      move(-1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(matches[active].item);
    }
  };

  let lastGroup = "";
  return (
    <Modal
      title={props.project ? `Search ${props.project}` : "Search"}
      onClose={props.onClose}
    >
      <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        <TextInput
          placeholder="Jump to a worktree, or run a command…"
          value={query}
          onChange={(e) => {
            setQuery(e.currentTarget.value);
            setCursorId(null);
          }}
          onKeyDown={onKeyDown}
          styles={{
            input: { fontFamily: "var(--mantine-font-family-monospace)" },
          }}
          data-autofocus
        />
        <div className="palette-list">
          {matches.map(({ item, label }, i) => {
            const header = !searching && item.group !== lastGroup;
            if (header) lastGroup = item.group;
            return (
              <div key={item.id}>
                {header && <div className="section-label">{item.group}</div>}
                <button
                  type="button"
                  className={`wt-row${i === active ? " sel" : ""}`}
                  style={{ width: "100%" }}
                  ref={
                    i === active
                      ? (el) => {
                          if (!el || !scrollToCursor.current) return;
                          scrollToCursor.current = false;
                          el.scrollIntoView({ block: "nearest" });
                        }
                      : undefined
                  }
                  /* No onMouseEnter cursor sync: scrollIntoView slides rows
                     under a stationary pointer, whose mouseenter would drag
                     the cursor back and stall arrow-key navigation. Enter and
                     click can't disagree anyway — onClick uses this row's own
                     item, not matches[active]. Hover is :hover in CSS. */
                  onClick={() => choose(item)}
                >
                  {item.mark && (
                    <WorktreeMark settings={props.settings} worktree={item.mark} />
                  )}
                  <span className="pal-label">
                    <Highlighted
                      text={item.label}
                      positions={label?.positions ?? []}
                    />
                  </span>
                  <span className="pal-hint">
                    {item.hint ?? (searching ? item.group : "")}
                  </span>
                </button>
              </div>
            );
          })}
        </div>
        {matches.length === 0 && (
          <div className="note-card">Nothing matches “{query.trim()}”.</div>
        )}
      </div>
    </Modal>
  );
}

