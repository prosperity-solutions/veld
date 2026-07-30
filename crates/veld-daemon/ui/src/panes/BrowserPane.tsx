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
  IconBug,
  IconCheck,
  IconClockExclamation,
  IconCode,
  IconDeviceMobile,
  IconDevices,
  IconExternalLink,
  IconLockOff,
  IconMinus,
  IconPlugConnectedX,
  IconPlus,
  IconRefresh,
  IconRestore,
  IconRotateClockwise,
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
  urlLabel,
} from "./model";
import { VeldLinks } from "./VeldLinks";
import {
  DEFAULT_ZOOM,
  DEVICE_GROUPS,
  DEVICE_PADDING,
  DEVICE_PRESETS,
  HANDLE_CORNER_GAP,
  HANDLE_CORNER_HIT_BLEED,
  HANDLE_EDGE_GAP,
  HANDLE_HIT_BLEED,
  HANDLE_LENGTH,
  MAX_DEVICE_PX,
  MIN_DEVICE_PX,
  type PaneEmulation,
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
  orientationLabel,
  resizeEmulation,
  responsiveEmulation,
  rotateEmulation,
  withMobileUserAgent,
  zoomStep,
} from "./devices";
import { type BrowserErrorKind, describeBrowserError } from "./browserError";
import {
  browserBackend,
  browserCommand,
  browserDevTools,
  browserStatus,
  clearBrowserSession,
  mountBrowser,
  navigateBrowser,
  onBrowserPointer,
  paneCovers,
  previewBrowserResize,
  reloadBrowser,
  setBrowserEmulation,
  setBrowserResizing,
  setBrowserZoom,
  subscribeBrowser,
  unmountBrowser,
} from "./browserHost";

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
  /** Why there are none — only the app knows (no run, or no veld.json). */
  urlsEmptyHint: string;
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

  // Read through a ref rather than as a dependency: `mountBrowser` only uses the
  // URL when it *creates* a view, and re-running the effect on every navigation
  // would remount the view on its own output. It has to track `tab.url` all the
  // same, because a profile switch creates a new view and it must open where the
  // pane currently is, not where it was first opened.
  const currentUrl = useRef(tab.url);
  currentUrl.current = tab.url;

  // The layout is the record for the emulated device and the zoom; `browserHost`
  // holds the live copy. Read here so both the chrome and the mount below work
  // off the state that gets persisted.
  const emulation = tab.emulation ?? null;
  const zoom = tab.zoom ?? DEFAULT_ZOOM;

  // Refs for the same reason `currentUrl` is one: `mountBrowser` uses these only
  // when it *creates* a view — a first mount, or a session switch, which rebuilds
  // one — and making them effect dependencies would remount the view, reloading
  // the page, every time you picked a device or nudged the zoom.
  const currentEmulation = useRef(emulation);
  currentEmulation.current = emulation;
  const currentZoom = useRef(zoom);
  currentZoom.current = zoom;

  useEffect(() => {
    const el = slot.current;
    if (!el) return;
    // Mount first, then subscribe: a profile change disposes the old view
    // (dropping its listeners) and creates a new one, so subscribing first
    // would attach to the view that is about to go away.
    mountBrowser(id, el, {
      url: currentUrl.current,
      profile,
      emulation: currentEmulation.current,
      zoom: currentZoom.current,
    });
    const unsubscribe = subscribeBrowser(id, bump);
    return () => {
      unsubscribe();
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

  const external = state.url || tab.url || "";
  const canStop = state.loading && !iframeBackend;

  // Which screen stands in for the page. `paneCovers` is the *same* predicate that
  // hides the native view in browserHost — shared rather than restated, because the
  // two disagreeing means either a screen painted under a live page or a pane that
  // stays blank, and neither is visible in the browser build.
  const covered = paneCovers(state, tab.url);
  const failure = state.error ? describeBrowserError(state.error) : null;
  const chooser = covered && !failure && !state.url && !tab.url;
  const opening = covered && !failure && !chooser;
  const color = BROWSER_PROFILE_COLORS[profile];

  // Anything but the default is removable, including the one this pane is on:
  // removing it moves every pane using it back to Default. Refusing instead meant
  // the session you were looking at was the one you could never get rid of.
  const removable = props.sessions.filter((p) => p !== "default");

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

  const submit = () => {
    const target = navigateBrowser(id, draft);
    if (target) {
      setDraft(target);
      onTab({ url: target });
    }
  };

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

  const applyEmulation = (next: PaneEmulation | null) => {
    setBrowserEmulation(id, next);
    // `undefined`, not `null`: "no device" is the absence of the field, so a tab
    // that never emulated anything and one switched back to pane size serialise
    // the same way.
    onTab({ emulation: next ?? undefined });
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

        <input
          className="browser-address"
          value={draft}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          aria-label="Address"
          placeholder="Enter a URL"
          onChange={(e) => setDraft(e.currentTarget.value)}
          onFocus={(e) => {
            setEditing(true);
            e.currentTarget.select();
          }}
          onBlur={() => setEditing(false)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
              e.currentTarget.blur();
            } else if (e.key === "Escape") {
              setDraft(state.url || tab.url || "");
              e.currentTarget.blur();
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
                        onClick={() => clearBrowserSession(p)}
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
                      onClick={() =>
                        BROWSER_PROFILES.forEach(clearBrowserSession)
                      }
                    >
                      All sessions, including retired ones
                    </Menu.Item>
                  </Menu.Sub.Dropdown>
                </Menu.Sub>
              </Menu.Dropdown>
            </Menu>
          </span>
        </Tooltip>

        {/* Device emulation and zoom. One menu, because they are one question —
            "what size is this page being shown at" — and because a pane is a
            narrow strip: the chrome is already six controls wide before this. The
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
                  {emulation ? (
                    <IconDeviceMobile size={14} />
                  ) : (
                    <IconDevices size={14} />
                  )}
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
                      // Measured from the pane's own box, and genuinely skipped when
                      // there is none to measure — `state.device*` is the screen's drawn
                      // box, already inset and scaled, which is the wrong-but-plausible
                      // number this path exists to stop producing.
                      onClick={() => {
                        const box = paneSize();
                        if (!box) return;
                        applyEmulation(
                          responsiveEmulation(
                            box.width - DEVICE_PADDING * 2,
                            box.height - DEVICE_PADDING * 2,
                          ),
                        );
                      }}
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
                              onClick={() =>
                                applyEmulation(
                                  emulationForPreset(preset, {
                                    chrome: HOST_CHROME,
                                    fit: emulation?.fit ?? true,
                                  }),
                                )
                              }
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
      <div className="browser-slot" ref={slot}>
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
          <div className="browser-screen start">
            <VeldLinks
              urls={props.serviceUrls}
              emptyHint={props.urlsEmptyHint}
              onOpen={(name, url) => {
                const target = navigateBrowser(id, url);
                if (target) onTab({ url: target, title: name });
              }}
            />
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
