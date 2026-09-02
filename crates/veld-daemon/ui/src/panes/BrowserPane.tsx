/**
 * One embedded browser pane: chrome (history, address, session) plus the slot
 * the live view is reparented into.
 *
 * The view itself is owned by `browserHost` and outlives this component — a
 * remount would reload the page and throw away scroll position, form state and
 * whatever the dev server hot-reloaded into it. So this holds no view state:
 * it reads through `browserStatus` and re-renders on `subscribeBrowser`.
 *
 * Under Electron the content is a native `WebContentsView` positioned over the
 * slot, which means **nothing here may render on top of the slot** — a native
 * view paints over DOM regardless of z-index. Chrome goes above it, status
 * below it, and overlays that would cover it are handled by `overlayGuard`.
 */

import { ActionIcon, Button, Loader, Menu, Tooltip } from "@mantine/core";
import {
  IconAlertTriangle,
  IconArrowLeft,
  IconArrowRight,
  IconBookmark,
  IconBug,
  IconCheck,
  IconChevronDown,
  IconChevronUp,
  IconClockExclamation,
  IconCode,
  IconDeviceMobile,
  IconDevices,
  IconExternalLink,
  IconInfinity,
  IconLockOff,
  IconMinus,
  IconMoon,
  IconPlugConnectedX,
  IconPlus,
  IconLivePhoto,
  IconRefresh,
  IconRestore,
  IconRotateClockwise,
  IconSearch,
  IconShieldLock,
  IconSun,
  IconSunMoon,
  IconTrash,
  IconUserCircle,
  IconWorldOff,
  IconX,
} from "@tabler/icons-react";
import { useEffect, useReducer, useRef, useState } from "react";
import {
  BROWSER_PROFILES,
  BROWSER_PROFILE_COLORS,
  type BrowserProfile,
  MAX_EXTRA_SESSIONS,
  type PaneTab,
  browserProfileLabel,
  fileLabel,
  filePathIn,
  resolveAddress,
  urlForProfile,
  urlLabel,
} from "./model";
import {
  BookmarksButton,
  BookmarksModal,
  FilesButton,
  FilesModal,
  PlaceList,
} from "./PlaceList";
import {
  indexOfPlaceUrl,
  inlineFiles,
  pickSuggestion,
  placesFor,
  stepIndex,
  suggestionsFor,
} from "./places";
import {
  DEFAULT_ZOOM,
  DEVICE_GROUPS,
  DEVICE_PADDING,
  DEVICE_PRESETS,
  type DevicePreset,
  HANDLE_CORNER_GAP,
  HANDLE_CORNER_HIT_BLEED,
  HANDLE_EDGE_GAP,
  HANDLE_HIT_BLEED,
  HANDLE_LENGTH,
  MAX_DEVICE_PX,
  MEDIA_FEATURES,
  MEDIA_LABELS,
  type MediaFeature,
  MIN_DEVICE_PX,
  type PaneEmulation,
  type PaneMedia,
  QUICK_DEVICE,
  RESPONSIVE_DEVICE,
  chromeVersionFrom,
  clampZoom,
  customEmulation,
  deviceLayout,
  dragSize,
  edgePinned,
  emulationForPreset,
  emulationLabel,
  emulationSize,
  formatPercent,
  formatZoom,
  nextColorScheme,
  orientationLabel,
  presetById,
  resizeEmulation,
  responsiveEmulation,
  rotateEmulation,
  withMediaFeature,
  withMobileUserAgent,
  zoomStep,
} from "./devices";
import { type BrowserErrorKind, describeBrowserError } from "./browserError";
import {
  abandonPermission,
  answerPermission,
  browserBackend,
  browserCommand,
  browserDevTools,
  browserStatus,
  clearBrowserSession,
  findNext,
  findPrevious,
  findSupported,
  mountBrowser,
  navigateBrowser,
  onBrowserAccelerator,
  onBrowserPointer,
  onFindResult,
  onPermissionRequest,
  onPermissionSettings,
  originOf,
  paneCovers,
  previewBrowserResize,
  noteShownFileMtime,
  reloadBrowser,
  setWatchOverride,
  shownFileMtime,
  watchOverride,
  requestPermissionSettings,
  setBrowserEmulation,
  setBrowserMedia,
  setBrowserOverlay,
  setBrowserResizing,
  setBrowserZoom,
  setPermission,
  startFind,
  stopFind,
  subscribeBrowser,
  unmountBrowser,
  type PermissionPrompt,
  type PermissionSetting,
} from "./browserHost";
import {
  PERMISSION_LABELS,
  effectiveLabel,
  permissionSentence,
  userChoice,
} from "./permissions";
import { api, type Quicklink, type ViewableFile } from "../api";
import type { QuickSwitchPrefs } from "../shared/settings";

/**
 * The Chromium version the shell is built on, for the mobile user-agent presets.
 *
 * Read off this document's own UA because that is the browser which will make the
 * request: a hardcoded Chrome version in a preset goes stale with every Electron
 * bump, and a UA claiming a release two years older than the engine sending it is
 * exactly the kind of thing a server's feature gating notices.
 */
const HOST_CHROME = chromeVersionFrom(
  typeof navigator === "undefined" ? undefined : navigator.userAgent,
);

/**
 * Shared tooltip settings for the pane's chrome.
 *
 * Mantine rather than the `title` attribute, which takes over a second to appear
 * and is styled by the OS. `position: top` is load-bearing: a tooltip opening
 * downwards lands on the pane's slot, where a native view paints over DOM and it
 * would simply not be there. Portalled — Mantine's default — so `.dock-body`'s
 * clipping cannot eat it, and `overlayGuard` deliberately does not match tooltips,
 * so hovering the chrome never hides the page underneath.
 */
const TIP = {
  position: "top",
  withArrow: true,
  openDelay: 350,
  fz: "xs",
} as const;

/** A session's identity marker, or `null` for the default slot (which stays
 *  unmarked, so the common case has nothing to read). */
export function browserTabDot(tab: PaneTab): string | null {
  return BROWSER_PROFILE_COLORS[tab.profile ?? "default"];
}

/** The icon for each error kind. The wording lives in `browserError.ts`, which
 *  is testable without a DOM; only the glyph is a rendering decision. */
const ERROR_ICONS: Record<BrowserErrorKind, React.ReactNode> = {
  unreachable: <IconPlugConnectedX size={26} />,
  dns: <IconWorldOff size={26} />,
  timeout: <IconClockExclamation size={26} />,
  cert: <IconLockOff size={26} />,
  crash: <IconBug size={26} />,
  generic: <IconAlertTriangle size={26} />,
};

/**
 * A resize handle's hit-area reach, as the custom property the stylesheet computes its
 * `inset` from.
 *
 * Inline rather than a literal in CSS so the number has **one** owner: it is the same
 * constant `MIN_DEVICE_PADDING` is computed from, and a literal in the stylesheet could
 * be edited to reach past the gap into the OS window-resize grip while the constant that
 * documents the floor went on claiming otherwise.
 */
function bleed(px: number): React.CSSProperties {
  return { "--handle-bleed": `${px}px` } as React.CSSProperties;
}

/**
 * What a `prefers-color-scheme` override is called, with `undefined` reading as
 * **System** — the absence of an override, which is a state the chrome has to name
 * even though nothing is stored for it.
 *
 * Falls through to the raw value for a scheme this build has no wording for, so a
 * pane restored from a newer layout says what it is rather than claiming System.
 */
function schemeName(value: string | undefined): string {
  if (!value) return "System";
  // The `??` keeps this total; it is not a live case. `sanitizeMedia` drops any
  // scheme outside `MEDIA_FEATURES` on every layout load (`panes/model.ts`), so the
  // real guard is upstream and a value only ever arrives as light or dark.
  return MEDIA_LABELS["prefers-color-scheme"].values[value] ?? value;
}

/**
 * How often a watched file's timestamp is checked.
 *
 * A second is fast enough that a rewritten deck feels immediate and slow enough
 * that the cost is invisible — the request is a `stat` on one path, and only the
 * *focused* pane runs it, because an inactive tab's `BrowserPane` is unmounted
 * (its view lives on in `browserHost`, but this component does not).
 */
const FILE_WATCH_INTERVAL_MS = 1000;

/** The suggestion panel's DOM id, which the address bar's `aria-controls` names. */
function suggestId(paneId: string): string {
  return `suggest-${paneId}`;
}

/** One coloured session pip. */
function SessionDot(props: { color: string | null; size?: number }) {
  const size = props.size ?? 8;
  return (
    <span
      className="session-dot"
      aria-hidden
      style={{
        width: size,
        height: size,
        background: props.color ?? "var(--faint)",
        // The default slot reads as an outline rather than a colour, so it is
        // clearly "no session chosen" and not one more colour to learn.
        opacity: props.color ? 1 : 0.5,
      }}
    />
  );
}

