/**
 * The project's own badges, buttons and menus in the top bar (`ide.extensions`).
 *
 * Three types render here. A **status** badge shows what its command printed and
 * can carry a link and actions; an **action** is a button that runs a command; a
 * **menu** groups actions into one control, which is what keeps a 42px bar
 * usable once a project declares more than two of them.
 *
 * Two rules the rest of this file exists to hold:
 *
 * - **Nothing here holds a command.** Every call names an extension `id` and the
 *   daemon resolves it against the project's config. The same is true of the
 *   actions a badge offers: the daemon has already resolved those ids, so the
 *   list this component receives is exactly the set the config declares.
 * - **Polling belongs to the visible worktree only.** The effect below asks for
 *   one worktree — the one on screen — and re-asks on the shortest declared
 *   interval while the document is visible. A hidden window asks for nothing,
 *   which is what keeps a badge that calls a rate-limited API from being
 *   evaluated for eighteen worktrees nobody is looking at.
 */
import { ActionIcon, Button, Menu, Tooltip } from "@mantine/core";
import { IconChevronDown, IconExternalLink, IconRefresh } from "@tabler/icons-react";
import { useContextMenu } from "mantine-contextmenu";
import React from "react";
import { type ExtensionSpec, type ExtensionStatus, api } from "../api";
import { paneIcon } from "../panes/paneIcons";
import { notifyError } from "../shared/notify";

/** The slot this component renders. One today — see `EXTENSION_SLOTS` in Rust. */
const SLOT = "topBar";

/**
 * Floor on how often the poll fires, whatever the config asked for.
 *
 * The daemon enforces its own minimum and answers a too-early request from the
 * run it already made, so this is not the security bound — it is here so a
 * project declaring the minimum does not make the browser wake up more often
 * than the daemon will ever answer differently.
 */
const MIN_POLL_MS = 15_000;

/** What one extension looks like once availability has been decided. */
type Resolved = {
  spec: ExtensionSpec;
  /** Rendered greyed rather than live. */
  disabled: boolean;
  /** Why it is greyed, for a tooltip. */
  reason: string | undefined;
};

/**
 * Decide whether an extension renders, and how.
 *
 * `when_missing` is the project's answer and it **wins over the user's
 * `hideDisabledActions` preference**: that preference is about hiding
 * inapplicable core actions, while an extension asking for `hint` is the project
 * telling a newcomer what to install. Silencing that would delete the lesson the
 * feature exists to teach.
 */
export function resolveExtension(spec: ExtensionSpec): Resolved | null {
  if (spec.available) return { spec, disabled: false, reason: undefined };
  if (spec.when_missing === "hide") return null;
  const missing = spec.missing?.join(", ");
  const needs = missing ? `Needs ${missing}` : "Not available on this machine";
  const reason =
    spec.when_missing === "hint" && spec.hint?.text ? `${needs}. ${spec.hint.text}` : needs;
  return { spec, disabled: true, reason };
}

/**
 * The shortest interval any visible status extension asked for, in ms.
 *
 * One timer for the whole slot rather than one per badge: the request is batched
 * per worktree anyway, so N timers would only produce N partial requests. The
 * daemon's per-extension rate limit is what stops a slow badge being re-run at a
 * fast one's cadence.
 */
export function pollInterval(specs: ExtensionSpec[]): number {
  const declared = specs
    .filter((s) => s.kind === "status" && s.available)
    .map((s) => (s.refresh_seconds ?? 60) * 1000);
  if (declared.length === 0) return 0;
  return Math.max(MIN_POLL_MS, Math.min(...declared));
}

