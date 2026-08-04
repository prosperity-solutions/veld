import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  runRef,
  type EmojiHolder,
  type EnvironmentList,
  type Lane,
  type Repo,
  type RepoList,
  type RunInfo,
  type SharesList,
  type SettingsDoc,
  type StatsResponse,
  type Worktree,
} from "./api";
import {
  markerFace,
  markerStyle,
  quickSwitchPrefs,
  terminalPrefs,
} from "./shared/settings";
import { applyTerminalPrefs } from "./panes/terminalHost";
import { SettingsDialog } from "./components/SettingsDialog";
import { useSettings } from "./shared/useSettings";
import {
  activeRun,
  bestFuzzyMatch,
  diagnosticsRun,
  fuzzyMatch,
  moveWorktree,
  prunePending,
  railGroups,
  runSignature,
  runsForWorktree,
  sortedUrls,
  worktreeStatus,
  TRASH_LANE,
  type PendingAction,
  type RailGroup,
  type PendingMap,
  type WorktreeStatus,
} from "./model";
import { Wordmark } from "./components/Wordmark";
import {
  ActionIcon,
  Button,
  Loader,
  MantineProvider,
  Menu,
  Popover,
  Select,
  Tooltip,
  TextInput,
} from "@mantine/core";
import {
  IconArrowBackUp,
  IconArrowsExchange,
  IconChevronLeft,
  IconChevronRight,
  IconDots,
  IconDotsVertical,
  IconFolderPlus,
  IconMoon,
  IconPlayerPlayFilled,
  IconPlayerStopFilled,
  IconPlus,
  IconRefresh,
  IconSearch,
  IconSettings,
  IconShare2,
  IconTrash,
  IconSun,
  IconDeviceDesktop,
  IconExternalLink,
  IconWorld,
} from "@tabler/icons-react";
import { Notifications } from "@mantine/notifications";
import { ContextMenuProvider, useContextMenu } from "mantine-contextmenu";
import { theme as mantineTheme } from "./theme";
import { RunsMode } from "./runs/RunsMode";
import { PaneArea } from "./panes/PaneArea";
import type { RunPaneContext } from "./panes/RunPanes";
import { notifyDone, notifyError, notifyRedirect } from "./shared/notify";
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
  activateTab,
  activeTab,
  addTab,
  addTabToFocused,
  adoptTabs,
  allTabs,
  browserIds,
  browserTab,
  closeTab,
  defaultLayout,
  diagTab,
  dockOf,
  forgetWorktreeLayouts,
  lastBlankBrowserId,
  loadLayouts,
  newTabId,
  nextFreeProfile,
  paneTabLabel,
  parseSessionSets,
  parseTransferTabs,
  readWorktreeLayout,
  saveLayouts,
  serializeSessionSets,
  sessionSetFor,
  terminalIds,
  updateTab,
} from "./panes/model";
import {
  applyTerminalTheme,
  pruneTerminals,
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
  startBody,
  startStorageKey,
} from "./components/StartConfig";
import {
  ChangeMarkerDialog,
  ImportRepoDialog,
  LaneNameDialog,
  Modal,
  NewWorktreeDialog,
  RemoveRepoDialog,
  RenameWorktreeDialog,
} from "./components/dialogs";

import {
  chromeless,
  desktopApp,
  desktopWindow,
  layoutSlot,
  openSettingsOnBoot,
  topbarClass,
  windowRestored,
  windowSeed,
} from "./shell";

const POLL_MS = 5000;

/** How long an optimistic pending marker survives without an observed run
 *  signature change. Several polls' worth, so a slow `veld start` isn't cut
 *  off. */
const PENDING_TTL_MS = 60_000;

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
 * The Runs|IDE switcher hides behind the wordmark: hover reveals it, unhover
 * restores the logo. The switcher stays rendered (visibility:hidden) so the
 * bar reserves its width and nothing shifts on hover.
 */
function LogoModeSwitch(props: {
  mode: string;
  onMode: (m: string) => void;
  /** Hover state lives in the parent: this component remounts when the mode
   *  switches bars, and a local state reset would flash the logo mid-hover. */
  hover: boolean;
  onHover: (h: boolean) => void;
}) {
  const { hover, onHover: setHover } = props;
  const other = props.mode === "ide" ? "runs" : "ide";
  const otherLabel = other === "runs" ? "Runs" : "IDE";
  return (
    <div
      className="logo-switch"
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      <div className={`ls-layer${hover ? " show" : ""}`}>
        <Tooltip label={`Switch to ${otherLabel}`}>
          <Button
            size="compact-xs"
            variant="default"
            leftSection={<IconArrowsExchange size={13} />}
            onClick={() => props.onMode(other)}
          >
            {otherLabel}
          </Button>
        </Tooltip>
      </div>
      <div className={`ls-layer${hover ? "" : " show"}`} style={{ pointerEvents: "none" }}>
        <Wordmark />
      </div>
    </div>
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
        <AppInner theme={theme} themePref={themePref} onCycleTheme={cycleTheme} />
      </ContextMenuProvider>
    </MantineProvider>
  );
}

