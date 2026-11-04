import { Button, Menu, Text, Tooltip } from "@mantine/core";
import React from "react";
import { api, type WorktreeLink } from "../api";
import { paneIcon } from "../panes/paneIcons";
import { openExternally } from "../panes/terminalHost";
import {
  arrangeLinks,
  groupLabel,
  isStale,
  kindGlyph,
  type LinkGroup,
  type LinkKind,
  linkGlyph,
  linkHostPath,
  linkTooltip,
  type NormalLink,
  overflowCount,
  railPrs,
  withoutBadged,
} from "./linkModel";

/** The modifier state of the click that opens a link. */
export type OpenClick = Pick<MouseEvent, "metaKey" | "ctrlKey" | "shiftKey">;

/**
 * Open a link the way a link in this worktree's terminal opens.
 *
 * A modifier held down is the per-click "in my own browser". Otherwise the daemon
 * answers with the terminal's rule — `terminal.openUrlsInApp` and the exempt list
 * — so an internal page lands in a pane and an exempt origin (a code host the user
 * is signed in to, say) in the system browser. With no pane to open into, or no
 * answer, it is the system browser.
 */
export async function openLink(
  url: string,
  worktreeId: number,
  click: OpenClick,
  onOpenInPane: ((url: string) => void) | undefined,
): Promise<void> {
  if (click.metaKey || click.ctrlKey || click.shiftKey || !onOpenInPane) {
    openExternally(url);
    return;
  }
  try {
    const { target } = await api.linkOpenTarget(worktreeId, url);
    if (target === "pane") {
      onOpenInPane(url);
      return;
    }
  } catch {
    // The link still has to open; the system browser is where it would have
    // gone without Veld.
  }
  openExternally(url);
}

/**
 * The links agents pinned to this worktree (`veld ide link set`), as pills in the
 * top bar. Read-only: the agent that set a link is the one that changes it.
 *
 * Laid out per kind, not per count — see `arrangeLinks`. A kind with one link is a
 * pill; several fold into one pill with a menu; past three kinds the rest share a
 * "+N" menu. Renders nothing when there are none.
 */
export function WorktreeLinks(props: {
  links: WorktreeLink[];
  branch: string;
  worktreeId: number;
  /** Where the project's status badges currently link — a link to one of these is
   *  left out of the bar, since the badge already shows it. */
  badgeHrefs?: readonly string[];
  onOpenInPane?: (url: string) => void;
}) {
  const slots = React.useMemo(
    () => arrangeLinks(withoutBadged(props.links, props.badgeHrefs ?? [])),
    [props.links, props.badgeHrefs],
  );
  if (slots.length === 0) return null;
  const now = Date.now();
  const open = (l: NormalLink, click: OpenClick) =>
    void openLink(l.url, props.worktreeId, click, props.onOpenInPane);

  return (
    <>
      {slots.map((slot) => {
        switch (slot.type) {
          case "single":
            return (
              <SinglePill
                key={slot.link.id}
                link={slot.link}
                branch={props.branch}
                now={now}
                onOpen={open}
              />
            );
          case "group":
            return (
              <Menu key={`group-${slot.kind}`} position="bottom-start" withinPortal={true} shadow="md">
                <Menu.Target>
                  <Button
                    variant="light"
                    size="compact-sm"
                    className={`ext-badge tone-${slot.tone}${
                      slot.links.every((l) => isStale(l, props.branch)) ? " wt-link-stale" : ""
                    }`}
                    leftSection={paneIcon(kindGlyph(slot.kind), 12)}
                  >
                    {groupLabel(slot.kind, slot.links.length)}
                  </Button>
                </Menu.Target>
                <Menu.Dropdown>
                  <LinkItems links={slot.links} branch={props.branch} onOpen={open} />
                </Menu.Dropdown>
              </Menu>
            );
          case "overflow":
            return (
              <OverflowMenu
                key="overflow"
                groups={slot.groups}
                branch={props.branch}
                onOpen={open}
              />
            );
        }
      })}
    </>
  );
}