export function TopBarExtensions(props: {
  /** All of the worktree's declarations, in config order. */
  extensions: ExtensionSpec[];
  /** Which side of the bar this instance renders. */
  align: "start" | "end";
  /** The worktree the badges describe. Every call is scoped to it. */
  worktreeId: number | null;
  /** Open a URL in a browser pane, or `undefined` when there is no layout to
   *  open into — then a `pane` link falls back to the system browser rather
   *  than doing nothing. */
  onOpenInPane: ((url: string) => void) | undefined;
}) {
  const { worktreeId } = props;
  const mine = React.useMemo(
    () =>
      props.extensions
        .filter((e) => e.slot === SLOT && (e.align ?? "start") === props.align)
        .map(resolveExtension)
        .filter((r): r is Resolved => r !== null),
    [props.extensions, props.align],
  );
  const hasStatus = mine.some((r) => r.spec.kind === "status" && !r.disabled);

  const [values, setValues] = React.useState<Record<string, ExtensionStatus>>({});
  // Keyed by worktree so a switch cannot render the previous worktree's PR
  // number against the new one for a poll's width. Effects cannot un-render a
  // value that is already on screen, so the guard has to be in the data.
  const [valuesFor, setValuesFor] = React.useState<number | null>(null);
  /**
   * Which badges have a request in flight, or `"*"` for the whole slot.
   *
   * Needed because a first evaluation runs a command that may take seconds, and
   * without it the bar simply has a hole in it for that long — the user cannot
   * tell "still working" from "this project declares nothing".
   */
  const [busy, setBusy] = React.useState<Set<string>>(new Set());
  const interval = pollInterval(mine.map((r) => r.spec));

  /**
   * Fetch this worktree's badge values.
   *
   * One function for the poll and for both Refresh entries, so a forced refresh
   * lands in state the same way a polled one does. `live` is a ref rather than a
   * closure flag because a Refresh outlives the effect that a poll belongs to.
   */
  const load = React.useCallback(
    (opts?: { force?: boolean; id?: string }) => {
      if (worktreeId === null) return;
      const mark = opts?.id ?? "*";
      setBusy((prev) => new Set(prev).add(mark));
      api
        .extensionStatus(worktreeId, opts)
        .then((res) => {
          const next: Record<string, ExtensionStatus> = {};
          for (const item of res.items) next[item.id] = item;
          setValues(next);
          setValuesFor(worktreeId);
        })
        // A background poll is deliberately silent — a daemon restart or a
        // sleeping laptop would otherwise raise a toast per badge, and a badge
        // that cannot be evaluated already renders as absent. A refresh the user
        // clicked is not silent: they are owed an answer.
        .catch((e) => {
          if (opts?.force) notifyError("Refresh this project's badges", e);
        })
        .finally(() =>
          setBusy((prev) => {
            const next = new Set(prev);
            next.delete(mark);
            return next;
          }),
        );
    },
    [worktreeId],
  );

  React.useEffect(() => {
    if (worktreeId === null || !hasStatus || interval === 0) {
      setValues({});
      setValuesFor(null);
      return;
    }
    let live = true;
    const poll = () => {
      // A hidden window asks for nothing: the badge is not on screen, and the
      // command behind it may cost an API call.
      if (!document.hidden && live) load();
    };
    poll();
    const timer = window.setInterval(poll, interval);
    const onVisible = () => {
      if (!document.hidden) poll();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      live = false;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [worktreeId, hasStatus, interval, load]);

  if (mine.length === 0 || worktreeId === null) return null;

  const valueFor = (id: string) => (valuesFor === worktreeId ? values[id] : undefined);

  const statusCount = mine.filter((r) => r.spec.kind === "status" && !r.disabled).length;

  const activate = (id: string) => {
    api
      .activateExtension(worktreeId, id)
      // An action usually changes what a badge says — creating the pull request
      // the badge just reported missing is the flagship case — so the badges are
      // re-read rather than left stale until the next interval. Forced, since the
      // interval has almost certainly not elapsed.
      .then(() => {
        if (statusCount > 0) load({ force: true });
      })
      .catch((e) => notifyError("Run this action", e));
  };
  const open = (url: string, where: "system" | "pane") => {
    if (where === "pane" && props.onOpenInPane) {
      props.onOpenInPane(url);
      return;
    }
    // `noopener` and a real tab: in the desktop shell this is handed to the OS
    // browser, which is the point — the page is one the user is already signed
    // into there.
    window.open(url, "_blank", "noopener,noreferrer");
  };

  return (
    <>
      {mine.map((r) => (
        <ExtensionControl
          key={r.spec.id}
          resolved={r}
          value={valueFor(r.spec.id)}
          busy={busy.has(r.spec.id) || busy.has("*")}
          all={props.extensions}
          statusCount={statusCount}
          onActivate={activate}
          onOpen={open}
          onRefresh={load}
        />
      ))}
    </>
  );
}

function ExtensionControl(props: {
  resolved: Resolved;
  value: ExtensionStatus | undefined;
  /** A request covering this extension is in flight. */
  busy: boolean;
  all: ExtensionSpec[];
  /** How many live badges the slot has, so "Refresh all" only appears when it
   *  would mean something more than "Refresh". */
  statusCount: number;
  onActivate: (id: string) => void;
  onOpen: (url: string, where: "system" | "pane") => void;
  onRefresh: (opts?: { force?: boolean; id?: string }) => void;
}) {
  const { spec, disabled, reason } = props.resolved;
  if (spec.kind === "menu") return <ExtensionMenu {...props} />;
  if (spec.kind === "action") {
    return (
      <Tooltip label={reason ?? spec.description ?? spec.label} withArrow openDelay={300}>
        {/* A disabled Mantine control has `pointer-events: none`, so the tooltip
            needs a wrapper to have anything to hover. */}
        <span style={{ display: "inline-flex" }}>
          {spec.icon ? (
            <ActionIcon
              variant="subtle"
              size="sm"
              className="ext-action"
              disabled={disabled}
              aria-label={spec.label}
              onClick={() => props.onActivate(spec.id)}
            >
              {paneIcon(spec.icon, 14)}
            </ActionIcon>
          ) : (
            // No icon means the label is the only thing identifying it, so it is
            // rendered rather than hidden behind a hover. `paneIcon`'s own
            // fallback is a terminal glyph, which would be a lie here.
            <Button
              variant="subtle"
              size="compact-xs"
              className="ext-action-label"
              disabled={disabled}
              onClick={() => props.onActivate(spec.id)}
            >
              {spec.label}
            </Button>
          )}
        </span>
      </Tooltip>
    );
  }
  return <ExtensionBadge {...props} />;
}

/**
 * A status badge.
 *
 * Absent rather than empty when there is nothing to show: a command that exited
 * 0 with no output has said "not applicable to this worktree", and a hole in the
 * bar is the honest rendering of that. A *failed* command is the opposite — it
 * renders, in `danger`, with the tool's own last line in the tooltip, because a
 * badge that silently stopped updating is the worst outcome a status indicator
 * has.
 */
function ExtensionBadge(props: {
  resolved: Resolved;
  value: ExtensionStatus | undefined;
  busy: boolean;
  statusCount: number;
  onActivate: (id: string) => void;
  onOpen: (url: string, where: "system" | "pane") => void;
  onRefresh: (opts?: { force?: boolean; id?: string }) => void;
}) {
  const { spec, disabled, reason } = props.resolved;
  const value = props.value;
  const { showContextMenu } = useContextMenu();

  if (disabled) {
    const hintHref = spec.hint?.href;
    return (
      <Tooltip
        label={hintHref ? `${reason} (click to open)` : reason}
        withArrow
        openDelay={200}
        multiline
        w={260}
      >
        <Button
          variant="light"
          size="compact-sm"
          className="ext-badge ext-badge-hint tone-neutral"
          leftSection={spec.icon ? paneIcon(spec.icon, 12) : undefined}
          style={hintHref ? undefined : { cursor: "default" }}
          onClick={hintHref ? () => props.onOpen(hintHref, "system") : undefined}
        >
          {spec.label}
        </Button>
      </Tooltip>
    );
  }
  // The first evaluation. A badge's command can take seconds — a network call to
  // a code host is the flagship case — and with nothing rendered the bar just has
  // a gap in it, which is indistinguishable from a project that declares no
  // badges. So the declared label appears with a spinner until there is an answer.
  if (!value) {
    if (!props.busy) return null;
    return (
      <Tooltip label={`Checking ${spec.label}…`} withArrow openDelay={200}>
        <Button
          variant="light"
          size="compact-sm"
          className="ext-badge ext-badge-loading tone-neutral"
          loading
          loaderProps={{ size: 11 }}
          style={{ cursor: "default" }}
        >
          {spec.label}
        </Button>
      </Tooltip>
    );
  }
  // Nothing to show: the command exited 0 with no output, which is how an
  // extension says "not applicable to this worktree".
  if (value.state === "empty") return null;

  const href = value.href;
  const actions = value.actions ?? [];
  const clickable = Boolean(href) || actions.length > 0;
  const tooltip = [value.tooltip, value.age_seconds > 60 ? `${value.age_seconds}s ago` : null]
    .filter(Boolean)
    .join(" · ");

  // The output's icon wins over the declaration's: that is what lets one badge
  // change its glyph with its state (a merge mark once merged) while still having
  // a sensible one before the first run.
  const glyph = value.icon ?? spec.icon;
  const badge = (
    <Button
      variant="light"
      size="compact-sm"
      className={`ext-badge tone-${value.tone}`}
      // `loading` keeps the label and swaps the glyph for a centred spinner, so a
      // refresh does not change the badge's width and shuffle the bar.
      loading={props.busy}
      loaderProps={{ size: 11 }}
      leftSection={glyph ? paneIcon(glyph, 12) : undefined}
      style={clickable ? undefined : { cursor: "default" }}
      onClick={
        // One link and no actions is a plain link; one action and no link runs
        // it. Anything more needs the menu below, so this handler is not used.
        !clickable || (href && actions.length > 0) || actions.length > 1
          ? undefined
          : href
            ? () => props.onOpen(href, value.open_in)
            : () => props.onActivate(actions[0].id)
      }
    >
      {value.text ?? spec.label}
    </Button>
  );

  const wrapped = tooltip ? (
    <Tooltip label={tooltip} withArrow openDelay={200} multiline w={300}>
      {badge}
    </Tooltip>
  ) : (
    badge
  );

  // **Refresh lives on the right-click menu, never on the primary click.** A
  // badge whose one useful meaning is "open the pull request" must keep that as a
  // single click; promoting Refresh into a left-click dropdown would cost every
  // badge its directness to expose something wanted once a week. Uses
  // `mantine-contextmenu`, the same mechanism the worktree rail's rows use, so a
  // right-click anywhere in the app behaves the same way.
  const refreshMenu = showContextMenu([
    {
      key: "refresh",
      icon: <IconRefresh size={14} />,
      title: `Refresh ${spec.label}`,
      onClick: () => props.onRefresh({ force: true, id: spec.id }),
    },
    // Only when it would mean more than the entry above it.
    ...(props.statusCount > 1
      ? [
          {
            key: "refresh-all",
            icon: <IconRefresh size={14} />,
            title: "Refresh all badges",
            onClick: () => props.onRefresh({ force: true }),
          },
        ]
      : []),
  ]);

  // A badge with both a link and actions, or with several actions, still needs a
  // left-click menu: one click cannot mean two things.
  const body =
    (href && actions.length > 0) || actions.length > 1 ? (
      <Menu position="bottom-start" withinPortal>
        <Menu.Target>{wrapped}</Menu.Target>
        <Menu.Dropdown>
          {href && (
            <Menu.Item
              leftSection={<IconExternalLink size={14} />}
              onClick={() => props.onOpen(href, value.open_in)}
            >
              Open
            </Menu.Item>
          )}
          {actions.map((a) => (
            <Menu.Item key={a.id} onClick={() => props.onActivate(a.id)}>
              {a.label}
            </Menu.Item>
          ))}
        </Menu.Dropdown>
      </Menu>
    ) : (
      wrapped
    );

  return (
    <span style={{ display: "inline-flex" }} onContextMenu={refreshMenu}>
      {body}
    </span>
  );
}

/**
 * A group of actions in one control.
 *
 * Members are looked up in the full declaration list by id — the daemon has
 * already dropped any that did not resolve, so a member missing here means it was
 * declared `hide` and is not installed. A menu whose every member has gone that
 * way renders nothing: the daemon drops a menu with no *declared* members, and
 * this handles the availability half of the same idea.
 */
function ExtensionMenu(props: {
  resolved: Resolved;
  all: ExtensionSpec[];
  onActivate: (id: string) => void;
}) {
  const { spec, disabled, reason } = props.resolved;
  const members = (spec.items ?? [])
    .map((id) => props.all.find((e) => e.id === id))
    .filter((e): e is ExtensionSpec => e !== undefined)
    .map(resolveExtension)
    .filter((r): r is Resolved => r !== null);
  if (members.length === 0) return null;

  return (
    <Menu position="bottom-start" withinPortal>
      <Menu.Target>
        <Tooltip label={reason ?? spec.description ?? spec.label} withArrow openDelay={300}>
          <span style={{ display: "inline-flex" }}>
            <ActionIcon
              variant="subtle"
              size="sm"
              className="ext-menu"
              disabled={disabled}
              aria-label={spec.label}
            >
              {spec.icon ? paneIcon(spec.icon, 14) : <IconChevronDown size={14} />}
            </ActionIcon>
          </span>
        </Tooltip>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Label>{spec.label}</Menu.Label>
        {members.map((m) => (
          <Menu.Item
            key={m.spec.id}
            disabled={m.disabled}
            leftSection={m.spec.icon ? paneIcon(m.spec.icon, 14) : undefined}
            onClick={() => props.onActivate(m.spec.id)}
          >
            {m.spec.label}
            {m.disabled && m.reason ? ` — ${m.reason}` : ""}
          </Menu.Item>
        ))}
      </Menu.Dropdown>
    </Menu>
  );
}