function AppInner(props: {
  theme: string;
  themePref: string;
  onCycleTheme: () => void;
}) {
  const { theme, themePref, onCycleTheme } = props;

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
  }, [settings]);

  // Which quick switches a browser pane's chrome shows. Read here and threaded,
  // rather than each pane calling `useSettings` — that would be a fetch and a
  // focus listener per pane for one document the app already holds.
  const quickSwitches = quickSwitchPrefs(settings ?? {});

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
      setEnvs(environments);
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
  }, [wantRunState]);

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
  const worktrees = useMemo(() => repo?.worktrees ?? [], [repo]);
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

  /**
   * Worktrees another window is showing.
   *
   * Only for the rail's benefit — the shell is the authority on who may show
   * what, and `selectWorktree` still asks it. Rendering this is what stops the
   * ownership model reading as a bug: without it, a row simply refuses to open
   * and some other window jumps forward with no stated connection between the
   * two.
   */
  const [elsewhere, setElsewhere] = useState<Set<number>>(new Set());
  useEffect(() => {
    const shell = desktopWindow;
    if (chromeless || !shell?.onClaimsChanged) return;
    let cancelled = false;
    let pushed = false;
    // Both, and in this order: the subscription first, so a claim made while
    // the initial query is in flight is not lost, then the query for the state
    // that predates this window. `pushed` because the answer to the query can
    // land after an update that supersedes it — and "nobody else has anything"
    // is a real answer, so an empty set cannot stand in for "not yet asked".
    const off = shell.onClaimsChanged((p) => {
      pushed = true;
      setElsewhere(new Set(p.worktreeIds));
    });
    void shell
      .claimedElsewhere()
      .then((ids) => {
        if (!cancelled && !pushed) setElsewhere(new Set(ids));
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      off();
    };
  }, [chromeless]);

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
  const selectWorktree = (w: Worktree) => {
    if (!desktopWindow) {
      setActiveRepoRoot(w.repo_root);
      setActiveWtKey(String(w.id));
      return;
    }
    void desktopWindow
      .claimWorktree(w.id)
      .then((result) => {
        if (!result?.ok) {
          // Without this the click reads as ignored: the row does not open,
          // and a *different* window comes forward with nothing tying the two
          // together. The toast is in this window, not the one that took
          // focus — it is what you find when you come back, and the greyed
          // rail row (`elsewhere`) is what warns you before you click.
          if (result?.reason === "shown-elsewhere") {
            notifyRedirect(`${w.alias} is open in another window — switched to it`);
          }
          return;
        }
        setActiveRepoRoot(w.repo_root);
        setActiveWtKey(String(w.id));
      })
      .catch(() => {
        // An older shell without the channel: behave as it did before.
        setActiveRepoRoot(w.repo_root);
        setActiveWtKey(String(w.id));
      });
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

  // ---- derived run state --------------------------------------------------
  const runs = worktree ? runsForWorktree(envs, worktree) : [];
  const run = activeRun(runs);
  const urls = sortedUrls(run);
  const status = worktreeStatus(runs);

  // The permission policy every browser pane in this window is answered against.
  //
  // Pushed from here because this is what knows both halves: the rules are the
  // selected worktree's `ide.permissions`, and the trusted origins are the URLs
  // veld itself serves for its run — the only origins a pane may capture its own
  // contents at without asking, which is what makes `veld feedback` screenshots
  // work inside a pane. Re-sent whenever either changes; a no-op in the browser
  // build, which has no panes to govern.
  const permissionRules = worktree?.ide.permissions ?? [];
  const trustedOrigins = urls.map(([, url]) => url);
  useEffect(() => {
    // The daemon's own origin is in the set because veld's UI is served from it,
    // and a pane pointed at a veld surface is still veld's own page.
    setBrowserPolicy(permissionRules, [...trustedOrigins, window.location.origin]);
    // Serialised rather than compared by reference: both are rebuilt on every
    // poll, so an identity dependency would push the policy several times a
    // second and republish every pane's panel with it.
  }, [JSON.stringify(permissionRules), trustedOrigins.join(" ")]);

  // What the diagnostics panes and the Sharing surface read. Wider than `run` on
  // purpose: an ended run still has logs, last node states and possibly a share
  // left to stop — see `diagnosticsRun`.
  const diagRun: RunInfo | null = diagnosticsRun(runs);
  const diagRef = worktree && diagRun ? runRef(worktree.path, diagRun) : null;
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
  const effectiveStart = worktree
    ? (pruneStartSelection(worktree, parseStartSelection(startRaw)) ??
      defaultStartSelection(worktree))
    : null;

  // Optimistic pending markers while 202'd start/stop/restarts take effect,
  // keyed by worktree: the rail can fire actions on several rows at once, and
  // a single global slot would let the second overwrite the first's marker
  // and strand its spinner. Each entry clears when THAT worktree's run
  // signature moves off the value it had when the action was fired.
  const [pending, setPending] = useState<PendingMap>({});
  // Every loaded worktree, not just the selected repo's: switching projects
  // mid-action must not look like the worktree vanished, or the marker is
  // dropped and the re-enabled control invites a double fire.
  const allWorktrees = useMemo(
    () => repos.flatMap((r) => r.worktrees),
    [repos],
  );
  useEffect(() => {
    setPending((cur) =>
      prunePending(cur, Date.now(), (id) => {
        const wt = allWorktrees.find((w) => w.id === id);
        return wt ? runSignature(runsForWorktree(envs, wt)) : null;
      }),
    );
    // `poll` is a dependency so the TTL is re-checked on every tick, not only
    // when the payload changes: a failed refresh leaves `envs` identical, and
    // without this a marker could never expire while the daemon is down.
  }, [envs, allWorktrees, poll]);
  const pendingFor = (w: Worktree | null): PendingAction | null =>
    w ? (pending[w.id]?.label ?? null) : null;

  /** Run actions make sense only for a worktree of an on-disk repo. */
  const canRunWorktreeNow = (w: Worktree) =>
    w.has_veld_config && (repo?.available ?? false);

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

  const act = async (
    w: Worktree,
    label: PendingAction,
    fn: () => Promise<void>,
  ) => {
    const sigAtSet = runSignature(runsForWorktree(envs, w));
    setPending((cur) => ({
      ...cur,
      [w.id]: { label, sigAtSet, expiresAt: Date.now() + PENDING_TTL_MS },
    }));
    try {
      await fn();
    } catch (e) {
      setPending((cur) => {
        const next = { ...cur };
        delete next[w.id];
        return next;
      });
      // Name the worktree: actions fire from the rail, the context menu and the
      // palette on ANY row, so an unattributed message leaves the user guessing
      // which one failed.
      notifyError(`${label} failed on ${w.alias}`, e);
    }
  };

  // Run actions for ANY worktree, not just the selected one — the rail rows,
  // the context menu and the palette all drive these.
  const startWorktree = (w: Worktree) => {
    const sel = resolveStartSelection(w);
    if (!sel) {
      // Defence in depth: all four ▶ surfaces gate on `canStartWorktree`,
      // which rejects exactly this case, so this should be unreachable. If a
      // future caller skips the guard, say what's wrong instead of no-opping.
      notifyError(
        `Start ${w.alias}`,
        "nothing to start — no presets or startable nodes in its veld.json.",
      );
      return;
    }
    void act(w, "start", () => api.startRun(w.id, startBody(sel)));
  };
  // `w.path` is the run's project root — every worktree with a veld.json is
  // its own project (see `runsForWorktree`), and the run name alone would be
  // ambiguous across repos.
  const stopWorktree = (w: Worktree) => {
    const r = activeRun(runsForWorktree(envs, w));
    if (r) void act(w, "stop", () => api.stopRun(runRef(w.path, r)));
  };
  const restartWorktree = (w: Worktree) => {
    const r = activeRun(runsForWorktree(envs, w));
    if (r) void act(w, "restart", () => api.restartRun(runRef(w.path, r)));
  };

  // ---- run diagnostics ----------------------------------------------------
  // One object rather than six props threaded through PaneArea → DockView: the
  // `logs` and `nodes` panes read the *selected* worktree's run, so every pane
  // re-points on a worktree switch and none of them captures a run of its own.
  const runCtx: RunPaneContext = {
    ref: diagRef,
    run: diagRun,
    stats: diagStats,
    emptyHint: runEmptyHint,
    onChanged: () => void refresh(),
    // A node's URL, opened beside the terminal instead of in another application.
    // The same action ⌘K and the URL launcher offer, so all three land a pane in
    // the focused dock rather than each inventing a placement.
    onOpenPane: (name, url) =>
      setLayout((prev) => addTabToFocused(prev, browserTab({ url, title: name }))),
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
    | { kind: "new-worktree" }
    | { kind: "rename"; worktree: Worktree; deleteFocus?: boolean }
    | { kind: "marker"; worktree: Worktree }
    /** `worktree` set means "create it, then move this one into it". */
    | { kind: "new-lane"; worktree?: Worktree }
    | { kind: "rename-lane"; lane: string }
    | { kind: "settings" }
    | { kind: "remove-repo"; repo: Repo }
    | { kind: "search" }
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
      const holder = { id: w.id, alias: w.alias };
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
  const openWorktreeWindow = async (w: Worktree) => {
    if (!desktopWindow) return;
    try {
      const result = await desktopWindow.newWindow({
        repoRoot: w.repo_root,
        worktreeId: w.id,
      });
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

  const worktreeMenu = (w: Worktree) => {
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
        title: "Move to lane",
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
                  title: "Remove from lane",
                  onClick: () => void assignLane(w, ""),
                },
              ]
            : []),
          ...(lanes.length > 0 ? [{ key: "lane-new-divider" }] : []),
          {
            key: "lane-new",
            title: "New lane…",
            onClick: () => setDialog({ kind: "new-lane", worktree: w }),
          },
        ],
      },
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
              onClick: () =>
                setDialog({ kind: "rename", worktree: w, deleteFocus: true }),
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
        onClick: () =>
          setDialog({ kind: "rename", worktree: w, deleteFocus: true }),
      },
    ]);
  };
  const assignLane = async (w: Worktree, lane: string) => {
    try {
      await api.patchWorktree(w.id, { lane });
    } catch (e) {
      notifyError(`Could not move ${w.alias}`, e);
    }
    await refresh();
  };

  const laneMenu = (lane: string) => {
    const index = lanes.findIndex((l) => l.name === lane);
    const move = async (to: number) => {
      if (!repo) return;
      const order = lanes.map((l) => l.name);
      const [name] = order.splice(index, 1);
      order.splice(to, 0, name);
      try {
        await api.reorderLanes(repo.root, order);
      } catch (e) {
        notifyError("Could not reorder the lanes", e);
      }
      await refresh();
    };
    return showContextMenu([
      {
        key: "lane-rename",
        title: "Rename lane…",
        onClick: () => setDialog({ kind: "rename-lane", lane }),
      },
      {
        key: "lane-up",
        title: "Move lane up",
        disabled: index <= 0,
        onClick: () => void move(index - 1),
      },
      {
        key: "lane-down",
        title: "Move lane down",
        disabled: index < 0 || index >= lanes.length - 1,
        onClick: () => void move(index + 1),
      },
      { key: "lane-divider" },
      {
        key: "lane-delete",
        title: "Delete lane",
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
      notifyError(`Could not delete the lane "${lane}"`, e);
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
      if (mod && (e.key === "k" || ((e.key === "P" || e.key === "p") && e.shiftKey))) {
        e.preventDefault();
        setDialog({ kind: "search" });
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

  const [layouts, setLayouts] = useState<Record<number, PaneLayout>>(() =>
    loadLayouts(layoutSlot, windowSeed, windowRestored, chromeless),
  );
  const layout = worktree ? layouts[worktree.id] : undefined;

  useEffect(() => {
    saveLayouts(layoutSlot, layouts, chromeless);
  }, [layouts]);

  // Give a newly selected worktree a layout. New worktrees inherit the split
  // of one already open, so the proportions the user chose carry across
  // instead of snapping back to 50/50 on every new worktree.
  useEffect(() => {
    if (!worktree) return;
    setLayouts((prev) => {
      if (prev[worktree.id]) return prev;
      // The shared store first, read *now* rather than at boot: another window
      // may have been using this worktree since, and its panes are the ones
      // that exist. Defaulting straight to a fresh layout here is precisely how
      // a second set would appear.
      const existing = readWorktreeLayout(worktree.id, chromeless, layoutSlot);
      if (existing) return { ...prev, [worktree.id]: existing };
      const seed = Object.values(prev)[0]?.ratio ?? DEFAULT_RATIO;
      return { ...prev, [worktree.id]: defaultLayout(seed) };
    });
  }, [worktree?.id]);

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
   */
  useEffect(() => {
    if (chromeless || !desktopWindow) return;
    return desktopWindow.onYieldWorktree(({ worktreeId }) => {
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
    });
  }, [chromeless]);

  /**
   * Tell the shell what this window is holding, so it knows who to ask.
   *
   * Only a main window: a detached one holds tabs transferred out of a worktree
   * its origin owns, and must never be asked to yield them — they are already
   * where they belong.
   */
  useEffect(() => {
    if (chromeless || !desktopWindow) return;
    void desktopWindow.holdsWorktrees(Object.keys(layouts).map(Number)).catch(() => {});
  }, [chromeless, layouts]);

  /**
   * Claim the worktree this window resolved to on its own, without a click.
   *
   * `selectWorktree` covers the rail; this covers boot, a restored `?wt=`, and
   * the fallback that lands on the first repo — all of which put a worktree on
   * screen without anyone choosing it. Without it the first window to open
   * claims nothing, and the second one is free to show the same worktree.
   */
  useEffect(() => {
    const shell = desktopWindow;
    if (chromeless || !shell || !worktree) return;
    let cancelled = false;
    void (async () => {
      const mine = await shell.claimWorktree(worktree.id, false).catch(() => null);
      if (cancelled || mine?.ok !== false) return;
      // **Refused, so this window must show something else.** Ignoring the
      // answer here was the hole that made the whole ownership model a
      // suggestion: `⌘N` opens on the last-selected worktree by design, which
      // is the one the window you pressed it in is showing — so the claim was
      // always refused, always ignored, and the new window rendered the same
      // panes and took their shells. `selectWorktree` honoured the refusal
      // because a click has somewhere to stay; a window opening has not.
      for (const candidate of worktrees) {
        if (candidate.id === worktree.id) continue;
        const free = await shell.claimWorktree(candidate.id, false).catch(() => null);
        if (cancelled) return;
        if (free?.ok) {
          setActiveRepoRoot(candidate.repo_root);
          setActiveWtKey(String(candidate.id));
          return;
        }
      }
      // Every worktree in this repo is already on screen somewhere. Say so
      // rather than showing a set of panes that belongs to another window.
      if (!cancelled) setClaimBlocked(true);
    })();
    return () => {
      cancelled = true;
    };
  }, [chromeless, worktree?.id, worktrees]);

  // Cleared as soon as this window is showing something it owns.
  useEffect(() => {
    if (claimBlocked) setClaimBlocked(false);
  }, [worktree?.id]);

  // Accepts an updater as well as a value: two panes can report a change in the
  // same commit (two browser panes both finishing a navigation), and a value
  // computed from the render's `layout` would silently discard the other write.
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

  // A focused native view swallows every keystroke, so the shell forwards the
  // palette accelerator back to us (it also moves focus to the page, or the
  // palette would open with the keyboard still pointed at the view).
  useEffect(
    () =>
      onBrowserAccelerator((accelerator) => {
        if (accelerator === "palette") setDialog({ kind: "search" });
      }),
    [],
  );

  /**
   * Forget worktrees that no longer exist — everywhere they are recorded.
   *
   * In memory, so their terminals get collected below. In the shared layout
   * store, because that write is a *merge*: dropping a worktree from `layouts`
   * leaves its stored panes untouched. And in the shell's claim map, so no window
   * goes on reporting a deleted worktree as one it is showing.
   *
   * All three matter for the same reason: `worktrees.id` is a plain `INTEGER
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
    forgetWorktreeLayouts(gone);
    void desktopWindow?.worktreesGone(gone).catch(() => {});
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
      const title = worktree ? `${label} — ${worktree.alias}` : label;
      void desktopWindow.setTitle(title).catch(() => {});
      return;
    }
    if (heldTabs.current) void desktopWindow.close().catch(() => {});
  }, [chromeless, layout, layouts, worktree?.id, worktree?.alias, repoList, activeWtKey]);

  // Terminals live outside React (see panes/terminalHost.ts), so nothing
  // unmounts them. The layouts are the whole record of which should exist;
  // anything else is a shell nobody can see, still holding one of the
  // daemon's session slots. Disposal also tells the daemon to hang the shell
  // up, which closing the socket deliberately does not.
  useEffect(() => {
    pruneTerminals(Object.values(layouts).flatMap(terminalIds));
    // Same contract for browser panes: a `WebContentsView` left behind is a
    // renderer process with nothing to paint into.
    pruneBrowsers(Object.values(layouts).flatMap(browserIds));
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
  // run's URLs live now (`panes/VeldLinks.tsx`). An existing blank pane is already
  // showing exactly that, so it gets focused instead of stacking up another one —
  // and the *last* of them, so asking twice lands in the same place rather than
  // cycling.
  const showVeldLinks = () => {
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
        `Could not delete ${w.alias}`,
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
      notifyError(`Could not dismiss the error on ${w.alias}`, e);
    }
    await refresh();
  };

  const deleteTrashedWorktree = async (w: Worktree) => {
    try {
      await api.deleteTrashedWorktree(w.id);
      notifyDone(`Deleting ${w.alias}`);
    } catch (e) {
      notifyError(`Could not delete ${w.alias}`, e);
    }
    await refresh();
  };

  const emptyTrash = async () => {
    if (!repo) return;
    try {
      const { queued } = await api.emptyTrash(repo.root);
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
      notifyDone(`Restored ${w.alias}`);
    } catch (e) {
      // 404 is the expected failure — the worker got there first — and saying so is
      // better than a generic error for a race the design admits to. Anything else
      // (a 500, a dead daemon) must NOT claim the worktree was removed, because it
      // is still there and the user would stop looking for it.
      const gone = e instanceof Error && /not found|already removed/i.test(e.message);
      notifyError(
        gone
          ? `${w.alias} has already been deleted`
          : `Could not restore ${w.alias}`,
        e,
      );
    }
    await refresh();
  };

  // Hover lives here so the crossfade survives LogoModeSwitch remounting
  // when it moves between the runs and IDE bars.
  const [switchHover, setSwitchHover] = useState(false);
  const modeSwitch = (
    <LogoModeSwitch
      mode={mode}
      onMode={setMode}
      hover={switchHover}
      onHover={setSwitchHover}
    />
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
      items.push({
        id: `wt:${w.id}`,
        group: "Worktrees",
        label: w.alias,
        hint: w.branch,
        alt: [w.branch],
        mark: { emoji: w.emoji, marker_color: w.marker_color },
        status: worktreeStatus(runsForWorktree(envs, w)),
        run: () => selectWorktree(w),
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
          label: `Stop ${w.alias}`,
          run: () => stopWorktree(w),
        });
        items.push({
          id: "run:restart",
          group: "Run",
          label: `Restart ${w.alias}`,
          run: () => restartWorktree(w),
        });
      } else if (!running && canStartWorktree(w)) {
        items.push({
          id: "run:start",
          group: "Run",
          label: `Start ${w.alias}`,
          run: () => startWorktree(w),
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
            label: `Share ${w.alias}`,
            hint: "copies the join link",
            alt: ["share", "invite", "join"],
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
            label: `Stop sharing ${w.alias}`,
            run: () => shareAction("stop sharing", () => api.stopShare(share.id)),
          });
        }
        if (live && runShares.web.length === 0) {
          items.push({
            id: "share:web",
            group: "Run",
            label: `Share ${w.alias} to the web`,
            alt: ["public", "gateway", "tunnel"],
            run: () => shareAction("web share", () => api.startShare(ref, { web: true })),
          });
        }
        for (const web of runShares.web) {
          items.push({
            id: `share:web-stop:${web.id}`,
            group: "Run",
            label: `Stop the public web share of ${w.alias}`,
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
        run: () => setDialog({ kind: "new-worktree" }),
      });
    }
    if (worktree) {
      const w = worktree;
      items.push({
        id: "wt:rename",
        group: "Worktree",
        label: `Rename ${w.alias}…`,
        run: () => setDialog({ kind: "rename", worktree: w }),
      });
      items.push({
        id: "wt:marker",
        group: "Worktree",
        label: `Change marker for ${w.alias}…`,
        hint: w.emoji,
        run: () => setDialog({ kind: "marker", worktree: w }),
      });
      items.push({
        id: "wt:copy-path",
        group: "Worktree",
        label: `Copy path of ${w.alias}`,
        hint: w.path,
        alt: [w.path],
        run: () => void navigator.clipboard.writeText(w.path),
      });
      if (!w.is_main) {
        items.push({
          id: "wt:remove",
          group: "Worktree",
          label: `Remove worktree ${w.alias}…`,
          run: () =>
            setDialog({ kind: "rename", worktree: w, deleteFocus: true }),
        });
      }
    }

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
        hint: worktree?.alias,
        run: () =>
          setLayout(
            addTabToFocused(layout, {
              id: newTabId(),
              kind: "terminal",
              title: "terminal",
            }),
          ),
      });
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
        run: showVeldLinks,
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
  // Beside the theme button, so both top bars get it — runs mode has no rail and
  // no palette, and would otherwise have no door to settings at all in a browser
  // tab, which has no application menu either.
  const settingsButton = (
    <Tooltip label="Settings (⌘,)">
      <ActionIcon
        size="md"
        variant="default"
        aria-label="Settings"
        onClick={() => setDialog({ kind: "settings" })}
      >
        <IconSettings size={14} />
      </ActionIcon>
    </Tooltip>
  );

  const themeButton = (
    <Tooltip
      label={`Theme: ${themePref === "auto" ? `system (${theme})` : themePref} — click to change`}
    >
      <ActionIcon size="md" variant="default" onClick={onCycleTheme}>
        {themePref === "auto" ? (
          <IconDeviceDesktop size={14} />
        ) : themePref === "light" ? (
          <IconSun size={14} />
        ) : (
          <IconMoon size={14} />
        )}
      </ActionIcon>
    </Tooltip>
  );

  if (mode === "runs") {
    return (
      <div className="frame">
        <RunsMode
          modeSwitch={modeSwitch}
          themeButton={themeButton}
          settingsButton={settingsButton}
        />
      </div>
    );
  }

  /**
   * The top-level Sharing surface (#152: one surface, not a relay-details dump).
   *
   * A popover rather than a `Menu`, because its content is interactive — the
   * auto-accept checkbox and the copy buttons must not close it on click. It is
   * portalled, so `overlayGuard` hides the embedded browser panes while it is
   * open without this having to say so.
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
  const sharingSurface =
    worktree && (canRunWorktreeNow(worktree) || sharingActive) ? (
      <Popover position="bottom-end" width={430} shadow="md" withinPortal>
        <Popover.Target>
          <Button
            size="compact-sm"
            variant={sharingActive ? "light" : "default"}
            color={sharingActive ? "green" : undefined}
            leftSection={<IconShare2 size={14} />}
            title={
              sharingActive
                ? "This run is shared — open for links and connections"
                : "Share this run"
            }
          >
            {sharingActive ? "Sharing" : "Share"}
          </Button>
        </Popover.Target>
        <Popover.Dropdown p={0}>
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
        </Popover.Dropdown>
      </Popover>
    ) : null;

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
            urlsEmptyHint={
              worktree.has_veld_config
                ? "Start the run and its services appear here."
                : "This worktree has no veld.json, so there is nothing to run."
            }
            sessions={sessions}
            onAddSession={nextSession ? addSession : undefined}
            onRemoveSession={removeSession}
            quickSwitches={quickSwitches}
            runCtx={runCtx}
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
        startConfig={
          worktree && canRunWorktreeNow(worktree) ? (
            <StartConfig
              worktree={worktree}
              value={effectiveStart}
              onChange={(sel) => setStartRaw(JSON.stringify(sel))}
            />
          ) : null
        }
        canStart={worktree ? canStartWorktree(worktree) : false}
        running={status !== "stopped"}
        pending={pendingFor(worktree)}
        run={run}
        urls={urls}
        sharing={sharingSurface}
        onShowVeldLinks={layout && showVeldLinks}
        onSelectRepo={(root) => {
          setActiveRepoRoot(root);
          setActiveWtKey("");
        }}
        onImport={() => setDialog({ kind: "import" })}
        onRemoveRepo={() => repo && setDialog({ kind: "remove-repo", repo })}
        onStart={() => worktree && startWorktree(worktree)}
        onStop={() => worktree && stopWorktree(worktree)}
        onRestart={() => worktree && restartWorktree(worktree)}
        onSearch={() => setDialog({ kind: "search" })}
        themeButton={themeButton}
        settingsButton={settingsButton}
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
        <div className="center-page">
          <Button size="md" onClick={() => setDialog({ kind: "import" })}>
            Import your first project
          </Button>
        </div>
      ) : claimBlocked ? (
        // Every worktree is already on screen in another window. Saying so beats
        // the alternative this replaced — rendering a set of panes that belongs
        // to a different window, and taking its shells on the way.
        <div className="center-page">
          <p>Every worktree is already open in another window.</p>
          <Button size="md" variant="default" onClick={() => setDialog({ kind: "new-worktree" })}>
            Create a worktree
          </Button>
        </div>
      ) : (
        <div className="workspace">
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
            onSelect={selectWorktree}
            onAdd={() => setDialog({ kind: "new-worktree" })}
            onMenu={(e, w) => worktreeMenu(w)(e)}
            onStart={startWorktree}
            onStop={stopWorktree}
            onAddLane={() => setDialog({ kind: "new-lane" })}
            onLaneMenu={(e, lane) => laneMenu(lane)(e)}
            onMove={moveWorktreeTo}
            onRestore={restoreWorktree}
            onEmptyTrash={emptyTrash}
          />
          {worktree && layout && (
            <PaneArea
              layout={layout}
              onLayout={setLayout}
              worktreeId={worktree.id}
              repoRoot={worktree.repo_root}
              serviceUrls={urls}
              quicklinks={worktree.ide.quicklinks}
              urlsEmptyHint={
                worktree.has_veld_config
                  ? "Start the run and its services appear here."
                  : "This worktree has no veld.json, so there is nothing to run."
              }
              sessions={sessions}
              onAddSession={nextSession ? addSession : undefined}
              onRemoveSession={removeSession}
              quickSwitches={quickSwitches}
              runCtx={runCtx}
            />
          )}
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
          onCreate={async (body) => {
            const created = await api.createWorktree({
              repo_root: repo.root,
              ...body,
            });
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
            await api.removeRepo(dialog.repo.root);
            setActiveRepoRoot("");
            setActiveWtKey("");
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "rename" && (
        <RenameWorktreeDialog
          current={dialog.worktree.alias}
          isMain={dialog.worktree.is_main}
          /* Read off the LIVE row, not the one captured when the dialog opened: a
             background removal can fail while it is open, and the force affordance
             has to appear when the refusal arrives. */
          trashError={
            worktrees.find((w) => w.id === dialog.worktree.id)?.trash_error ?? ""
          }
          deleteFocus={dialog.deleteFocus ?? false}
          onClose={closeDialog}
          onRename={async (alias) => {
            await api.patchWorktree(dialog.worktree.id, { alias });
            await refresh();
            closeDialog();
          }}
          onDelete={async (force) => {
            await api.deleteWorktree(dialog.worktree.id, force);
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "marker" && (
        <ChangeMarkerDialog
          current={dialog.worktree.emoji}
          currentColor={dialog.worktree.marker_color}
          alias={dialog.worktree.alias}
          worktreeId={dialog.worktree.id}
          usedBy={markerUsedBy.emoji}
          colorUsedBy={markerUsedBy.color}
          style={markerStyle(settings ?? {})}
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
          title="New lane"
          confirmLabel="Create lane"
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
          title="Rename lane"
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
      {dialog.kind === "settings" && (
        <SettingsDialog
          settings={settings}
          saving={savingSettings}
          error={settingsError}
          onSave={saveSettings}
          onClose={closeDialog}
        />
      )}
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

/** A single run's status as one of the rail's `.dot` classes. */
function runStatusClass(status: string): WorktreeStatus {
  if (status === "running") return "running";
  if (status === "failed") return "failed";
  if (status === "stopped") return "stopped";
  return "partial";
}

function TopBar(props: {
  modeSwitch: React.ReactNode;
  repos: Repo[];
  repo: Repo | null;
  worktree: Worktree | null;
  startConfig: React.ReactNode;
  canStart: boolean;
  running: boolean;
  pending: PendingAction | null;
  run: { name: string; status: string } | null;
  urls: Array<[string, string]>;
  /** The Sharing surface, built by the app (it owns the shares poll). */
  sharing: React.ReactNode;
  /** Open a pane on the run's URLs. Absent when there is no layout to open into. */
  onShowVeldLinks: (() => void) | undefined;
  onSelectRepo: (root: string) => void;
  onImport: () => void;
  onRemoveRepo: () => void;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
  onSearch: () => void;
  themeButton: React.ReactNode;
  settingsButton: React.ReactNode;
}) {
  const { worktree, run } = props;
  const repoAvailable = props.repo?.available ?? false;
  // No run controls for a repo we can't see on disk — git/veld actions would
  // only fail later with a worse error.
  const canRun = !!worktree?.has_veld_config && repoAvailable;
  return (
    <div className={topbarClass}>
      {props.modeSwitch}
      {props.repos.length > 0 && (
        <Select
          title="Switch project"
          size="xs"
          w={170}
          allowDeselect={false}
          value={props.repo?.root ?? null}
          onChange={(v) => v && props.onSelectRepo(v)}
          data={props.repos.map((r) => ({
            value: r.root,
            label: r.available ? r.name : `${r.name} (unavailable)`,
          }))}
          comboboxProps={{ width: 240, position: "bottom-start" }}
          className="mono-field"
          styles={{
            option: { fontFamily: "var(--mantine-font-family-monospace)" },
          }}
        />
      )}
      <Menu position="bottom-start" width={200}>
        <Menu.Target>
          <ActionIcon size="md" variant="default" title="Project actions">
            <IconDots size={14} />
          </ActionIcon>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Item
            leftSection={<IconFolderPlus size={14} />}
            onClick={props.onImport}
          >
            Import repository…
          </Menu.Item>
          <Menu.Item
            color="red"
            leftSection={<IconTrash size={14} />}
            disabled={!props.repo}
            onClick={props.onRemoveRepo}
          >
            Remove project…
          </Menu.Item>
        </Menu.Dropdown>
      </Menu>
      {worktree && (
        <>
          <div className="sep" />
          {canRun && props.startConfig}
          {canRun && (
            <>
              {/* The spinner belongs on the button that was pressed. Putting
                  it on play/stop for every action made a restart look like a
                  stop in progress. */}
              <Tooltip label={props.running ? "Stop run" : "Start run"}>
                <ActionIcon
                  size="md"
                  variant="light"
                  color={props.running ? "red" : "green"}
                  loading={props.pending === "start" || props.pending === "stop"}
                  disabled={
                    props.pending !== null ||
                    (!props.running && !props.canStart)
                  }
                  onClick={props.running ? props.onStop : props.onStart}
                >
                  {props.running ? (
                    <IconPlayerStopFilled size={13} />
                  ) : (
                    <IconPlayerPlayFilled size={13} />
                  )}
                </ActionIcon>
              </Tooltip>
              <Tooltip label="Restart">
                <ActionIcon
                  size="md"
                  variant="default"
                  loading={props.pending === "restart"}
                  disabled={!props.running || props.pending !== null}
                  onClick={props.onRestart}
                >
                  <IconRefresh size={13} />
                </ActionIcon>
              </Tooltip>
              {/* Status is a dot, not a word: the text was long enough to be
                  clipped in a crowded bar, and it duplicated what the
                  start/stop icon already says. */}
              {run && (
                <Tooltip
                  label={
                    props.pending
                      ? `Run ${run.name}: ${props.pending}…`
                      : `Run ${run.name}: ${run.status}`
                  }
                >
                  <span
                    className={`dot ${runStatusClass(run.status)}`}
                    role="img"
                    aria-label={`Run ${run.name}: ${props.pending ?? run.status}`}
                  />
                </Tooltip>
              )}
              {run && (
                // Opens a browser pane on the run's URLs, not an overlay of its
                // own: the URLs live in whichever pane is about to need them, and
                // a modal listing them was a second, inconsistent surface that
                // also covered the panes it was talking about.
                <Button
                  size="compact-sm"
                  variant="default"
                  leftSection={<IconWorld size={14} />}
                  onClick={props.onShowVeldLinks}
                  disabled={!props.onShowVeldLinks}
                  title={`Open the run's URLs in a pane`}
                >
                  {props.urls.length}
                </Button>
              )}
              {props.sharing}
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
      <Tooltip label="Search (⌘K)">
        <ActionIcon size="md" variant="default" onClick={props.onSearch}>
          <IconSearch size={14} />
        </ActionIcon>
      </Tooltip>
      {props.settingsButton}
      {props.themeButton}
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
  /** Worktrees another window is showing. Clicking one goes there instead of
   *  opening it here, so it is marked rather than left to surprise. */
  elsewhere: Set<number>;
  onToggle: () => void;
  onWidth: (w: number) => void;
  onSelect: (w: Worktree) => void;
  onAdd: () => void;
  onMenu: (e: React.MouseEvent, w: Worktree) => void;
  onStart: (w: Worktree) => void;
  onStop: (w: Worktree) => void;
  onAddLane: () => void;
  onLaneMenu: (e: React.MouseEvent, lane: string) => void;
  onMove: (path: string, toLane: string, toIndex: number) => void;
  onRestore: (w: Worktree) => void;
  onEmptyTrash: () => void;
}) {
  const groups = railGroups(props.worktrees, props.lanes);
  // Drag state is local: it is transient pointer feedback, and lifting it would
  // re-render the pane area on every dragover.
  const [dragPath, setDragPath] = useState<string | null>(null);
  const [dropAt, setDropAt] = useState<{ key: string; index: number } | null>(
    null,
  );
  // Suppresses the rail's width transition for the duration of a resize drag. The
  // transition exists for the collapse/expand toggle, where 236px→64px should
  // animate; during a drag it re-animates on every pointer move, so the edge
  // visibly lags behind the cursor instead of tracking it.
  const [resizing, setResizing] = useState(false);
  const endDrag = () => {
    setDragPath(null);
    setDropAt(null);
  };
  // Dropping is disabled while the rail is collapsed. A 64px row shows only a
  // marker, so there is no way to see *where* a drop would land — and a reorder
  // whose result you cannot see is a reorder you did not mean.
  const canDrag = props.wide;

  /**
   * Drop handlers for a section, resolving to an insertion index.
   *
   * `half` splits a row into its top and bottom halves so the gap *below* the last
   * row is reachable — without it, appending to a group was impossible: the row's
   * own handler stops propagation before the section's `index = length` handler
   * runs, and a flex column has no blank space under its last child to aim at.
   */
  const dropZone = (group: RailGroup, index: number, half = false) => ({
    onDragOver: (e: React.DragEvent) => {
      if (!dragPath || group.pinned) return;
      // Both required: preventDefault marks the element a valid drop target, and
      // without stopPropagation the enclosing section's own zone also fires and the
      // indicator flickers between the two.
      e.preventDefault();
      e.stopPropagation();
      setDropAt({ key: group.key, index: index + (half && below(e) ? 1 : 0) });
    },
    onDrop: (e: React.DragEvent) => {
      if (!dragPath || group.pinned) return;
      e.preventDefault();
      e.stopPropagation();
      props.onMove(dragPath, group.key, index + (half && below(e) ? 1 : 0));
      endDrag();
    },
  });

  return (
    <div
      className={`rail${props.wide ? " wide" : ""}${resizing ? " resizing" : ""}`}
      style={props.wide ? { width: props.width } : undefined}
    >
      <div className="rail-head">
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          title="Expand / collapse"
          onClick={props.onToggle}
        >
          {props.wide ? <IconChevronLeft size={13} /> : <IconChevronRight size={13} />}
        </ActionIcon>
        {props.wide && (
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            title="New lane"
            onClick={props.onAddLane}
          >
            <IconFolderPlus size={14} />
          </ActionIcon>
        )}
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          title="New worktree"
          onClick={props.onAdd}
        >
          <IconPlus size={14} />
        </ActionIcon>
      </div>
      <div className="rail-list" onDragEnd={endDrag}>
        {groups.map((group) => (
          <div
            key={group.key}
            className={`rail-group${group.key === TRASH_LANE ? " trash" : ""}${
              // Lit while a drag is over this section, so the target reads as a
              // whole area and not only as a caret between two rows. This is the
              // only feedback an EMPTY lane can give.
              dragPath && !group.pinned && dropAt?.key === group.key
                ? " drop-in"
                : ""
            }`}
            // The section itself is the fallback target, and it resolves to its
            // FIRST position rather than its last. What actually reaches this
            // handler is the header and the padding above it — the rows stop
            // propagation — and both sit at the *top* of the section, so
            // appending here contradicted where the pointer was: dragging down
            // into a lane crossed its header and the caret jumped to the bottom.
            // Appending is still reachable, and unambiguously so: it is the lower
            // half of the last row.
            {...dropZone(group, 0)}
          >
            {group.label !== null && props.wide && (
              <div
                className="lane-head"
                onContextMenu={
                  group.pinned
                    ? undefined
                    : (e) => props.onLaneMenu(e, group.lane)
                }
              >
                <span className="lane-name">{group.label}</span>
                <span className="lane-count">{group.worktrees.length}</span>
                {/* The trash's own action, in the place a lane keeps its menu:
                    emptying it is the only thing the section as a whole can do. */}
                {group.key === TRASH_LANE && (
                  <button
                    type="button"
                    className="lane-edit"
                    title="Delete everything in the trash"
                    aria-label="Empty the trash"
                    onClick={(e) => {
                      e.stopPropagation();
                      props.onEmptyTrash();
                    }}
                  >
                    <IconTrash size={12} />
                  </button>
                )}
                {/* Right-click alone is not an affordance — nothing on screen says
                    the header has a menu. The same ⋮ the rows carry, so the two read
                    as the same gesture. Not on a pinned section: the trash has no
                    lane to rename or delete. */}
                {!group.pinned && (
                  <button
                    type="button"
                    className="lane-edit"
                    title={`Lane menu for ${group.label}`}
                    aria-label={`Menu for lane ${group.label}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      props.onLaneMenu(e, group.lane);
                    }}
                  >
                    <IconDotsVertical size={12} />
                  </button>
                )}
              </div>
            )}
            {/* An empty lane needs a target you can see and hit. Without this the
                only droppable area was the header's few pixels of margin, so a lane
                you had just made looked like it refused worktrees. */}
            {props.wide && !group.pinned && group.worktrees.length === 0 && (
              <div className="lane-empty">
                {dragPath ? "Drop here" : "Empty — drag a worktree in"}
              </div>
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
              const status = worktreeStatus(runsForWorktree(props.envs, w));
              const running = status !== "stopped";
              const pending = props.pendingFor(w);
              const trashed = w.trashed_at !== "";
              // Inline controls are wide-only — a 64px collapsed row has no space
              // for them. Right-click reaches the same actions in either mode.
              // A worktree on its way out gets none: it cannot be started, and a
              // run control on it would be a button that only ever fails.
              const showRunControl = props.wide && !trashed && props.canRun(w);
              const away = props.elsewhere.has(w.id);
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
                  className={`wt-row${props.active?.id === w.id ? " active" : ""}${props.wide ? "" : " slim"}${away ? " away" : ""}${trashed ? " trashed" : ""}${w.trash_error ? " failed-remove" : ""}${dragPath === w.path ? " dragging" : ""}`}
                  title={
                    trashed
                      ? `${w.alias} — in the trash, still on disk`
                      : w.trash_error
                        ? `${w.alias} — could not be deleted: ${w.trash_error}`
                        : away
                          ? `${w.alias} — ${w.branch} (open in another window)`
                          : `${w.alias} — ${w.branch}`
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
                  <span className={`dot ${status}`} />
                  {/* Before the alias, where the eye already is for the dot — and
                      rendered in the collapsed rail too, which is exactly where a
                      greyed row alone would be too subtle to read. */}
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
                  {props.wide && <span className="wt-alias">{w.alias}</span>}
                  {props.wide && (
                    <span className="wt-branch">
                      {/* The branch column carries the removal failure, because
                          that is the one thing about this row that is not the
                          branch any more. Trashed rows say what is happening to
                          them; the branch is not the news. */}
                      {trashed
                        ? "in trash"
                        : w.trash_error
                          ? w.trash_error
                          : w.branch}
                    </span>
                  )}
                  {showRunControl && (
                    <button
                      type="button"
                      className={`wt-run${running ? " on" : ""}`}
                      title={
                        pending
                          ? `${pending}…`
                          : running
                            ? `Stop ${w.alias}`
                            : `Start ${w.alias}`
                      }
                      aria-label={running ? `Stop ${w.alias}` : `Start ${w.alias}`}
                      // Mirrors the context menu and the palette. Without the
                      // start guard the button looked live but its click hit a
                      // no-op for a worktree with no presets and no nodes.
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
                      {pending ? (
                        // The spinner carries the action's colour, so a row that
                        // is stopping reads as stopping and not as starting.
                        <Loader size={10} color={actionColor(pending)} />
                      ) : running ? (
                        <IconPlayerStopFilled size={10} />
                      ) : (
                        <IconPlayerPlayFilled size={10} />
                      )}
                    </button>
                  )}
                  {props.wide && !trashed && (
                    <button
                      type="button"
                      className="wt-edit"
                      title="Worktree menu"
                      aria-label={`Menu for ${w.alias}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        props.onMenu(e, w);
                      }}
                    >
                      <IconDotsVertical size={12} />
                    </button>
                  )}
                  {props.wide && trashed && (
                    <button
                      type="button"
                      className="wt-edit"
                      title={`Restore ${w.alias}`}
                      aria-label={`Restore ${w.alias}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        props.onRestore(w);
                      }}
                    >
                      <IconArrowBackUp size={12} />
                    </button>
                  )}
                  </div>
                  {caretAfter && <RailCaret />}
                </Fragment>
              );
            })}
          </div>
        ))}
      </div>
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
  status?: WorktreeStatus;
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
                  {item.status && <span className={`dot ${item.status}`} />}
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