function SinglePill(props: {
  link: NormalLink;
  branch: string;
  now: number;
  onOpen: (l: NormalLink, click: OpenClick) => void;
}) {
  const { link } = props;
  const stale = isStale(link, props.branch);
  return (
    <Tooltip
      label={linkTooltip(link, props.branch, props.now)}
      withArrow={true}
      openDelay={200}
      multiline={true}
      w={300}
      classNames={{ tooltip: "ext-badge-tooltip" }}
    >
      <Button
        variant="light"
        size="compact-sm"
        className={`ext-badge tone-${link.tone}${stale ? " wt-link-stale" : ""}`}
        leftSection={paneIcon(linkGlyph(link), 12)}
        onClick={(e) => props.onOpen(link, e)}
      >
        {link.label}
      </Button>
    </Tooltip>
  );
}

function OverflowMenu(props: {
  groups: LinkGroup[];
  branch: string;
  onOpen: (l: NormalLink, click: OpenClick) => void;
}) {
  const count = overflowCount(props.groups);
  return (
    <Menu position="bottom-start" withinPortal={true} shadow="md">
      <Menu.Target>
        <Button
          variant="light"
          size="compact-sm"
          className="ext-badge tone-neutral"
          aria-label={`${count} more links`}
        >
          +{count}
        </Button>
      </Menu.Target>
      <Menu.Dropdown>
        {props.groups.map((g) => (
          <React.Fragment key={g.kind}>
            <Menu.Label>{kindTitle(g.kind)}</Menu.Label>
            <LinkItems links={g.links} branch={props.branch} onOpen={props.onOpen} />
          </React.Fragment>
        ))}
      </Menu.Dropdown>
    </Menu>
  );
}

const KIND_TITLE: Record<LinkKind, string> = {
  pr: "Pull requests",
  preview: "Previews",
  issue: "Issues",
  doc: "Docs",
  link: "Links",
};
const kindTitle = (kind: LinkKind) => KIND_TITLE[kind];

function LinkItems(props: {
  links: NormalLink[];
  branch: string;
  onOpen: (l: NormalLink, click: OpenClick) => void;
}) {
  return (
    <>
      {props.links.map((l) => (
        <Menu.Item
          key={l.id}
          className={isStale(l, props.branch) ? "wt-link-stale" : undefined}
          leftSection={
            <span className={`wt-link-icon tone-${l.tone}`}>{paneIcon(linkGlyph(l), 14)}</span>
          }
          onClick={(e) => props.onOpen(l, e)}
        >
          <Text size="sm" lh={1.3}>
            {l.label}
          </Text>
          <Text size="xs" c="dimmed" lh={1.3}>
            {linkHostPath(l.url)}
          </Text>
        </Menu.Item>
      ))}
    </>
  );
}

/**
 * The pull-request marker on a rail row. It sits inside a row that selects on
 * click and starts a drag on pointer-down, so both are stopped here.
 */
export function RailPrLinks(props: {
  links: WorktreeLink[];
  branch: string;
  name: string;
  worktreeId: number;
  /** Set only for the worktree on screen — a pane opens in the layout being shown. */
  onOpenInPane?: (url: string) => void;
}) {
  const { prs, tone } = railPrs(props.links, props.branch);
  if (prs.length === 0) return null;
  const glyph = prs.length === 1 ? linkGlyph(prs[0]) : kindGlyph("pr");
  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  const button = (
    <button
      type="button"
      className={`wt-links tone-${tone}`}
      title={prs.map((l) => l.label).join(", ")}
      aria-label={
        prs.length === 1
          ? `Open pull request ${prs[0].label} for ${props.name}`
          : `${prs.length} pull requests for ${props.name}`
      }
      onPointerDown={stop}
      onClick={(e) => {
        e.stopPropagation();
        if (prs.length === 1) void openLink(prs[0].url, props.worktreeId, e, props.onOpenInPane);
      }}
    >
      {paneIcon(glyph, 11)}
      {prs.length > 1 && prs.length}
    </button>
  );
  if (prs.length === 1) return button;
  return (
    <Menu position="bottom-start" withinPortal={true} shadow="md">
      <Menu.Target>{button}</Menu.Target>
      <Menu.Dropdown onClick={stop} onPointerDown={stop}>
        <LinkItems links={prs} branch={props.branch} onOpen={(l, click) => void openLink(l.url, props.worktreeId, click, props.onOpenInPane)} />
      </Menu.Dropdown>
    </Menu>
  );
}