export function BrowserPane(props: {
  tab: PaneTab;
  /** Persist url/title/profile back into the layout. */
  onTab: (patch: Partial<Omit<PaneTab, "id" | "kind">>) => void;
  /** The run's live URLs, which an empty pane shows as its start page. */
  serviceUrls: Array<[string, string]>;
  /** The project's own links from `ide.quicklinks`, shown beside the veld URLs. */
  quicklinks: Quicklink[];
  /** The worktree's recently-edited viewable files, newest first. */
  files: ViewableFile[];
  /** Whether that list is still being fetched for this worktree. */
  filesLoading: boolean;
  /** Whether the daemon can serve local files at all. */
  filesServing: boolean;
  /** Which worktree this pane belongs to — the file-stat route is scoped to it. */
  worktreeId: number;
  /** `ViewableFiles.root` for this worktree: the `<origin>/<grant>/` prefix that
   *  says a URL in this pane is a local file, and turns it back into a path. Null
   *  while the list is still loading, or when the daemon cannot serve files. */
  filesRoot: string | null;
  /** `files.watchByDefault`: whether a pane showing a local file watches it.
   *  Each pane can override this for itself. */
  watchFilesByDefault: boolean;
  /** Why there are none — only the app knows (no run, or no veld.json). */
  urlsEmptyHint: string;
  /** Which one-click toggles the chrome shows, from the settings store. */
  quickSwitches: QuickSwitchPrefs;
  /** `browser.searchUrl` — where words that are not an address go, or `""`. */
  searchUrl: string;
  /** The sessions that exist for this worktree, in slot order. */
  sessions: BrowserProfile[];
  /** Create a session and move this pane onto it. Absent at the slot cap. */
  onAddSession: (() => void) | undefined;
  onRemoveSession: (profile: BrowserProfile) => void;
}) {
  const { tab, onTab } = props;
  const id = tab.id;
  const profile: BrowserProfile = tab.profile ?? "default";
  const slot = useRef<HTMLDivElement>(null);
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const iframeBackend = browserBackend === "iframe";

  // The layout is the record for the emulated device and the zoom; `browserHost`
  // holds the live copy. Read here so both the chrome and the mount below work
  // off the state that gets persisted.
  const emulation = tab.emulation ?? null;
  const zoom = tab.zoom ?? DEFAULT_ZOOM;
  // Beside the other two because it is the same kind of state — per-`WebContents`,
  // recorded in the layout — and because the mount effect below needs it.
  const media = tab.media ?? null;

  // Refs for the same reason `currentUrl` is one: `mountBrowser` uses these only
  // when it *creates* a view — a first mount, or a session switch, which rebuilds
  // one — and making them effect dependencies would remount the view, reloading
  // the page, every time you picked a device or nudged the zoom.
  const currentEmulation = useRef(emulation);
  currentEmulation.current = emulation;
  const currentZoom = useRef(zoom);
  currentZoom.current = zoom;
  // Same reason as the two above, and it was missing: emulation state is
  // per-`WebContents`, so a pane that switches session — or retries a refused
  // create — rebuilds its view and must be handed everything back. Without this
  // the layout still said "dark" while the new view emulated nothing.
  const currentMedia = useRef(media);
  currentMedia.current = media;

  useEffect(() => {
    const el = slot.current;
    if (!el) return;
    // Mount first, then subscribe: a profile change disposes the old view
    // (dropping its listeners) and creates a new one, so subscribing first
    // would attach to the view that is about to go away.
    mountBrowser(id, el, {
      // A session's own remembered position, falling back to the pane's current
      // `url` for a session never visited in this tab. A profile change
      // re-runs this effect and recreates the view, so it is here — not in a
      // navigation — that the per-session URL is chosen.
      url: urlForProfile(tab, profile),
      profile,
      emulation: currentEmulation.current,
      media: currentMedia.current,
      zoom: currentZoom.current,
    });
    const unsubscribe = subscribeBrowser(id, bump);
    return () => {
      unsubscribe();
      // The find bar is this component's own state, and unmounting here is a
      // detach, not a close — the view (and Chromium's highlights on it) lives
      // on. Without this, switching away from a pane mid-search leaves
      // highlights painted on a page with no bar and no React state left to
      // clear them from; returning to the tab shows a plain closed pane over a
      // still-highlighted page.
      stopFind(id);
      // Detach only — the view lives on; `pruneBrowsers` in App.tsx is what
      // closes one for good.
      unmountBrowser(id);
    };
  }, [id, profile]);

  const state = browserStatus(id);

  // Persist what the page did, so a reload returns where the pane was left
  // rather than where it was opened. `updateTab` de-duplicates, so a
  // `did-navigate` re-reporting the same URL costs nothing.
  //
  // One effect for both fields, not two: a navigation changes URL and title
  // together, and two patches in one commit is how a title write ends up
  // clobbering the URL one. Only a *non-empty* title — an empty one must not
  // overwrite the name the tab was opened with (a service name beats a
  // hostname), and the iframe backend can never read one at all.
  useEffect(() => {
    const patch: Partial<Omit<PaneTab, "id" | "kind">> = {};
    if (state.url && state.url !== tab.url) patch.url = state.url;
    // Record the session's own position too, so switching back to this session
    // restores where it was rather than inheriting whatever another session's
    // navigation left on `url`.
    if (state.url && state.url !== tab.urls?.[profile]) {
      patch.urls = { ...(tab.urls ?? {}), [profile]: state.url };
    }
    if (state.title && state.title !== tab.title) patch.title = state.title;
    if (Object.keys(patch).length > 0) onTab(patch);
  }, [state.url, state.title]);

  // The address bar is a text field, so it cannot be driven straight off
  // `state.url` — that would rewrite a half-typed address on every background
  // navigation. It follows the view only while unfocused.
  const [draft, setDraft] = useState(tab.url ?? "");
  const [editing, setEditing] = useState(false);
  useEffect(() => {
    if (!editing) setDraft(state.url || tab.url || "");
  }, [state.url, editing]);

  // Which screen stands in for the page. `paneCovers` is the *same* predicate that
  // hides the native view in browserHost — shared rather than restated, because the
  // two disagreeing means either a screen painted under a live page or a pane that
  // stays blank, and neither is visible in the browser build.
  const covered = paneCovers(state, tab.url);
  const failure = state.error ? describeBrowserError(state.error) : null;
  // Veld's own UI, refused. Ranks above the others: nothing failed and nothing is
  // loading, so neither of those screens applies.
  const nested = state.nested;
  const chooser = covered && !failure && !nested && !state.url && !tab.url;
  /**
   * Whether the draft is something the user typed.
   *
   * Not `editing`: focusing the bar selects the address already in it, so filtering
   * by the draft would open the panel filtered by the page you are on — which
   * matches nothing and offers "Go to <the URL you are already on>". Until a
   * keystroke, the panel is the unfiltered list of places.
   */
  const [typed, setTyped] = useState(false);

  // Suggestions: the run's URLs, the project's bookmarks, and what has been typed.
  // The same list the new-pane chooser shows, so picking and typing are not two
  // different ways of naming the same places.
  //
  // **Two lists and one visible one.** `allPlaces` is everything, which is what
  // typing searches. `startPlaces` is what the blank-pane start page offers
  // unprompted — the same three-from-the-last-day rule the chooser uses, because
  // that screen answers the same question and a longer list there was a directory
  // listing under a "Where to?" heading.
  //
  // They collapse to one `places` rather than being rendered separately, and that is
  // not tidiness: `placeKey`, `suggestions`, the arrow keys and what Enter opens are
  // all derived from this, so two lists would mean the keyboard indexing one and the
  // screen showing the other. The panel and the start page are mutually exclusive
  // (`suggesting && !chooser` below), so exactly one of these is ever on screen.
  const allPlaces = placesFor(props.serviceUrls, props.quicklinks, props.files);
  const startPlaces = placesFor(
    props.serviceUrls,
    props.quicklinks,
    inlineFiles(props.files, {
      hasRunUrls: props.serviceUrls.length > 0,
      now: Date.now(),
    }),
  );
  const places = chooser && !typed ? startPlaces : allPlaces;
  /**
   * What the bookmarks modal shows: **every** bookmark, never the filtered set.
   *
   * `suggestions.bookmarks` is the ones currently collapsed *out of the list*, which
   * is the right thing to count on a button and the wrong thing to put in the modal —
   * with text typed it is empty, and the modal is "every address this project
   * declares" rather than a second view of the same filter.
   */
  const allBookmarks = allPlaces.filter((p) => p.kind === "bookmark");
  const suggestions = suggestionsFor(places, typed ? draft : "", props.searchUrl);
  /**
   * Which row the arrows are on — **carrying the list it was chosen from**.
   *
   * A highlighted row is a position, and the list it indexes comes from a poll: a
   * service coming up or going down mid-run reorders `places`, so a bare index would
   * have Enter open a place the user never arrowed to, ring and all, with nothing
   * looking wrong. The first attempt reset the index from an effect, which is the
   * mistake this repo has made three times: an effect runs *after* the commit, so the
   * frame between the new list and the reset is a real frame, and a keypress already
   * queued is dispatched against it.
   *
   * So the staleness is a value the data carries. `key` is the list the row was picked
   * from; a render whose list no longer matches reads the row as "none" without any
   * ordering having to be won. `-1` is also what Enter reads as "go to whatever is
   * typed" rather than "open the highlighted row".
   *
   * **The row also carries its URL, which is what survives the list changing.** With
   * files in `places`, the list now churns on its own: the app refetches every 20
   * seconds and an agent writing a file is the premise of the feature, so "the list
   * changed" went from *never happens mid-typing* to *happens while you are arrowing*.
   * Dropping the highlight was the right answer when a change meant a service came up;
   * it is the wrong answer when a change means an unrelated file was saved, because
   * Enter then silently navigates the typed text instead of the ringed row.
   *
   * So a changed list is re-checked rather than abandoned: if the URL that was
   * highlighted is still present, the highlight moves to wherever it now is. The
   * safety property is unchanged — Enter can still only ever open the row the user
   * actually arrowed to — and it now holds across a poll.
   */
  // The action row's *presence* is part of the key, not just the places. It shifts
  // every place's index by one, so a flip while a row is arrowed would leave the stale
  // index naming a different place. Typing already resets the row, which covers the
  // common case; this covers the uncommon one — `browser.searchUrl` arriving from a
  // settings sync changes whether a typed query resolves to an action at all.
  const placeKey = `${suggestions.action ? "a|" : ""}${places
    .map((p) => p.url)
    .join(" ")}`;
  const [highlight, setHighlight] = useState<{
    key: string;
    row: number;
    url: string | null;
  }>({ key: placeKey, row: -1, url: null });
  const activeRow = (() => {
    if (highlight.row < 0) return -1;
    if (highlight.key === placeKey) return highlight.row;
    // The list moved. Follow the URL if it is still offered, otherwise no row.
    if (highlight.url === null) return -1;
    return indexOfPlaceUrl(suggestions, highlight.url);
  })();
  const setActiveRow = (row: number) => {
    // The URL of the row being highlighted, so a later render can find it again.
    // Indexes here are `Suggestions`-relative, with the action row first.
    const offset = suggestions.action ? 1 : 0;
    const place = row >= offset ? suggestions.places[row - offset] : undefined;
    setHighlight({ key: placeKey, row, url: place?.url ?? null });
  };
  /**
   * The row Enter would open, which is not the same as the row the arrows have moved
   * to.
   *
   * With text typed and nothing arrowed to, the action row is what Enter does — so it
   * is what carries the ring. Leaving it unringed meant the highlight appeared only
   * *after* pressing Down, on a screen where Enter already had an effect: the ring
   * has to show what the key will do, not what the pointer has touched.
   */
  const active = activeRow < 0 && suggestions.action ? 0 : activeRow;
  // Whether the panel is up. Opens on focus *before* anything is typed — the list is
  // the answer to "what can I do here", and a panel that appears only after a
  // keystroke is a panel a first-time user never sees.
  const [suggesting, setSuggesting] = useState(false);

  const external = state.url || tab.url || "";
  const canStop = state.loading && !iframeBackend;

  const opening = covered && !failure && !nested && !chooser;
  const color = BROWSER_PROFILE_COLORS[profile];

  /**
   * Whether the suggestion **panel** is on screen, which is not the same as the
   * address bar having focus.
   *
   * On a blank pane the start page already *is* the list, so no panel renders — and
   * three things read this rather than `suggesting`: the ARIA combobox state (it must
   * not name a listbox that does not exist), Escape's first step (which otherwise
   * fired invisibly and swallowed the keypress), and the panel itself.
   *
   * The bookmarks disjunct matters for a project that declares bookmarks and has no
   * run URLs: with nothing typed those places are all collapsed, `count` is 0, and
   * gating on it alone left the address bar with no way to reach them at all.
   */
  const panelOpen =
    suggesting &&
    !chooser &&
    (suggestions.count > 0 || suggestions.bookmarks.length > 0);

  /** Every project bookmark, which is no longer inline on any surface. */
  const [bookmarksOpen, setBookmarksOpen] = useState(false);
  const [filesOpen, setFilesOpen] = useState(false);

  /**
   * The panel is an overlay over a *frozen* page, so the native view has to go.
   *
   * It used to sit in flow between the chrome and the slot — the only place a DOM
   * element is visible in the desktop app, since a `WebContentsView` paints over DOM
   * whatever the z-index says. The cost was that typing an address pushed the page
   * down by up to 60% of the pane and reflowed it on every keystroke. `setBrowserOverlay`
   * freezes and hides **this pane's view only** (not the global suspend, which would
   * stop a page somebody is watching in the other dock), leaving a still of the page
   * for the panel to dim.
   *
   * **`bookmarksOpen` is in here, and it is not decoration.** Opening the modal from the
   * panel closes the panel in the same commit, so gating on `panelOpen` alone released
   * the view — and `overlayGuard`, which is what hides views for a portalled Mantine
   * dialog, only re-takes it on the next animation frame and then spends up to
   * `FREEZE_TIMEOUT_MS` capturing. The live page popped back over the just-mounted modal
   * for that window. Holding this pane's own flag across the hand-off makes the hidden
   * period continuous instead of two overlapping ones with a gap between them.
   *
   * `profile` is a dependency because a profile change *replaces the view* — the mount
   * effect above disposes it and `ensure` builds a fresh one with `overlay: false`. That
   * effect is declared first, so it runs first, and this one then re-asserts the flag on
   * the new view rather than leaving a visible page painted over an open panel.
   */
  useEffect(() => {
    const hidden = panelOpen || bookmarksOpen;
    setBrowserOverlay(id, hidden);
    return () => setBrowserOverlay(id, false);
  }, [id, profile, panelOpen, bookmarksOpen]);

  // Anything but the default is removable, including the one this pane is on:
  // removing it moves every pane using it back to Default. Refusing instead meant
  // the session you were looking at was the one you could never get rid of.
  const removable = props.sessions.filter((p) => p !== "default");

  // A blank pane deliberately does **not** focus its own address bar. It did, on the
  // theory that a caret is what says "type here"; driving it, the pane taking the
  // keyboard on open reads as the app grabbing something rather than offering it —
  // and the pane is often opened to click a URL in it, not to type. What answers the
  // "there is no URL in here" confusion is the start page's own heading and the
  // ringed first suggestion, neither of which costs the user their keyboard.

  // A first load that never finishes used to leave the pane blank with no way
  // out but the reload button — which is exactly what the user found by accident.
  // The spinner covers the normal case; this adds the escape hatch to it rather
  // than inventing a timeout and calling a slow dev server an error.
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (!opening) {
      setSlow(false);
      return;
    }
    const timer = window.setTimeout(() => setSlow(true), 8000);
    return () => window.clearTimeout(timer);
  }, [opening, state.url]);

  // ---- Permissions --------------------------------------------------------
  //
  // Two surfaces, one policy. The **prompt** is a strip in the pane's chrome
  // rather than a native dialog or an overlay: a dialog saying "Veld" cannot
  // honestly ask on example.com's behalf, and an overlay would be painted over by
  // the native view. Sitting in the chrome, above the slot, it can name the pane
  // *and* the site, and it shrinks the slot instead of covering it — the
  // `ResizeObserver` on the slot republishes the view's box, so the page reflows
  // the way it would under any other chrome.
  //
  // The **panel** behind the shield is per site, the way a browser's site settings
  // are, and shows where each answer came from: a grant a project made in
  // `veld.json` must not read as one the user gave.
  const [site, setSite] = useState<{ origin: string | null; settings: PermissionSetting[] }>({
    origin: null,
    settings: [],
  });
  const [prompt, setPrompt] = useState<PermissionPrompt | null>(null);
  /**
   * Which session clear is awaiting confirmation — a profile, or "all".
   *
   * Clearing signs you out of every pane on that session and cannot be undone, and
   * it sat one click deep in a menu with only a `Menu.Label` as warning (#188, the
   * same missing-confirm gap as worktree removal). A menu item that destroys
   * credentials needs the second click to be a decision, not an accident.
   */
  const [confirmClear, setConfirmClear] = useState<BrowserProfile | "all" | null>(
    null,
  );

  // ---- Find in page --------------------------------------------------------
  //
  // A bar in flow between the chrome and the slot, exactly like the permission
  // prompt above and for the same reason: a native view paints over DOM
  // whatever the z-index says, so an overlay is not an option here — shrinking
  // the slot is, and its `ResizeObserver` republishes the view's box the same
  // way it does for every other row this pane inserts above it.
  //
  // Electron only. An `<iframe>` cannot read cross-origin content to search it
  // (`contentWindow` throws for the pane's whole reason to exist — a page that
  // refuses to be framed elsewhere), so the button and the `Ctrl/⌘+F` bind are
  // both gated on `canFind` and there is nothing here to disable-and-explain.
  //
  // `findSupported`, not just `!iframeBackend`: the app shell and this bundle
  // update independently, so an app older than this feature has no `find` on
  // its bridge at all. Gating on the backend alone would still render an open,
  // typeable bar that silently does nothing — worse than no bar.
  const canFind = !iframeBackend && findSupported;
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findResult, setFindResult] = useState<{
    matches: number;
    activeMatchOrdinal: number;
  } | null>(null);
  /**
   * `null` is overloaded on `findResult` for two different things — "have not
   * asked" and "asked, no answer yet" — and the count/prev/next below need to
   * tell them apart from a third: "asked, and the page really has none." This
   * flag is that third state. Without it, clearing `findResult` the instant a
   * new query starts (so a stale count from the *previous* query does not
   * linger) reads, until the reply lands, as an equally confident "No
   * results" for the *current* one — flashing that on every keystroke of a
   * page that has matches, which is worse than the stale count it replaced.
   */
  const [findPending, setFindPending] = useState(false);
  const findInputRef = useRef<HTMLInputElement>(null);

  const closeFind = () => {
    stopFind(id);
    setFindOpen(false);
    setFindQuery("");
    setFindResult(null);
    setFindPending(false);
  };

  // `Ctrl/⌘+F` while this pane's native view has keyboard focus: the shell
  // cannot forward it as a normal keystroke (a focused `WebContentsView`
  // swallows every key), so it comes back as an accelerator naming which pane
  // asked — filtered here rather than in `App.tsx`, since only the one pane the
  // key was pressed in should open its bar.
  useEffect(() => {
    if (!canFind) return;
    return onBrowserAccelerator((payload) => {
      if (payload.accelerator !== "find" || payload.viewId !== id) return;
      setFindOpen(true);
      // A repeat `Ctrl/⌘+F` after clicking back into the page is the common
      // case a real find bar handles by refocusing — but the bar is already
      // open here, so `setFindOpen(true)` is a no-op React bails out of, and
      // the mount-triggered focus effect below (keyed on `findOpen`) never
      // reruns. Focus directly for that case; a no-op when the bar isn't
      // mounted yet, which the effect below still covers on first open.
      findInputRef.current?.focus();
    });
  }, [id, canFind]);

  // The match count, from the page's own search — never derived here, since
  // only Chromium knows what its highlighter actually found. Clears the
  // pending flag: this reply is the answer the last `startFind` was waiting on
  // (main-process forwards only Chromium's `finalUpdate` reply per request, so
  // there is exactly one of these per query, not an intermediate scoping tick).
  useEffect(() => {
    return onFindResult((payload) => {
      if (payload.viewId !== id) return;
      setFindResult({ matches: payload.matches, activeMatchOrdinal: payload.activeMatchOrdinal });
      setFindPending(false);
    });
  }, [id]);

  // Live search-as-you-type, the same as a real browser's find bar. An empty
  // query clears the highlights rather than searching for nothing, which would
  // otherwise flash "No results" on every keystroke while the field is blank.
  //
  // For a non-empty query, `findResult` is cleared but `findPending` is set
  // instead of also showing "No results": the previous query's count must not
  // linger (a stale "3 of 5"), but flashing a confident zero on *every*
  // keystroke of a page that has matches — while genuinely waiting on the
  // reply — is a worse, more visible bug than the stale count it would replace.
  useEffect(() => {
    if (!findOpen) return;
    if (findQuery === "") {
      setFindResult(null);
      setFindPending(false);
      stopFind(id);
      return;
    }
    setFindResult(null);
    setFindPending(true);
    startFind(id, findQuery);
  }, [id, findOpen, findQuery]);

  useEffect(() => {
    if (findOpen) findInputRef.current?.focus();
  }, [findOpen]);

  // A stale match count surviving a navigation is a wrong answer, not a stale
  // render — the query no longer describes the page underneath it, so closing
  // the bar (rather than leaving "3 of 5" up for a page with none of them) is
  // the only honest move. `findOpenRef` (not `findOpen` in the dependency
  // array) is deliberate: opening the bar must not immediately close it.
  //
  // Keyed on `state.loading`'s rising edge, not on `state.url` directly — the
  // permission prompt below draws the same lesson from the same trap
  // (`promptOrigin`: "a fragment change cannot cross an origin"), just with a
  // different fix for a different question. `state.url` is pushed on
  // `did-navigate-in-page` too, which fires for a single-page app's own
  // `history.pushState`/hash change with no real navigation underneath it, so
  // closing the bar on every one of those would be firing on the wrong signal.
  // Origin (that fix) is the wrong substitute here, though: a real same-origin
  // navigation — an ordinary link to another page on the same site — must
  // still close the bar, and origin-keying would miss exactly that. What
  // actually distinguishes the two is whether a network load happened at all:
  // `did-start-loading` — the one thing that flips `loading` to `true` — never
  // fires for an in-page navigation, since there is nothing to load.
  const findOpenRef = useRef(false);
  findOpenRef.current = findOpen;
  const wasLoadingRef = useRef(state.loading);
  useEffect(() => {
    const startedLoading = state.loading && !wasLoadingRef.current;
    wasLoadingRef.current = state.loading;
    if (!startedLoading || !findOpenRef.current) return;
    stopFind(id);
    setFindOpen(false);
    setFindQuery("");
    setFindResult(null);
  }, [id, state.loading]);

  // Read by the unmount cleanup, which must not re-subscribe on every prompt.
  const promptRef = useRef<PermissionPrompt | null>(null);
  promptRef.current = prompt;
  useEffect(() => {
    const offSettings = onPermissionSettings((p) => {
      if (p.viewId !== id) return;
      setSite({ origin: p.origin, settings: p.settings });
    });
    const offPrompt = onPermissionRequest((p) => {
      if (p.viewId !== id) return;
      // One prompt at a time per pane — a page asking for the camera and the
      // microphone in the same tick would otherwise stack two strips, each
      // pushing the page down again. The one that loses is **released**, not
      // dropped: without that its request stayed blocked on a callback nothing
      // could ever fire, so the page hung with no error and no way back.
      setPrompt((current) => {
        if (current) {
          abandonPermission(p.requestId);
          return current;
        }
        return p;
      });
    });
    requestPermissionSettings(id);
    return () => {
      offSettings();
      offPrompt();
    };
  }, [id]);
  // A navigation to **another origin** invalidates a prompt raised by the page
  // that is leaving: answering it would attribute a grant to the site now in the
  // address bar. The request is released rather than merely hidden, since an
  // in-page navigation does not tear the frame down and its callback is still
  // waiting.
  //
  // Keyed on the *origin*, not the URL. `did-navigate-in-page` pushes state too,
  // so a single-page app calling `history.replaceState` on its first route — the
  // ordinary shape of the dev servers panes exist to show — was cancelling a
  // prompt its own origin had raised a moment earlier: the strip flashed and the
  // page got a refusal it could not tell from a Block. The shell already draws
  // this line correctly for the per-site panel ("a fragment change cannot cross
  // an origin"); this is the same rule on the other half.
  const promptOrigin = originOf(state.url);
  useEffect(() => {
    setPrompt((current) => {
      if (current) abandonPermission(current.requestId);
      return null;
    });
  }, [promptOrigin]);

  // A prompt cannot outlive the chrome that shows it: only the *active* tab
  // renders a `BrowserPane`, so switching tabs with a prompt up would otherwise
  // leave the page blocked on a callback with nothing left to answer it.
  useEffect(
    () => () => {
      if (promptRef.current) abandonPermission(promptRef.current.requestId);
    },
    [],
  );

  const answer = (verdict: "allow" | "deny") => {
    if (!prompt) return;
    answerPermission(prompt.requestId, verdict);
    setPrompt(null);
  };

  /** Navigate, and record where the pane ended up. */
  const go = (raw: string, opts: { force?: boolean; title?: string } = {}) => {
    const target = navigateBrowser(id, raw, opts);
    if (target) {
      setDraft(target);
      // A destination the user chose retires this pane's watch override: it answers
      // "not this file, not right now", and the answer does not carry to a file they
      // then went and picked. A *link* followed inside the page does not come through
      // here, which is exactly the difference that matters.
      setWatchOverride(id, null);
      // The title only when the caller has one — a picked place knows its service
      // name, which beats the hostname the page will report.
      onTab({
        url: target,
        ...(opts.title ? { title: opts.title } : {}),
      });
    }
    // Whatever happened, the panel's work is done: on success the page is loading,
    // and on a refusal the error screen is what has to be readable.
    closeSuggestions();
  };

  const closeSuggestions = () => {
    setSuggesting(false);
    setActiveRow(-1);
    setTyped(false);
  };

  /**
   * Go where the address bar says, which is two questions.
   *
   * A highlighted row wins over the text, because arrowing to a row and pressing
   * Enter is picking that row — even though the text under the cursor still says
   * something else. With no row highlighted, the text is resolved: an address is
   * navigated to and anything else is searched for (`resolveAddress`), which is what
   * makes a blank pane usable for reading documentation and not only for previewing
   * a dev server.
   */
  const submit = () => {
    const picked = pickSuggestion(suggestions, active);
    if (picked) {
      go(picked.url, {
        title: picked.path ? fileLabel(picked.path) : picked.title,
      });
      return;
    }
    const resolved = resolveAddress(draft, props.searchUrl);
    // `invalid` goes through `go` with the raw text on purpose: `navigateBrowser`
    // owns the "not an http(s) address" error, and one refusal path means the pane
    // cannot end up silently doing nothing.
    go(resolved.kind === "invalid" ? draft : resolved.url);
  };

  // ---- Watching a local file ---------------------------------------------

  /**
   * The worktree-relative path this pane is showing, or `null` for anything that
   * is not a local file.
   *
   * Read out of the URL rather than remembered from how the pane was opened
   * (`filePathIn`), which is what makes a *linked* file watchable: click through
   * from one deck to the next and this follows, with nothing written on navigation
   * and nothing to clear. `tab.url` and not `state.url` because that is the field
   * the persistence effect above keeps — and the one a restored pane has before
   * its view has reported anything.
   */
  const file = filePathIn(props.filesRoot, tab.url);
  /**
   * This pane's own answer, overriding the setting. `null` defers to the setting.
   *
   * Lives in `browserHost` beside the view it belongs to, not in state — see
   * `watchOverride` there for why a remount must not drop it.
   *
   * **Retired by a deliberate open, not by arriving at a different file.** It used to
   * carry the path it was chosen for, so any file but that one read as no override —
   * correct while picking a file was the only way to change one, and wrong the moment
   * the watch started following links: turning watching off and clicking through to
   * the next deck re-armed it at the default and reloaded the deck under the
   * presenter. Retiring it in {@link go} instead keeps the case that reasoning was
   * written for (pick deck B in this pane and it is watched again, setting willing)
   * without breaking the one the override exists for. That reset is written in the
   * same event as the navigation, never in an effect — an effect that resets state
   * runs a frame after the render that used the stale value.
   */
  const override = watchOverride(id);
  const watching = file !== null && (override ?? props.watchFilesByDefault);

  /**
   * When the last poll went out, so a page cannot set the poll rate.
   *
   * The path comes out of the URL now, and a page served from the file origin is
   * agent-authored HTML — the prompt-injectable content this whole feature's
   * threat model is about (`veld-daemon/src/files.rs`). It is same-origin with
   * itself, so `history.pushState` lets it rename its own URL as fast as it likes,
   * and every *new* path restarts the effect below with an immediate poll. The
   * requests stay confined (the daemon resolves and re-checks every path), but the
   * rate would be the page's to choose. This makes the leading-edge poll wait out
   * the remainder of the interval instead, which costs nothing when a person opens
   * a file: the first poll only establishes the baseline.
   *
   * Per mount, not per pane — a tab switch re-grants an immediate poll. That is a
   * person clicking, which no page can make happen, so the bound still holds against
   * the thing it is for.
   */
  const lastPoll = useRef(0);

  useEffect(() => {
    if (!watching || !file) return;
    let cancelled = false;
    const check = async () => {
      lastPoll.current = Date.now();
      try {
        const stat = await api.fileStat(props.worktreeId, file);
        if (cancelled) return;
        // The baseline lives in `browserHost`, beside the view it describes, **not** in
        // this effect. This component unmounts whenever its tab is not the active one
        // in its dock while the view keeps its page, so an effect-local baseline was
        // re-seeded on every remount — and a file rewritten while the tab sat in the
        // background was never noticed. Absent means "no baseline yet", which is how
        // arriving on a file avoids reloading it immediately.
        const shown = shownFileMtime(id, file);
        if (shown !== null && stat.mtimeMs !== shown) reloadBrowser(id);
        noteShownFileMtime(id, file, stat.mtimeMs);
      } catch {
        // A file mid-write, deleted, or a daemon restarting. Silent on purpose:
        // this runs on a timer, and a toast per second is worse than a pane that
        // reloads a moment late.
      }
    };
    // One schedule, not two: the interval starts *after* the leading poll, so the
    // gap between any two polls is the interval. Starting it beside the timeout
    // anchored it at effect-setup time instead, and the two could then fire a
    // millisecond apart — twice the rate the comment above claims to bound.
    const wait = Math.max(0, FILE_WATCH_INTERVAL_MS - (Date.now() - lastPoll.current));
    let timer = 0;
    const first = window.setTimeout(() => {
      void check();
      timer = window.setInterval(check, FILE_WATCH_INTERVAL_MS);
    }, wait);
    return () => {
      cancelled = true;
      window.clearTimeout(first);
      window.clearInterval(timer);
    };
  }, [watching, file, props.worktreeId, id]);

  // ---- Device emulation and zoom -----------------------------------------
  //
  // Every change writes both sides: `browserHost` applies it to the view that
  // exists now, and the tab is what a *recreated* view (a session switch, a
  // retried create) comes back as.
  //
  // What the pane asked for versus what it got: `emulationScale` is how far a
  // fitted viewport had to shrink, and `touchActive` is false while DevTools
  // holds the CDP session touch needs.
  const fitted = emulation?.fit === true && state.emulationScale < 0.995;
  // `state.loaded` gates it: a pane with no page yet has nothing emulated *at all*
  // — the shell cannot touch a view that has never navigated — so reporting that
  // as "paused" would explain a state the user is not in.
  const touchSuspended =
    !iframeBackend &&
    emulation?.touch === true &&
    !state.touchActive &&
    state.loaded;

  // The media overrides ride the same debugger session as touch, so they get the
  // same treatment: what was asked for lives in the tab (declared above, beside
  // the emulation), and whether it is *in force* comes back from the shell. Not
  // gated on `emulation` — asking what a page looks like in dark mode has nothing
  // to do with emulating a phone.
  const mediaSuspended =
    !iframeBackend && media !== null && !state.mediaActive && state.loaded;

  const applyEmulation = (next: PaneEmulation | null) => {
    setBrowserEmulation(id, next);
    // `undefined`, not `null`: "no device" is the absence of the field, so a tab
    // that never emulated anything and one switched back to pane size serialise
    // the same way.
    onTab({ emulation: next ?? undefined });
  };

  const applyMedia = (next: PaneMedia | null) => {
    setBrowserMedia(id, next);
    onTab({ media: next ?? undefined });
  };

  const applyZoom = (factor: number) => {
    const next = clampZoom(factor);
    setBrowserZoom(id, next);
    onTab({ zoom: next === DEFAULT_ZOOM ? undefined : next });
  };

  // ---- Dragging the screen's edges ---------------------------------------
  //
  // The size a fixed list can never contain: you drag until the layout breaks and
  // read the number off the chrome. Any device can be dragged — a phone dragged
  // narrower keeps its touch events and its user agent and becomes a custom size —
  // while the responsive viewport stays itself.
  //
  // The page reflows as you drag, which needs the pointer from **two** sources: this
  // document's own `pointermove`, and the ones the shell forwards from the pane's
  // page (`onBrowserPointer`). A `WebContentsView` owns every mouse event inside its
  // rect, so without the second source a cursor that crossed the page would take the
  // rest of the gesture with it — no moves, and no `pointerup` to end on.
  const [drag, setDrag] = useState<{ width: number; height: number } | null>(
    null,
  );
  const startResize = (event: React.PointerEvent, axis: "x" | "y" | "both") => {
    if (!emulation) return;
    event.preventDefault();
    event.stopPropagation();
    const originX = event.clientX;
    const originY = event.clientY;
    const from = { width: emulation.width, height: emulation.height };
    let latest = from;
    const pointerId = event.pointerId;

    // Sampled once, from the pane's own box, in this tick — not read per move from the
    // published geometry. Two reasons, both learned the hard way:
    //
    // - the published box is coalesced to one animation frame, and a mouse reports
    //   faster than the display, so a per-move read describes the *previous* painted
    //   size. The pinned answer then flipped between moves and the emulated size
    //   stopped being monotonic in pointer travel — worse than no correction at all.
    // - a gain that changes mid-gesture is applied to the *whole* travel (`from` plus
    //   the total delta), so every flip jumps the size. Sampling once makes the
    //   mapping linear for the gesture, which is what a drag should be.
    //
    // The cost is that dragging from pinned into unpinned keeps the slower gain. That
    // is predictable, and it is the direction that errs quietly.
    const paneBox = slot.current?.getBoundingClientRect();
    const startBox = paneBox ? { width: paneBox.width, height: paneBox.height } : null;
    const scale = startBox ? deviceLayout(emulation, startBox).scale : state.emulationScale;
    const pinned = startBox
      ? edgePinned(emulation, startBox)
      : { width: false, height: false };

    setDrag(from);
    setBrowserResizing(id, true);

    // One core for both pointer sources, so a cursor crossing onto the page cannot
    // change how the drag behaves — only where its events arrive from.
    const to = (clientX: number, clientY: number) => {
      latest = dragSize(from, { x: clientX - originX, y: clientY - originY }, axis, scale, pinned);
      setDrag(latest);
      // The page itself resizes and reflows, which is the point: a drag is a
      // responsive test rather than a preview of one.
      previewBrowserResize(id, latest.width, latest.height);
    };
    // Gated on the pointer that started the gesture: a second finger's press or
    // release on touch hardware is not this drag's business.
    const move = (e: PointerEvent) => {
      if (e.pointerId === pointerId) to(e.clientX, e.clientY);
    };
    const release = (e: PointerEvent) => {
      if (e.pointerId === pointerId) finish();
    };
    // Any view's forwarded pointer, not just this pane's: a sideways drag ends over
    // the *neighbouring* pane as often as not, and that view owns its own mouse-up.
    // The coordinates are window-relative and taken from the cursor, so whichever view
    // reports them they mean the same thing. Only one pointer exists, so only one drag
    // can be live in this document — there is nothing to disambiguate.
    const forwarded = onBrowserPointer((e) => {
      if (e.type === "mouseUp") finish();
      else to(e.x, e.y);
    });
    const finish = () => {
      window.clearTimeout(armBackstop);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
      window.removeEventListener("pointerdown", finish);
      forwarded();
      setDrag(null);
      // Apply *before* leaving resize mode, not after: leaving it redraws the screen
      // from the applied emulation, so the other order repaints the pre-drag size for
      // a frame and reads as the drag snapping back before it takes.
      //
      // Applied on release rather than on every move, though: each apply is an
      // `enableDeviceEmulation`, which relayouts the guest page, and a layout write
      // per pointer move would also fill the undo-less layout history with noise.
      if (latest.width !== from.width || latest.height !== from.height) {
        applyEmulation(resizeEmulation(emulation, latest.width, latest.height));
      }
      setBrowserResizing(id, false);
    };
    // On `window`, not on the handle: the handle is a React element whose position
    // is a function of the size being dragged, so it re-renders — and moves — under
    // the pointer on every event. Listeners on it (and the pointer capture that
    // went with them) died with the first re-render, which is exactly the shape of
    // "the outline appears and then nothing moves". The window survives the render,
    // and the native view is hidden for the duration, so nothing else can take the
    // events.
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    // Backstop: if a release is lost despite all of the above, the next press anywhere
    // ends the gesture rather than leaving it resizing a button-less cursor. Armed off
    // this tick — registering it inline made it depend on `stopPropagation` above to
    // avoid firing on the very press that starts the drag, which is a dependency
    // nothing states and one reorder away from ending every gesture as it begins.
    const armBackstop = window.setTimeout(() => {
      window.addEventListener("pointerdown", finish);
    }, 0);
  };

  /**
   * Where the screen is drawn right now.
   *
   * Straight from the host's own geometry, during a drag as well as outside one:
   * `previewBrowserResize` redraws the frame *and* republishes this box, so the
   * handles ride the screen's edges without a second calculation to disagree with.
   * Deriving it here from the dragged size was the earlier shape, and it drifted the
   * moment fitting clamped the screen to the pane.
   */
  /**
   * The pane's content box — what "the size the pane can hold" means — or `null` when
   * the slot is not laid out.
   *
   * `null` rather than a fallback, and specifically not `state.device*`: that is the
   * *screen's* drawn box, already inset and scaled, which is the 172px-for-a-phone-at-50%
   * answer this whole path exists to stop producing. A plausible wrong number here is
   * worse than no number, because the caller cannot tell it apart from a real one.
   */
  const paneSize = (): { width: number; height: number } | null => {
    const box = slot.current?.getBoundingClientRect();
    if (!box || box.width < 1 || box.height < 1) return null;
    return { width: box.width, height: box.height };
  };

  /**
   * Turn the responsive viewport on at whatever the pane can hold.
   *
   * Its own function for one rule: the size is measured from the *pane's* box, and
   * the click is genuinely skipped when there is no box to measure. `state.device*`
   * is the *screen's* drawn box, already inset and scaled, which is the
   * 172px-for-a-phone-at-50% answer `paneSize` exists to stop producing.
   *
   * **One caller now** — the device menu's Responsive item. It had two until the
   * pane bar's quick switch became a phone (`applyPreset`), and the rule is worth
   * keeping in one named place regardless: it is the whole difference between a
   * responsive viewport that starts at the pane and one that starts at whatever the
   * last device happened to be drawn at.
   */
  const enterResponsive = () => {
    const box = paneSize();
    if (!box) return;
    applyEmulation(
      responsiveEmulation(
        box.width - DEVICE_PADDING * 2,
        box.height - DEVICE_PADDING * 2,
      ),
    );
  };

  /**
   * Emulate a preset, keeping this pane's own answers to the two questions the
   * preset table cannot hold.
   *
   * One owner for the same reason `enterResponsive` is one: the orientation reset
   * and the user-agent template belong to `emulationForPreset`, but *which* Chrome
   * version to claim and *whether* to fit are the pane's, and two controls now pick
   * a preset — the device menu's list and the quick switch.
   */
  const applyPreset = (preset: DevicePreset) =>
    applyEmulation(
      emulationForPreset(preset, {
        chrome: HOST_CHROME,
        fit: emulation?.fit ?? true,
      }),
    );

  // ---- Quick switches -----------------------------------------------------
  //
  // The two things people do dozens of times an hour while working on a layout,
  // both three levels deep in the device menu. Nothing here is new capability — it
  // is reach, and which of them appears is a global preference: whether you want
  // the shortcut at all, deliberately *not* an answer to one narrow pane (see the
  // note beside the Rust defaults).
  //
  // **Each switch's off is one definite state, never menu history.** The colour scheme cycles
  // System → Dark → Light → System rather than toggling dark on and off: System is
  // the *absence* of an override (which is what lets the CDP session be released),
  // and Light is a real destination, because a light-only layout bug is as ordinary
  // as a dark one and sending someone back into the menu for it is the reach problem
  // this switch exists to fix. The device switch's off is no emulation at all: it
  // deliberately does not restore a previously picked device, so the switch reads as
  // one fact — "am I on the phone" — rather than as a history of the menu.
  //
  // `quickSwitches.responsive` (and `browser.quickSwitch.responsive` behind it) names
  // the *slot*, not what it applies — the key predates the switch becoming a phone
  // and renaming it would be a settings migration for a word.
  const quickPreset = presetById(QUICK_DEVICE);
  const quickOn = emulation?.device === QUICK_DEVICE;
  const scheme = media?.["prefers-color-scheme"];
  // Achieved, not requested: `mediaSuspended` is the shell's own report that the
  // override is not in force because Chromium's debugger is held elsewhere. A switch
  // that claims Dark over a page that is still light is the exact lie `mediaActive`
  // exists to prevent.
  const schemeSuspended = scheme !== undefined && mediaSuspended;
  const schemeLabel = schemeName(scheme);
  // Derived from the cycle rather than from a second table keyed by label, so the
  // tooltip cannot promise a destination the click does not go to.
  const nextSchemeLabel = schemeName(nextColorScheme(scheme) ?? undefined);

  const screen = {
    x: state.deviceX,
    y: state.deviceY,
    width: state.deviceWidth,
    height: state.deviceHeight,
  };

  // Empty means "keep the current one", so one field can be changed without
  // retyping the other. The placeholders show what that currently is.
  const [customW, setCustomW] = useState("");
  const [customH, setCustomH] = useState("");
  const applyCustom = () => {
    const w = Number(customW) || emulation?.width || 1280;
    const h = Number(customH) || emulation?.height || 800;
    // Keeps the device flags of whatever is set now, so nudging a phone's width
    // stays a phone — the useful reading of "custom size".
    applyEmulation(customEmulation(w, h, emulation));
  };

  return (
    <div className="browser-pane">
      {/* The session's colour on the chrome's own edge: enough to tell two panes
          of the same app apart at a glance, and it costs no layout — a strip
          above the view would move the native view's box on every switch. */}
      <div
        className="browser-bar"
        style={color ? { borderBottomColor: color } : undefined}
      >
        <Tooltip
          {...TIP}
          label={iframeBackend ? "History needs the desktop app" : "Back"}
        >
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label="Back"
            disabled={!state.canGoBack}
            onClick={() => browserCommand(id, "back")}
          >
            <IconArrowLeft size={14} />
          </ActionIcon>
        </Tooltip>
        <Tooltip
          {...TIP}
          label={iframeBackend ? "History needs the desktop app" : "Forward"}
        >
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label="Forward"
            disabled={!state.canGoForward}
            onClick={() => browserCommand(id, "forward")}
          >
            <IconArrowRight size={14} />
          </ActionIcon>
        </Tooltip>
        <Tooltip {...TIP} label={canStop ? "Stop loading" : "Reload"}>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label={canStop ? "Stop loading" : "Reload"}
            onClick={() =>
              canStop ? browserCommand(id, "stop") : reloadBrowser(id)
            }
          >
            {canStop ? <IconX size={14} /> : <IconRefresh size={14} />}
          </ActionIcon>
        </Tooltip>

        {/* Only for a pane showing a local file, and only then: on every other page
            there is nothing to watch, and a permanently disabled button would be
            four pixels of explanation on every pane in the app. */}
        {file && (
          <Tooltip
            {...TIP}
            label={
              watching
                ? "Reloading when the file changes — click to stop"
                : "Reload when the file changes"
            }
          >
            <ActionIcon
              size="sm"
              variant={watching ? "light" : "subtle"}
              color={watching ? undefined : "gray"}
              aria-label={
                watching
                  ? "Stop reloading when the file changes"
                  : "Reload when the file changes"
              }
              aria-pressed={watching}
              onClick={() =>
                file && (setWatchOverride(id, !watching), bump())
              }
            >
              <IconLivePhoto size={14} />
            </ActionIcon>
          </Tooltip>
        )}

        {/* Find in page: Electron only, for the same reason the bar itself is —
            an iframe cannot search cross-origin content it cannot read — and
            also gated on `findSupported`, so an app shell older than this
            feature (the two update independently) doesn't show a button that
            opens a bar with no way to ever report a real count. Hidden rather
            than disabled-with-tooltip, like the permission shield below: there
            is nothing to explain past "this needs a newer desktop app". */}
        {canFind && (
          <Tooltip {...TIP} label="Find in page">
            <ActionIcon
              size="sm"
              variant={findOpen ? "light" : "subtle"}
              color={findOpen ? "blue" : "gray"}
              aria-label="Find in page"
              onClick={() => (findOpen ? closeFind() : setFindOpen(true))}
            >
              <IconSearch size={14} />
            </ActionIcon>
          </Tooltip>
        )}

        {/* Site settings, where a browser puts them: at the head of the address
            bar, about the site the address bar is showing. Hidden in the browser
            build — an iframe's permissions are the embedding document's business
            and veld has nothing to answer there. */}
        {!iframeBackend && site.origin && (
          <Tooltip {...TIP} label={`Permissions for ${site.origin}`}>
            <span className="bar-tip">
              <Menu position="bottom-start" withinPortal>
                <Menu.Target>
                  <ActionIcon
                    size="sm"
                    variant="subtle"
                    color={site.settings.some((s) => s.verdict === "allow") ? "blue" : "gray"}
                    aria-label={`Permissions for ${site.origin}`}
                  >
                    <IconShieldLock size={14} />
                  </ActionIcon>
                </Menu.Target>
                <Menu.Dropdown className="permission-panel">
                  {/* Not `Menu.Label`: this has to stay pinned while the rows
                      scroll, and styling that through Mantine's own class name
                      would break silently the day the class is renamed. A
                      per-site panel whose site has scrolled out of view is a
                      list of switches for an origin you have to guess at. */}
                  <div className="permission-site">{site.origin}</div>
                  {site.settings.map((setting) => (
                    <div
                      className={
                        setting.source === "user"
                          ? "permission-row decided"
                          : "permission-row"
                      }
                      key={setting.id}
                    >
                      <span className="permission-name">
                        {PERMISSION_LABELS[setting.id]?.title ?? setting.id}
                        {/* What it currently resolves to, and *why* — the two
                            questions the buttons below cannot answer, because
                            they show your preference rather than the outcome. A
                            project's grant must never read as your own decision. */}
                        <span className="permission-effect faint">
                          {effectiveLabel(setting)}
                        </span>
                      </span>
                      <Button.Group>
                        {(["default", "allow", "deny"] as const).map((choice) => (
                          <Button
                            key={choice}
                            size="compact-xs"
                            // Colour carries the meaning, not just the selection:
                            // green for a grant, red for a block, and grey for
                            // Default — the untouched state, and the one a reader
                            // is trying to skip past. With one accent for all
                            // three, twenty rows give no clue which two were set.
                            color={
                              choice === "allow" ? "green" : choice === "deny" ? "red" : "gray"
                            }
                            // **The buttons show *your* setting, not the outcome.**
                            // They used to show the resolved verdict, which made
                            // the third one unusable: clicking "Ask" on a
                            // permission `veld.json` grants cleared the override,
                            // the row re-resolved to allow, and the button you
                            // pressed lit up as Allow. It looked like the control
                            // refused the click. Nothing was wrong but the label —
                            // the third state is "I have not decided", so it says
                            // Default and the outcome is spelled out beside the
                            // name.
                            variant={userChoice(setting) === choice ? "filled" : "default"}
                            onClick={() =>
                              setPermission(id, site.origin as string, setting.id, choice)
                            }
                          >
                            {choice === "allow" ? "Allow" : choice === "deny" ? "Block" : "Default"}
                          </Button>
                        ))}
                      </Button.Group>
                    </div>
                  ))}
                </Menu.Dropdown>
              </Menu>
            </span>
          </Tooltip>
        )}

        <input
          className="browser-address"
          value={draft}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          aria-label="Address"
          // Names the capability rather than the field. "Enter a URL" was already
          // there and a first-time user still asked "there is no URL in here, how do
          // I use it?" — a noun tells you what goes in the box, not what the box can
          // do for you. The `%s` template being unset takes the promise back out.
          placeholder={
            props.searchUrl.trim() === ""
              ? "Go to an address"
              : "Search, or go to an address"
          }
          // The list of places is what this field is *for* — see `suggesting`.
          //
          // `panelOpen`, not `suggesting`: on a blank pane the start page *is* the
          // list, so no panel with this id is rendered, and claiming an expanded
          // listbox that does not exist points a screen reader at nothing.
          role="combobox"
          aria-expanded={panelOpen}
          aria-controls={panelOpen ? suggestId(id) : undefined}
          aria-activedescendant={
            panelOpen && active >= 0 ? `${suggestId(id)}-row-${active}` : undefined
          }
          aria-autocomplete="list"
          onChange={(e) => {
            setDraft(e.currentTarget.value);
            setTyped(true);
            setSuggesting(true);
            // Typing invalidates the highlight: the row that was under it has very
            // likely been filtered out, and a stale index would open a row the user
            // can no longer see.
            setActiveRow(-1);
          }}
          // **Clicking an unfocused bar selects all of it**, which `onFocus`'s `select()`
          // alone does not achieve: the pointer sequence is mousedown → focus → mouseup,
          // and mouseup's default action collapses the selection to a caret where the
          // click landed. So the whole address was selected for one frame and then
          // wasn't, and replacing it took a second ⌘A — while arriving by Tab or by the
          // keyboard shortcut worked perfectly, which is what made it look intermittent.
          //
          // Taking the mousedown and focusing by hand skips the caret placement
          // entirely. Gated on the field not already having focus, so once you are in
          // it a click still positions the caret and a drag still selects a range —
          // the same trade every browser's address bar makes.
          onMouseDown={(e) => {
            if (document.activeElement === e.currentTarget) return;
            e.preventDefault();
            e.currentTarget.focus();
          }}
          onFocus={(e) => {
            setEditing(true);
            setSuggesting(true);
            setTyped(false);
            e.currentTarget.select();
          }}
          // **Closing on blur is what makes the panel dismissible at all.** It was
          // left out because a click on a suggestion blurs the field first, which
          // would unmount the row mid-click — but the panel is in flow above the
          // page, so without this, clicking into the page left up to 60% of the pane
          // occupied by a list of one row reading "Go to <the URL you are already
          // on>", with no way back but the address bar. The mid-click problem is
          // solved where it happens instead: the panel swallows `mousedown` so the
          // field never loses focus to a row (see `.suggest-panel` below).
          // **Blur means the user left, so everything resets** — and the reason that is
          // safe belongs here, because getting it wrong took four attempts.
          //
          // A click on a row is *not* leaving, and that is enforced where it happens:
          // both surfaces that render rows swallow `mousedown` (the panel below, and the
          // start page's list), so the field never blurs while a click is in flight and
          // this handler cannot reflow anything under the pointer.
          //
          // Two dead ends, recorded so they are not retried. Leaving blur alone entirely
          // wedged the panel in flow above the page until the user came back and clicked
          // the bar. Gating the reset on `panelOpen` fixed that and broke the blank
          // pane — and gating only `typed` did not fix the blank pane either, because the
          // reflow there is not caused by any flag: `setEditing(false)` re-runs the
          // draft-sync effect above, which on a blank pane writes `draft = ""` (there is
          // no page URL to restore), and the empty query widens the list all by itself.
          // One mechanism for both surfaces is what actually closes it.
          onBlur={() => {
            setEditing(false);
            closeSuggestions();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
              e.currentTarget.blur();
            } else if (e.key === "Escape") {
              // One Escape at a time: with the panel up it closes the panel and
              // leaves the text alone, so a mis-arrowed highlight costs nothing.
              // A second one restores the address and gives the keyboard back.
              //
              // Gated on the panel actually being *rendered*: on a blank pane
              // `suggesting` is true from the focus alone, so this branch used to
              // swallow the first Escape invisibly — widening the still-visible start
              // page back to unfiltered and not giving the keyboard back.
              if (panelOpen) {
                closeSuggestions();
                return;
              }
              setDraft(state.url || tab.url || "");
              e.currentTarget.blur();
            } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              if (suggestions.count === 0) return;
              e.preventDefault();
              setSuggesting(true);
              // Stepped from `active`, not from the raw state: with the action row
              // ringed by default, Down has to move to row 1, and an updater reading
              // `-1` would move to row 0 and look like the key did nothing.
              setActiveRow(
                stepIndex(suggestions.count, active, e.key === "ArrowDown" ? 1 : -1),
              );
            }
          }}
        />

        <Tooltip {...TIP} label={`Session: ${browserProfileLabel(profile)}`}>
          {/* On a span, not on the Menu.Target: `Popover.Target` overwrites the ref
              a Tooltip puts on its child, so a tooltip cloned around a menu target
              has nothing to anchor to. The hover area is the same. */}
          <span className="bar-tip">
            <Menu position="bottom-end" withinPortal>
              <Menu.Target>
                <ActionIcon
                  size="sm"
                  variant="subtle"
                  color="gray"
                  aria-label={`Browser session: ${browserProfileLabel(profile)}`}
                >
                  {color ? (
                    <SessionDot color={color} size={10} />
                  ) : (
                    <IconUserCircle size={14} />
                  )}
                </ActionIcon>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Label>
                  {iframeBackend
                    ? "Separate sessions need the desktop app"
                    : "Cookie jar for this pane"}
                </Menu.Label>
                {/* The sessions that exist for this worktree — an explicit set, not
                the occupied slots. Deriving it from occupancy made moving a pane
                onto a new session vacate its old one, so adding a session looked
                like deleting the previous. */}
                {props.sessions.map((p) => (
                  <Menu.Item
                    key={p}
                    disabled={iframeBackend}
                    fw={p === profile ? 700 : undefined}
                    leftSection={
                      <SessionDot color={BROWSER_PROFILE_COLORS[p]} />
                    }
                    onClick={() => onTab({ profile: p })}
                  >
                    {browserProfileLabel(p)}
                    {p === "default" ? " · default" : ""}
                  </Menu.Item>
                ))}
                <Menu.Divider />
                {/* Adding moves this pane onto the new session, because that is the
                only reason to create one — but the old session stays in the list,
                which is the whole point of the set being explicit. */}
                <Menu.Item
                  leftSection={<IconPlus size={14} />}
                  disabled={iframeBackend || !props.onAddSession}
                  onClick={props.onAddSession}
                >
                  {props.onAddSession
                    ? "Add a session for this pane"
                    : `All ${MAX_EXTRA_SESSIONS} sessions exist`}
                </Menu.Item>
                <Menu.Sub>
                  <Menu.Sub.Target>
                    <Menu.Sub.Item
                      leftSection={<IconMinus size={14} />}
                      disabled={iframeBackend || removable.length === 0}
                    >
                      {removable.length === 0
                        ? "Nothing to remove"
                        : "Remove a session"}
                    </Menu.Sub.Item>
                  </Menu.Sub.Target>
                  <Menu.Sub.Dropdown>
                    <Menu.Label>
                      Frees the slot and returns its panes to Default; data is
                      kept
                    </Menu.Label>
                    {removable.map((p) => (
                      <Menu.Item
                        key={p}
                        leftSection={
                          <SessionDot color={BROWSER_PROFILE_COLORS[p]} />
                        }
                        onClick={() => props.onRemoveSession(p)}
                      >
                        {browserProfileLabel(p)}
                      </Menu.Item>
                    ))}
                  </Menu.Sub.Dropdown>
                </Menu.Sub>
                <Menu.Sub>
                  <Menu.Sub.Target>
                    <Menu.Sub.Item
                      leftSection={<IconTrash size={14} />}
                      disabled={iframeBackend}
                    >
                      Clear session data
                    </Menu.Sub.Item>
                  </Menu.Sub.Target>
                  <Menu.Sub.Dropdown>
                    <Menu.Label>Signs out every pane using it</Menu.Label>
                    {props.sessions.map((p) => (
                      <Menu.Item
                        key={p}
                        leftSection={
                          <SessionDot color={BROWSER_PROFILE_COLORS[p]} />
                        }
                        onClick={() => setConfirmClear(p)}
                      >
                        {browserProfileLabel(p)}
                        {p === profile ? " · this pane" : ""}
                      </Menu.Item>
                    ))}
                    <Menu.Divider />
                    {/* The reachable way to clear a session nothing is using any
                    more: its slot is not listed above, but its cookies are still
                    on disk. */}
                    <Menu.Item
                      onClick={() => setConfirmClear("all")}
                    >
                      All sessions, including retired ones
                    </Menu.Item>
                  </Menu.Sub.Dropdown>
                </Menu.Sub>
              </Menu.Dropdown>
            </Menu>
          </span>
        </Tooltip>

        {/* The device quick switch, immediately before the control it is a shortcut
            into, so the size question stays in one place on the bar. Real in both
            backends: an iframe really is that many CSS pixels wide, so a phone-sized
            one is a phone-sized viewport — what it cannot claim there is touch and
            the device pixel ratio, which the device menu states.

            The colour-scheme switch sits on the *far* side of the device button
            rather than beside this one, because it is not a size question: two
            switches in a row read as one group, and the order the bar is read in is
            session → what size → what the page looks like at that size. */}
        {props.quickSwitches.responsive && quickPreset && (
          <Tooltip
            {...TIP}
            label={
              quickOn
                ? "Stop emulating — the page is the pane again"
                : emulation
                  ? // A dragged preset or a hand-entered size is `custom`, not the
                    // preset it came from (`resizeEmulation`), so the switch reads off
                    // over a viewport that is already phone-shaped — and the click
                    // *replaces* that size, which the layout cannot undo. An
                    // off-looking toggle reads as costless, so the cost is named here
                    // rather than discovered.
                    `Replace ${emulationLabel(emulation)} (${emulationSize(emulation)}) with ${quickPreset.label} (${quickPreset.width} × ${quickPreset.height})`
                  : iframeBackend
                    ? // The size is real in an iframe and the rest is not, which is the
                      // split this block's own comment states — so the copy has to make
                      // it too, the way every other control on this bar does. Claiming
                      // touch here would be the one unhedged promise on the bar.
                      `${quickPreset.label}: ${quickPreset.width} × ${quickPreset.height} — the size is real here; touch and the mobile user agent need the desktop app`
                    : `${quickPreset.label}: ${quickPreset.width} × ${quickPreset.height}, touch and a mobile user agent — drag its edges from there`
            }
          >
            <ActionIcon
              size="sm"
              variant={quickOn ? "light" : "subtle"}
              color={quickOn ? "blue" : "gray"}
              // The reason and the cost go in the *name*, not only the tooltip:
              // Mantine's Tooltip wires no `aria-describedby`, so anything stated
              // only there reaches sighted users only — and what this click discards
              // is exactly what someone who cannot see the chip needs told.
              aria-label={
                quickOn
                  ? `Turn off ${quickPreset.label}`
                  : emulation
                    ? `Replace ${emulationLabel(emulation)}, ${emulationSize(emulation)}, with ${quickPreset.label}, ${quickPreset.width} × ${quickPreset.height}`
                    : `Emulate ${quickPreset.label}, ${quickPreset.width} × ${quickPreset.height}`
              }
              aria-pressed={quickOn}
              onClick={() => (quickOn ? applyEmulation(null) : applyPreset(quickPreset))}
            >
              <IconDeviceMobile size={14} />
            </ActionIcon>
          </Tooltip>
        )}
        {/* Device emulation and zoom. One menu, because they are one question —
            "what size is this page being shown at" — and because a pane is a
            narrow strip: the chrome is already six controls wide before this, plus
            the device quick switch when that is enabled. The
            target carries the answer as text when there is one to carry, so the
            emulated size is readable without opening anything, and nothing is
            added to the bar while the pane is just a pane. */}
        <Tooltip
          {...TIP}
          label={
            emulation
              ? `${emulationLabel(emulation)} · ${emulationSize(emulation)}${
                  fitted ? ` at ${formatPercent(state.emulationScale)}` : ""
                } — drag the screen's edges to resize`
              : "Emulate a device, or zoom the page"
          }
        >
          <span className="bar-tip">
            <Menu position="bottom-end" withinPortal>
              <Menu.Target>
                {/* A Mantine `Button` rather than a bare one, purely so its ink is
                    Mantine's: the `ActionIcon`s either side of it take their colour
                    from a variant resolver that runs in JS (`--ai-color`), so no
                    token this stylesheet could name is guaranteed to match, and
                    guessing one is what made an available control look disabled.
                    `subtle` when nothing is set, so it is the same button as its
                    neighbours; `default` once a device or a zoom is, where the
                    border is the "something is set here" marker. */}
                <Button
                  className="browser-device"
                  variant={emulation || zoom !== DEFAULT_ZOOM ? "default" : "subtle"}
                  color="gray"
                  size="compact-xs"
                  px={5}
                  aria-label={`Device and zoom: ${
                    emulation ? emulationLabel(emulation) : "pane size"
                  }, zoom ${formatZoom(zoom)}`}
                >
                  {/* The same glyph whether or not a device is set. It used to
                      switch to `IconDeviceMobile` while emulating, which is now the
                      quick switch's own icon one button to the left — two identical
                      phones side by side read as a mistake, and the chip and the
                      `default` border already say a device is on. */}
                  <IconDevices size={14} />
                  {emulation && (
                    // While dragging this is the size under the pointer, which is why the
                    // drag needs no readout of its own — and it keeps counting past the
                    // point where fitting clamps the screen to the pane, which is the one
                    // moment the number and the box disagree.
                    <span
                      className="browser-chip"
                      data-live={drag ? "true" : undefined}
                    >
                      {drag
                        ? `${drag.width} × ${drag.height}`
                        : emulationSize(emulation)}
                      {!drag && fitted
                        ? ` · ${formatPercent(state.emulationScale)}`
                        : ""}
                    </span>
                  )}
                  {/* Not under the iframe backend: there is no zoom to apply there, so
                  a percentage in the chrome would be a claim about the page that
                  isn't true. The value is kept in the layout regardless, so opening
                  the same worktree in Veld Desktop gets it back. */}
                  {!iframeBackend && zoom !== DEFAULT_ZOOM && (
                    <span className="browser-chip">{formatZoom(zoom)}</span>
                  )}
                </Button>
              </Menu.Target>
              {/* Two columns, because this menu answers two questions — *which* device,
              and *how* it is shown — and one list of both was taller than the
              window it opened in. The device list scrolls on its own so growing the
              preset table can never push the zoom controls off screen again, and
              the dropdown is capped to the viewport as a second guard for a short
              window. */}
              <Menu.Dropdown className="device-menu">
                <div className="device-menu-cols">
                  <div className="device-menu-col devices">
                    <Menu.Label>Device</Menu.Label>
                    <Menu.Item
                      fw={emulation ? undefined : 700}
                      leftSection={
                        emulation ? undefined : <IconCheck size={14} />
                      }
                      onClick={() => applyEmulation(null)}
                    >
                      Pane size
                    </Menu.Item>
                    {/* The size no list can contain. Starts at what the pane can hold,
                    so turning it on changes nothing except that the screen now has
                    edges you can drag and a number on it. */}
                    <Menu.Item
                      fw={
                        emulation?.device === RESPONSIVE_DEVICE
                          ? 700
                          : undefined
                      }
                      leftSection={
                        emulation?.device === RESPONSIVE_DEVICE ? (
                          <IconCheck size={14} />
                        ) : undefined
                      }
                      // See `enterResponsive` for why the size is measured from the
                      // pane's own box, and why the click is skipped when there is
                      // no box to measure.
                      onClick={enterResponsive}
                      rightSection={
                        <span className="menu-size faint">drag to resize</span>
                      }
                    >
                      Responsive
                    </Menu.Item>
                    {DEVICE_GROUPS.map((group) => (
                      <div key={group}>
                        <Menu.Label>{group}</Menu.Label>
                        {DEVICE_PRESETS.filter((p) => p.group === group).map(
                          (preset) => (
                            <Menu.Item
                              key={preset.id}
                              fw={
                                emulation?.device === preset.id
                                  ? 700
                                  : undefined
                              }
                              leftSection={
                                emulation?.device === preset.id ? (
                                  <IconCheck size={14} />
                                ) : undefined
                              }
                              // A preset arrives the way that device is held, so
                              // rotation resets: picking one is choosing a device,
                              // not adjusting the current one. It used to carry the
                              // orientation over, on the theory that you were
                              // comparing two phones sideways — but then picking
                              // "Small phone" could hand you a 780×360 strip with
                              // nothing on screen saying why. `fit` does carry over,
                              // because that is a preference about the *pane* rather
                              // than a property of the device.
                              onClick={() => applyPreset(preset)}
                              rightSection={
                                <span className="menu-size faint">
                                  {preset.width} × {preset.height}
                                </span>
                              }
                            >
                              {preset.label}
                            </Menu.Item>
                          ),
                        )}
                      </div>
                    ))}
                  </div>

                  <div className="device-menu-col">
                    {/* What is set right now, at the top of the column that changes it:
                    the size alone does not say which device it came from, and a
                    rotated preset is the same two numbers as a smaller one. */}
                    <Menu.Label>
                      {emulation
                        ? `${emulationLabel(emulation)} · ${emulationSize(emulation)}`
                        : "No device — the page is the pane"}
                    </Menu.Label>
                    {/* Everything here acts on the current device, so it is all inert
                    without one — disabled rather than hidden, because a menu whose
                    length changes is a menu you have to re-read. */}
                    <Menu.Item
                      leftSection={<IconRotateClockwise size={14} />}
                      disabled={!emulation}
                      onClick={() =>
                        emulation && applyEmulation(rotateEmulation(emulation))
                      }
                      rightSection={
                        <span className="menu-size faint">
                          {emulation ? orientationLabel(emulation) : ""}
                        </span>
                      }
                    >
                      Rotate
                    </Menu.Item>
                    <Menu.Item
                      closeMenuOnClick={false}
                      leftSection={
                        emulation?.fit ? <IconCheck size={14} /> : undefined
                      }
                      disabled={!emulation}
                      onClick={() =>
                        emulation &&
                        applyEmulation({ ...emulation, fit: !emulation.fit })
                      }
                      rightSection={
                        <span className="menu-size faint">
                          {fitted ? formatPercent(state.emulationScale) : ""}
                        </span>
                      }
                    >
                      Fit to pane
                    </Menu.Item>
                    <Menu.Item
                      closeMenuOnClick={false}
                      leftSection={
                        emulation?.touch ? <IconCheck size={14} /> : undefined
                      }
                      disabled={!emulation || iframeBackend}
                      onClick={() =>
                        emulation &&
                        applyEmulation({
                          ...emulation,
                          touch: !emulation.touch,
                        })
                      }
                    >
                      Touch events
                    </Menu.Item>
                    {/* Separate from the size, because "does my app serve the mobile
                    bundle at this width" and "does my layout survive this width" are
                    different questions — and a responsive or custom size has no
                    preset to inherit a user agent from at all. Reloads the pane: a
                    document reads `navigator.userAgent` once, while it loads. */}
                    <Menu.Item
                      closeMenuOnClick={false}
                      leftSection={
                        emulation?.ua ? <IconCheck size={14} /> : undefined
                      }
                      disabled={!emulation || iframeBackend}
                      onClick={() =>
                        emulation &&
                        applyEmulation(
                          withMobileUserAgent(
                            emulation,
                            !emulation.ua,
                            HOST_CHROME,
                          ),
                        )
                      }
                    >
                      Mobile user agent
                    </Menu.Item>
                    {/* Stated rather than implied: `setUserAgent` sets the *string*
                    only and Electron exposes no metadata argument, so
                    `navigator.userAgentData` and the `Sec-CH-UA*` request headers keep
                    reporting this desktop. A stack that branches on client hints
                    instead of the UA string therefore still serves its desktop bundle.
                    Doing it properly means `Emulation.setUserAgentOverride` with
                    `userAgentMetadata` over CDP, which would put the user agent behind
                    a debugger attach that DevTools can take away — a trade worth its
                    own increment rather than a quiet half-fix. */}
                    {emulation?.ua && !iframeBackend && (
                      <Menu.Label>
                        UA string only — client hints still report desktop
                      </Menu.Label>
                    )}
                    {/* Touch needs Chromium's debugger session, which something else
                    can hold — DevTools does on some Electron versions, though not
                    this one. Reported from what the shell actually achieved rather
                    than from a guess about the cause. */}
                    {touchSuspended && (
                      <Menu.Label>
                        Touch is paused — Chromium's debugger is in use
                        elsewhere
                      </Menu.Label>
                    )}

                    <Menu.Divider />
                    {/* About the *page*, not about Veld: the app themes itself
                        light/dark too, and a control that reads as "dark mode"
                        beside the device picker would be taken for that one. So
                        the label says whose preference is being emulated. */}
                    <Menu.Label>
                      {iframeBackend
                        ? "Media features need the desktop app"
                        : "The page's media features"}
                    </Menu.Label>
                    {(Object.keys(MEDIA_FEATURES) as MediaFeature[]).map((feature) => (
                      <Menu.Sub key={feature}>
                        <Menu.Sub.Target>
                          <Menu.Sub.Item
                            disabled={iframeBackend}
                            leftSection={media?.[feature] ? <IconCheck size={14} /> : undefined}
                            rightSection={
                              <span className="menu-size faint">
                                {media?.[feature]
                                  ? MEDIA_LABELS[feature].values[media[feature]]
                                  : "System"}
                              </span>
                            }
                          >
                            {MEDIA_LABELS[feature].title}
                          </Menu.Sub.Item>
                        </Menu.Sub.Target>
                        <Menu.Sub.Dropdown>
                          {/* "System" is the absence of an override, not a third
                              value — it is what makes turning one off possible at
                              all, and it is what lets the debugger be released
                              when no feature is overridden any more. */}
                          <Menu.Item
                            leftSection={!media?.[feature] ? <IconCheck size={14} /> : undefined}
                            onClick={() => applyMedia(withMediaFeature(media, feature, null))}
                          >
                            System
                          </Menu.Item>
                          {MEDIA_FEATURES[feature].map((value) => (
                            <Menu.Item
                              key={value}
                              leftSection={
                                media?.[feature] === value ? <IconCheck size={14} /> : undefined
                              }
                              onClick={() => applyMedia(withMediaFeature(media, feature, value))}
                            >
                              {MEDIA_LABELS[feature].values[value]}
                            </Menu.Item>
                          ))}
                        </Menu.Sub.Dropdown>
                      </Menu.Sub>
                    ))}
                    {/* Same debugger session as touch, so the same honesty: report
                        what the shell achieved, not what was asked for. */}
                    {mediaSuspended && (
                      <Menu.Label>
                        Media features are paused — Chromium's debugger is in use elsewhere
                      </Menu.Label>
                    )}

                    <Menu.Divider />
                    <Menu.Label>Custom size</Menu.Label>
                    {/* Not a Menu.Item: these are fields, and a click in one must not
                    close the menu it lives in. */}
                    <div className="menu-fields">
                      <input
                        type="number"
                        aria-label="Custom width"
                        min={MIN_DEVICE_PX}
                        max={MAX_DEVICE_PX}
                        placeholder={String(emulation?.width ?? 1280)}
                        value={customW}
                        onChange={(e) => setCustomW(e.currentTarget.value)}
                        onKeyDown={(e) => e.key === "Enter" && applyCustom()}
                      />
                      <span className="faint">×</span>
                      <input
                        type="number"
                        aria-label="Custom height"
                        min={MIN_DEVICE_PX}
                        max={MAX_DEVICE_PX}
                        placeholder={String(emulation?.height ?? 800)}
                        value={customH}
                        onChange={(e) => setCustomH(e.currentTarget.value)}
                        onKeyDown={(e) => e.key === "Enter" && applyCustom()}
                      />
                      <button className="btn" onClick={applyCustom}>
                        Apply
                      </button>
                    </div>

                    <Menu.Divider />
                    <Menu.Label>
                      {iframeBackend
                        ? "Page zoom needs the desktop app"
                        : "Page zoom"}
                    </Menu.Label>
                    {/* A 1440-wide layout is readable in a 600px pane at 60%, which is
                    useful well before any device preset is — and it is the same
                    "state lives in the layout, re-asserted when the view is
                    recreated" problem, so it belongs in the same menu. */}
                    <div className="menu-fields">
                      <ActionIcon
                        size="sm"
                        variant="subtle"
                        color="gray"
                        aria-label="Zoom out"
                        disabled={iframeBackend}
                        onClick={() => applyZoom(zoomStep(zoom, -1))}
                      >
                        <IconMinus size={14} />
                      </ActionIcon>
                      <span className="menu-value">{formatZoom(zoom)}</span>
                      <ActionIcon
                        size="sm"
                        variant="subtle"
                        color="gray"
                        aria-label="Zoom in"
                        disabled={iframeBackend}
                        onClick={() => applyZoom(zoomStep(zoom, 1))}
                      >
                        <IconPlus size={14} />
                      </ActionIcon>
                      <button
                        className="btn"
                        disabled={iframeBackend || zoom === DEFAULT_ZOOM}
                        onClick={() => applyZoom(DEFAULT_ZOOM)}
                      >
                        Reset
                      </button>
                    </div>

                    <Menu.Divider />
                    {/* One way out of every setting at once. Each control undoes itself,
                    but after a session of dragging, rotating and zooming, "put it
                    back" is a single intention and should be a single click —
                    otherwise it is four, and you have to remember which four. */}
                    <Menu.Item
                      leftSection={<IconRestore size={14} />}
                      disabled={!emulation && zoom === DEFAULT_ZOOM}
                      onClick={() => {
                        applyEmulation(null);
                        applyZoom(DEFAULT_ZOOM);
                      }}
                    >
                      Reset to pane size, 100%
                    </Menu.Item>

                    {iframeBackend && (
                      <Menu.Label>
                        Sizes work in a browser tab; user agent, touch and zoom
                        need the desktop app
                      </Menu.Label>
                    )}
                  </div>
                </div>
              </Menu.Dropdown>
            </Menu>
          </span>
        </Tooltip>

        {/* **`data-disabled`, not `disabled`.** A real `<button disabled>` is styled
            the same but dispatches no pointer events, so its Tooltip never opens —
            Mantine puts the hover handlers on the child element itself, and adds no
            `pointer-events: none` of its own. The explanation would be unreachable in
            the one backend that needs it, which is the "control that silently does
            nothing" this was meant to avoid. `data-disabled` is Mantine's own answer:
            it drives the disabled *styling* through `mod` and leaves the element
            hoverable, so the click has to be refused in the handler instead, and
            `aria-disabled` carries what the missing attribute used to. The device
            menu never hit this because it states its gaps in a `Menu.Label`, which
            renders regardless of any item's state. */}
        {props.quickSwitches.colorScheme && (
          <Tooltip
            {...TIP}
            label={
              iframeBackend
                ? "Emulating the page's colour scheme needs the desktop app"
                : schemeSuspended
                  ? `${schemeLabel} is not in force — Chromium's debugger is in use elsewhere`
                  : // Whose preference, and where the next click goes. "The page's"
                    // is load-bearing: Veld themes itself light and dark too, and a
                    // sun beside the device button would otherwise be taken for
                    // that one.
                    `The page's colour scheme: ${schemeLabel} — click for ${nextSchemeLabel}`
            }
          >
            <ActionIcon
              size="sm"
              variant={scheme ? "light" : "subtle"}
              // Yellow rather than blue for a scheme that is set and not in force —
              // the same distinction the device menu draws in words for touch.
              color={scheme ? (schemeSuspended ? "yellow" : "blue") : "gray"}
              // Not `aria-pressed`: three states are a cycle, not a pressed
              // toggle, and a two-valued attribute would have to lie about one of
              // them. The label carries the state instead, the way the app's own
              // theme button does.
              // Same rule as the device switch's name: `aria-disabled` with no
              // reason is the screen-reader equivalent of the unreachable tooltip
              // `bbaa9b7` fixed, so the reason belongs in the name too.
              aria-label={
                iframeBackend
                  ? `Page colour scheme: ${schemeLabel} — needs the desktop app`
                  : schemeSuspended
                    ? `Page colour scheme: ${schemeLabel}, paused — Chromium's debugger is in use elsewhere`
                    : `Page colour scheme: ${schemeLabel} — click for ${nextSchemeLabel}`
              }
              data-disabled={iframeBackend || undefined}
              aria-disabled={iframeBackend || undefined}
              onClick={() => {
                // `data-disabled` styles but does not disable, so the refusal lives
                // here. Silent rather than a toast: the tooltip already answers it,
                // and an error for clicking a control that looks inert is noise.
                if (iframeBackend) return;
                applyMedia(
                  withMediaFeature(
                    media,
                    "prefers-color-scheme",
                    nextColorScheme(scheme),
                  ),
                );
              }}
            >
              {/* Sun and moon match the app's own theme button, because they answer
                  the same question. System does **not** reuse its
                  `IconDeviceDesktop` — the device picker is the button immediately
                  to the left, where two monitor shapes next to each other would read
                  as two device controls. */}
              {scheme === "dark" ? (
                <IconMoon size={14} />
              ) : scheme === "light" ? (
                <IconSun size={14} />
              ) : (
                <IconSunMoon size={14} />
              )}
            </ActionIcon>
          </Tooltip>
        )}

        {/* Detached, always — a docked inspector resizes the view from the inside
            while the renderer mirrors the pane's box from the outside, and the two
            fight. In a browser tab the page has the browser's own inspector, so
            this is the one control with nothing to fall back to. */}
        <Tooltip
          {...TIP}
          label={
            iframeBackend
              ? "DevTools for a pane needs the desktop app"
              : state.devToolsOpen
                ? "Close DevTools"
                : "Inspect this pane — opens a separate window"
          }
        >
          <ActionIcon
            size="sm"
            variant={state.devToolsOpen ? "light" : "subtle"}
            color={state.devToolsOpen ? "blue" : "gray"}
            aria-label={state.devToolsOpen ? "Close DevTools" : "Open DevTools"}
            disabled={iframeBackend}
            onClick={() => browserDevTools(id, "toggle")}
          >
            <IconCode size={14} />
          </ActionIcon>
        </Tooltip>

        <Tooltip {...TIP} label="Open in your system browser">
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label="Open in the system browser"
            disabled={external === ""}
            onClick={() => window.open(external, "_blank", "noreferrer")}
          >
            <IconExternalLink size={14} />
          </ActionIcon>
        </Tooltip>
      </div>

      {/* The view's box. Nothing may be painted over this — under Electron the
          content is a native view that ignores z-index. The placeholder below
          only renders while there is no page, so it never overlaps one; the resize
          handles sit in the gap *around* the emulated screen, which is DOM the
          native view does not cover. */}
      {/* Above the slot, never over it: a native view paints over DOM whatever
          the z-index says, so an overlaid prompt would be invisible under
          Electron — the one backend that can raise one. */}
      {prompt && (
        <div className="permission-prompt" role="alertdialog" aria-label="Permission request">
          <IconShieldLock size={16} />
          <span className="permission-ask">
            <strong>{prompt.origin}</strong> wants to {permissionSentence(prompt.permissions)}
            {/* Which pane, and whose cookie jar — the attribution a native dialog
                cannot give, and the reason panes refused every permission before. */}
            <span className="faint">
              {" · "}
              {browserProfileLabel(prompt.profile)} session
              {!prompt.isMainFrame && " · asked by a frame inside the page"}
            </span>
          </span>
          <Button size="compact-xs" variant="default" onClick={() => answer("deny")}>
            Block
          </Button>
          <Button size="compact-xs" onClick={() => answer("allow")}>
            Allow
          </Button>
        </div>
      )}

      {confirmClear && (
        /* An in-pane bar, not a Mantine Modal: an embedded `WebContentsView`
           ignores z-index, so a portalled dialog renders *behind* the page (#188).
           The permission prompt above solved this the same way, and this reuses its
           chrome so the two read as the same class of thing. */
        <div
          className="permission-prompt"
          role="alertdialog"
          aria-label="Confirm clearing session data"
        >
          <IconTrash size={16} />
          <span className="permission-ask">
            Clear{" "}
            <strong>
              {confirmClear === "all"
                ? "every browser session"
                : `the ${browserProfileLabel(confirmClear)} session`}
            </strong>
            ? This signs out every pane using it and cannot be undone.
          </span>
          <Button
            size="compact-xs"
            variant="default"
            onClick={() => setConfirmClear(null)}
          >
            Cancel
          </Button>
          <Button
            size="compact-xs"
            color="red"
            onClick={() => {
              if (confirmClear === "all") {
                BROWSER_PROFILES.forEach(clearBrowserSession);
              } else {
                clearBrowserSession(confirmClear);
              }
              setConfirmClear(null);
            }}
          >
            Clear
          </Button>
        </div>
      )}

      {/* Same row-in-flow trick as the two prompts above: it shrinks the slot
          instead of painting over it, which is the only way a DOM element gets
          to coexist with a native view that ignores z-index. */}
      {findOpen && canFind && (
        <div className="browser-find" role="search" aria-label="Find in page">
          <IconSearch size={14} className="browser-find-icon" aria-hidden />
          {/* A plain input, not Mantine's `TextInput`, to match the address bar two
              rows up (`.browser-address`) — same pill height, monospace and focus
              treatment. `TextInput`'s own wrapper and default sizing would need
              overriding to match it anyway, and two different input styles this
              close together would read as two toolbars, not one. */}
          <input
            ref={findInputRef}
            className="browser-find-input"
            value={findQuery}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            placeholder="Find in page"
            aria-label="Find in page"
            onChange={(e) => setFindQuery(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                if (e.shiftKey) findPrevious(id, findQuery);
                else findNext(id, findQuery);
              } else if (e.key === "Escape") {
                e.preventDefault();
                closeFind();
              }
            }}
          />
          <span className="browser-find-count faint">
            {findQuery === "" || findPending
              ? ""
              : findResult && findResult.matches > 0
                ? `${findResult.activeMatchOrdinal} of ${findResult.matches}`
                : "No results"}
          </span>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label="Previous match"
            disabled={!findResult?.matches}
            onClick={() => findPrevious(id, findQuery)}
          >
            <IconChevronUp size={14} />
          </ActionIcon>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label="Next match"
            disabled={!findResult?.matches}
            onClick={() => findNext(id, findQuery)}
          >
            <IconChevronDown size={14} />
          </ActionIcon>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label="Close find bar"
            onClick={closeFind}
          >
            <IconX size={14} />
          </ActionIcon>
        </div>
      )}

      <div className="browser-slot" ref={slot}>
        {/* The suggestions, over a page that is already loaded.

            **A dimmed overlay inside the slot, and the native view is hidden while it
            is up.** It used to sit in flow between the chrome and the slot, because a
            `WebContentsView` paints over DOM whatever the z-index says — so a panel
            positioned over a live view is invisible in the desktop app and perfectly
            visible in a browser tab, the worst of both. The cost was that typing an
            address shoved the page down by up to 60% of the pane and reflowed it on
            every keystroke. `setBrowserOverlay` (the effect beside `panelOpen`) takes
            the view down and leaves a still of the page in its place, so there is
            something real to dim rather than an empty pane.

            **First among the slot's children, and above every `.browser-screen` by
            z-index rather than by DOM order.** Those two facts are not in tension: the
            screens are `z-index: 1` and this is `2`, so painting is decided by the
            numbers, which leaves DOM order free to be the *tab* order. Rendering it
            last put the error and spinner screens' buttons — dimmed under the scrim —
            ahead of the suggestion rows when tabbing out of the address bar. It has to
            be above them either way: the address bar is how you leave a page that
            failed to load, so the panel must be readable over the error screen.

            Not rendered while the pane is blank: there the start page below *is* this
            list, at pane size, and two copies of it would be the duplication that made
            the blank pane and the new-pane chooser indistinguishable in the first
            place. */}
        {panelOpen && (
          <div
            className="suggest-overlay"
            // Swallow `mousedown` **inside the panel** so the address bar never loses
            // focus to a row. That is what lets `onBlur` close the panel — the two
            // together are one mechanism: blur means "the user went somewhere else",
            // and a click on a row is not going somewhere else. `preventDefault` on
            // mousedown is the only event that stops focus moving; a blur handler that
            // tried to guess would race the click.
            //
            // **The scrim is exempt, and that is the point.** Preventing it there too
            // made clicking the dimmed page cost two clicks: the first closed the panel
            // through a handler of its own while focus stayed pinned in the field, so
            // the bar was still focused with no list under it, and only a second click
            // blurred it. Letting mousedown's default action run on the scrim blurs the
            // field, and `onBlur` closes the panel — one click, one mechanism, and no
            // second path that can disagree with it.
            onMouseDown={(e) => {
              if (e.target !== e.currentTarget) e.preventDefault();
            }}
          >
            <div className="suggest-panel">
              <PlaceList
                suggestions={suggestions}
                activeIndex={active}
                listboxId={suggestId(id)}
                emptyHint={props.urlsEmptyHint}
                onOpen={(url, title, path) =>
                go(url, { title: path ? fileLabel(path) : title })
              }
              />
              {/* A sibling of the list, never a child of it: that list is the listbox
                  the address bar names through `aria-controls`, and a listbox may own
                  options and nothing else. */}
              {suggestions.bookmarks.length > 0 && (
                <BookmarksButton
                  count={suggestions.bookmarks.length}
                  onOpen={() => {
                    closeSuggestions();
                    setBookmarksOpen(true);
                  }}
                />
              )}
            </div>
          </div>
        )}

        {/* Drag any edge to resize the emulated screen — the answer to "which
            width does this break at", which no list of devices can give you. The
            handles are only reachable because an emulated screen is inset from the
            pane: under Electron the view covers its own rect and swallows the
            pointer there. */}
        {emulation && !covered && (
          <>
            <div
              className="device-handle east"
              data-dragging={drag ? "true" : undefined}
              role="separator"
              aria-label="Resize the emulated screen horizontally"
              title="Drag to change the emulated width"
              style={{
                left: screen.x + screen.width + HANDLE_EDGE_GAP,
                top: screen.y + screen.height / 2 - HANDLE_LENGTH / 2,
                ...bleed(HANDLE_HIT_BLEED),
              }}
              onPointerDown={(e) => startResize(e, "x")}
            />
            <div
              className="device-handle south"
              data-dragging={drag ? "true" : undefined}
              role="separator"
              aria-label="Resize the emulated screen vertically"
              title="Drag to change the emulated height"
              style={{
                left: screen.x + screen.width / 2 - HANDLE_LENGTH / 2,
                top: screen.y + screen.height + HANDLE_EDGE_GAP,
                ...bleed(HANDLE_HIT_BLEED),
              }}
              onPointerDown={(e) => startResize(e, "y")}
            />
            <div
              className="device-handle corner"
              data-dragging={drag ? "true" : undefined}
              role="separator"
              aria-label="Resize the emulated screen"
              title="Drag to change the emulated size"
              style={{
                left: screen.x + screen.width + HANDLE_CORNER_GAP,
                top: screen.y + screen.height + HANDLE_CORNER_GAP,
                ...bleed(HANDLE_CORNER_HIT_BLEED),
              }}
              onPointerDown={(e) => startResize(e, "both")}
            />
          </>
        )}
        {/* No readout over the screen: the page is live and reflowing there now, and
            a native view paints over DOM — so anything here would be invisible in
            the desktop app and present in a browser tab, which is the worst of both.
            The size being dragged to goes in the chrome's chip instead, where it is
            the same number in the same place it always is. */}
        {/* Everything below stands in for the native view, and only ever while
            it is hidden — `covered()` in browserHost decides that from the same
            state, so a screen can never end up painted under a live page. The
            frozen still is not here: browserHost paints it on the container
            itself, because a React render plus an image decode was a visible
            frame of nothing. */}
        {chooser && (
          // Nothing loaded yet, so the pane is the run's own start page. This is
          // the whole reason there is no separate URLs pane: the list belongs in
          // the thing that is about to become the page.
          //
          // It has to be tellable apart from the new-pane chooser at a glance, which
          // it was not: the chooser also showed this list, so a blank browser read as
          // the chooser plus an empty bar, and the user's question was "there is no
          // URL in here, how do I use it?". Two things answer it. The heading names
          // *this* pane's one question — where should it go — where the chooser asks
          // what a pane should be. And typing into the bar above puts what it will do
          // at the top of this list, ringed, so the field's effect is visible before
          // Enter is pressed. **Not** a focused caret: autofocus was tried and taken
          // back out (see the note beside `chooser` above), so do not reintroduce it
          // on the strength of this comment.
          <div className="browser-screen start">
            <div className="start-head">
              <div className="start-head-text">
                <p className="pane-screen-title">Where to?</p>
                <p className="faint">
                  {props.searchUrl.trim() === ""
                    ? "Type an address in the bar above, or pick one below."
                    : "Type an address in the bar above, search the web from it, or pick one below."}
                </p>
              </div>
              {/* The same two controls the chooser's heading carries, in the same
                  corner and icon-only for the same reason. No blank-pane button
                  beside them: this pane already *is* one.

                  This screen offers the same three-from-the-last-day list the chooser
                  does — see `startPlaces` above. An earlier round had it keep the
                  panel's longer list; that is no longer true and the note said so for
                  one round too long. */}
              {/* Wrapped, because `.start-head` is `space-between` over its children:
                  with one button that put it at the far edge, and with two it put the
                  slack *between them*. The chooser's heading already solved this the
                  same way — one actions group is one flex child. */}
              <div className="start-head-actions">
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
                  aria-label={`Project bookmarks (${allBookmarks.length})`}
                  onClick={() => setBookmarksOpen(true)}
                >
                  <IconBookmark size={13} />
                </ActionIcon>
              </Tooltip>
              </div>
            </div>
            {/* Swallows `mousedown`, exactly as the suggestion panel does, and for the
                same reason: a click on a row must not blur the address bar first. This
                list is filtered by what is in that bar, and blurring it re-runs the
                draft-sync effect above — which on a blank pane writes `draft = ""`,
                since `chooser` means there is no page URL to restore. The list then
                widens between mousedown and mouseup, the row moves out from under the
                pointer, and the click lands on the container instead of the row.
                Gating a *flag* could never fix that; the draft wipe does it on its own,
                which is why both surfaces need the same one mechanism.

                What this gives up, since `mousedown` is also where a native selection
                begins: you can no longer drag-select the URL text in a row. Each row
                carries an explicit copy button, which is the better affordance for the
                thing that text is for. `click` is untouched — `preventDefault` cancels
                mousedown's own default action, not the click that follows it — so every
                button and the open-externally anchor still work, as does the keyboard. */}
            <div onMouseDown={(e) => e.preventDefault()}>
              <PlaceList
                suggestions={suggestions}
                activeIndex={active}
                emptyHint={props.urlsEmptyHint}
                onOpen={(url, title, path) =>
                go(url, { title: path ? fileLabel(path) : title })
              }
              />
            </div>
          </div>
        )}
        {opening && (
          <div className="browser-screen" role="status">
            <Loader size="sm" />
            <p className="faint">{urlLabel(state.url || tab.url)}</p>
            {slow && (
              <>
                <p className="faint">This is taking a while.</p>
                <button className="btn big" onClick={() => reloadBrowser(id)}>
                  <IconRefresh size={15} /> Reload
                </button>
              </>
            )}
          </div>
        )}
        {nested && (
          // Pointing a pane at Veld's own UI is the first thing anyone tries, and
          // this is the moment to be funny rather than to show an error — but the
          // reason it is caught at all is in the second line, and it is not the
          // joke: a nested instance is a whole second copy of this app talking to
          // the same daemon.
          <div className="browser-screen" role="status">
            <span className="pane-screen-icon">
              <IconInfinity size={26} />
            </span>
            <p className="pane-screen-title">You are already here</p>
            <p>
              A Veld inside your Veld would open its own terminals against the same
              daemon, spend the session budget twice and write your pane layout from
              two places at once. One of you is enough.
            </p>
            <div className="browser-suggestions">
              {external && (
                <button
                  className="btn big"
                  onClick={() => window.open(external, "_blank", "noreferrer")}
                >
                  <IconExternalLink size={15} /> Open in system browser
                </button>
              )}
              {/* The escape hatch stays, because the guard cannot be certain: a dev
                  instance on a management host of its own is missed, and somebody
                  else's `/ide` route on this origin would be caught. Both are the
                  user's call to overrule. */}
              {/* The *refused* URL, not the address bar's draft: the user may
                  have typed something else in it without pressing Enter, and this
                  button means "that one, anyway". */}
              <button className="btn big" onClick={() => go(nested, { force: true })}>
                <IconArrowRight size={15} /> Load it here anyway
              </button>
            </div>
            <p className="pane-screen-url">{nested}</p>
          </div>
        )}
        {failure && (
          <div className="browser-screen" role="alert">
            <span className="pane-screen-icon">
              {ERROR_ICONS[failure.kind]}
            </span>
            <p className="pane-screen-title">{failure.title}</p>
            <p className="faint">{failure.hint}</p>
            <div className="browser-suggestions">
              <button className="btn big" onClick={() => reloadBrowser(id)}>
                <IconRefresh size={15} /> Try again
              </button>
              {external && (
                <button
                  className="btn big"
                  onClick={() => window.open(external, "_blank", "noreferrer")}
                >
                  <IconExternalLink size={15} /> Open in system browser
                </button>
              )}
            </div>
            {state.error?.url && (
              <p className="pane-screen-url">{state.error.url}</p>
            )}
          </div>
        )}
      </div>

      <BookmarksModal
        bookmarks={allBookmarks}
        opened={bookmarksOpen}
        onClose={() => setBookmarksOpen(false)}
        onOpen={(url, title) => {
          setBookmarksOpen(false);
          go(url, { title });
        }}
      />
      {/* Every file, not the panel's capped rows — the modal is the unbounded view
          and its search field is why. Built from `props.files` directly rather than
          from `places`, which has the run's URLs and the bookmarks mixed in. */}
      <FilesModal
        files={placesFor([], [], props.files)}
        serving={props.filesServing}
        opened={filesOpen}
        onClose={() => setFilesOpen(false)}
        onOpen={(url, title, path) => {
          setFilesOpen(false);
          go(url, { title: path ? fileLabel(path) : title });
        }}
      />

      {!state.error && iframeBackend && (state.url || tab.url) && (
        <div className="browser-note" role="status">
          <span>
            Framed preview — a page sending <code>X-Frame-Options</code> renders
            blank here. History and separate sessions need the desktop app.
          </span>
        </div>
      )}
    </div>
  );
}
