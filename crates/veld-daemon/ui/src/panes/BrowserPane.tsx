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

import { ActionIcon, Loader, Menu } from "@mantine/core";
import {
  IconAlertTriangle,
  IconArrowLeft,
  IconArrowRight,
  IconBug,
  IconClockExclamation,
  IconExternalLink,
  IconLockOff,
  IconMinus,
  IconPlugConnectedX,
  IconPlus,
  IconRefresh,
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
import { type BrowserErrorKind, describeBrowserError } from "./browserError";
import {
  browserBackend,
  browserCommand,
  browserStatus,
  clearBrowserSession,
  mountBrowser,
  navigateBrowser,
  reloadBrowser,
  subscribeBrowser,
  unmountBrowser,
} from "./browserHost";

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

  useEffect(() => {
    const el = slot.current;
    if (!el) return;
    // Mount first, then subscribe: a profile change disposes the old view
    // (dropping its listeners) and creates a new one, so subscribing first
    // would attach to the view that is about to go away.
    mountBrowser(id, el, { url: currentUrl.current, profile });
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

  // Which screen stands in for the page. Mirrors `covered()` in browserHost,
  // which is what actually hides the native view — the two must agree, or a
  // screen renders under a live page (invisible) or the pane shows nothing.
  const failure = state.error ? describeBrowserError(state.error) : null;
  const chooser = !failure && !state.url && !tab.url;
  const opening = !failure && !chooser && !state.loaded && state.loading;
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

  return (
    <div className="browser-pane">
      {/* The session's colour on the chrome's own edge: enough to tell two panes
          of the same app apart at a glance, and it costs no layout — a strip
          above the view would move the native view's box on every switch. */}
      <div
        className="browser-bar"
        style={color ? { borderBottomColor: color } : undefined}
      >
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          aria-label="Back"
          title={iframeBackend ? "History needs the desktop app" : "Back"}
          disabled={!state.canGoBack}
          onClick={() => browserCommand(id, "back")}
        >
          <IconArrowLeft size={14} />
        </ActionIcon>
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          aria-label="Forward"
          title={iframeBackend ? "History needs the desktop app" : "Forward"}
          disabled={!state.canGoForward}
          onClick={() => browserCommand(id, "forward")}
        >
          <IconArrowRight size={14} />
        </ActionIcon>
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          aria-label={canStop ? "Stop loading" : "Reload"}
          title={canStop ? "Stop loading" : "Reload"}
          onClick={() => (canStop ? browserCommand(id, "stop") : reloadBrowser(id))}
        >
          {canStop ? <IconX size={14} /> : <IconRefresh size={14} />}
        </ActionIcon>

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

        <Menu position="bottom-end" withinPortal>
          <Menu.Target>
            <ActionIcon
              size="sm"
              variant="subtle"
              color="gray"
              aria-label={`Browser session: ${browserProfileLabel(profile)}`}
              title={`${browserProfileLabel(profile)} — click to change`}
            >
              {color ? <SessionDot color={color} size={10} /> : <IconUserCircle size={14} />}
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
                leftSection={<SessionDot color={BROWSER_PROFILE_COLORS[p]} />}
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
                  {removable.length === 0 ? "Nothing to remove" : "Remove a session"}
                </Menu.Sub.Item>
              </Menu.Sub.Target>
              <Menu.Sub.Dropdown>
                <Menu.Label>
                  Frees the slot and returns its panes to Default; data is kept
                </Menu.Label>
                {removable.map((p) => (
                  <Menu.Item
                    key={p}
                    leftSection={<SessionDot color={BROWSER_PROFILE_COLORS[p]} />}
                    onClick={() => props.onRemoveSession(p)}
                  >
                    {browserProfileLabel(p)}
                  </Menu.Item>
                ))}
              </Menu.Sub.Dropdown>
            </Menu.Sub>
            <Menu.Sub>
              <Menu.Sub.Target>
                <Menu.Sub.Item leftSection={<IconTrash size={14} />} disabled={iframeBackend}>
                  Clear session data
                </Menu.Sub.Item>
              </Menu.Sub.Target>
              <Menu.Sub.Dropdown>
                <Menu.Label>Signs out every pane using it</Menu.Label>
                {props.sessions.map((p) => (
                  <Menu.Item
                    key={p}
                    leftSection={<SessionDot color={BROWSER_PROFILE_COLORS[p]} />}
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
                <Menu.Item onClick={() => BROWSER_PROFILES.forEach(clearBrowserSession)}>
                  All sessions, including retired ones
                </Menu.Item>
              </Menu.Sub.Dropdown>
            </Menu.Sub>
          </Menu.Dropdown>
        </Menu>

        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          aria-label="Open in the system browser"
          title="Open in system browser"
          disabled={external === ""}
          onClick={() => window.open(external, "_blank", "noreferrer")}
        >
          <IconExternalLink size={14} />
        </ActionIcon>
      </div>

      {/* The view's box. Nothing may be painted over this — under Electron the
          content is a native view that ignores z-index. The placeholder below
          only renders while there is no page, so it never overlaps one. */}
      <div className="browser-slot" ref={slot}>
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
            <span className="pane-screen-icon">{ERROR_ICONS[failure.kind]}</span>
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
            {state.error?.url && <p className="pane-screen-url">{state.error.url}</p>}
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
