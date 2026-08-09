/**
 * The run's nodes — health, URL, resources, actions — as a list of cards.
 *
 * **Not a table.** A table commits to columns, and this view has to be legible in
 * a 300px pane *and* in a 1080px card. Both honest ways to keep a table are bad:
 * let six columns squeeze until every cell is two characters wide, or drop columns
 * as the width shrinks — which loses the pid exactly when someone needed it. A
 * card per node has no columns to lose. Every fact keeps its place, the values that
 * are long (a URL, a liveness error) get a whole line to themselves, and width only
 * decides where lines wrap.
 *
 * The card is ordered by how often each part is read: identity and state, then the
 * URL with its actions, then whatever is wrong, then what you can do about it.
 * Resources sit on the opposite edge of the first line — they are the one thing you
 * scan *down* a list for, so they stay on the same edge in every card.
 *
 * Because there is no header row to hang meaning on, units and labels travel with
 * the values (`pid 21672`, an `aria-label` of "Memory 212 MB").
 *
 * Rendered by runs mode's environment card and by IDE mode's `nodes` pane through
 * the same [`NodesView`](./RunViews.tsx). Extracted rather than forked: the pair
 * would have drifted on the first change to what a node says, and the health line
 * (failures / recoveries / last liveness error) is the part a second implementation
 * reliably forgets.
 */

import { ActionIcon, Button, Text, Tooltip, UnstyledButton } from "@mantine/core";
import {
  IconCheck,
  IconChevronDown,
  IconChevronRight,
  IconCopy,
  IconExternalLink,
  IconWorld,
} from "@tabler/icons-react";
import { useState } from "react";
import {
  api,
  type ActionInfo,
  type EndpointInfo,
  type HistoryEntry,
  type NodeStats,
  type RunInfo,
  type RunRef,
} from "../api";
import { useCopyFlash } from "./copy";
import { notifyError } from "./notify";
import { ResourcePanel, fmtCpuTime } from "./ResourcePanel";
import { bucketColor, fmtBytes, shortUrl, statusBucket } from "./util";

/** One card's worth of node state, live or historical. */
export interface NodeRow {
  name: string;
  variant: string;
  status: string;
  url: string | null;
  /** Every port the node claimed, primary first. See {@link nodeEndpoints}. */
  endpoints: EndpointInfo[];
  pid: number | null;
  actions: ActionInfo[];
  recovery_count: number;
  consecutive_failures: number;
  last_liveness_error: string | null;
}

/**
 * The endpoints to render for a node.
 *
 * A node that declared several ports has them all; a node from a daemon or a run
 * that predates per-port endpoints has only `url`, which is synthesised into the
 * single primary entry it has always been. The synthesised entry carries no port
 * number because none was recorded — and inventing one to fill the shape would
 * put a wrong number on screen, which is worse than an absent one.
 */
export function nodeEndpoints(n: NodeRow): EndpointInfo[] {
  if (n.endpoints.length > 0) return n.endpoints;
  if (!n.url) return [];
  // `hostname` is the bare host, as it is on a real endpoint — the daemon's
  // `endpoints_or_legacy` strips the scheme for the same synthesised entry.
  // Latent today (this entry always has a `url`, so nothing reads its hostname)
  // and worth keeping true anyway: the day something does, a scheme in there
  // renders as `https://web.localhost:3000`.
  let hostname = n.url;
  try {
    hostname = new URL(n.url).hostname;
  } catch {
    // Not parseable as a URL — keep it verbatim rather than lose the value.
  }
  return [{ name: "http", hostname, url: n.url, port: 0, primary: true }];
}

/**
 * The rows for a run, optionally viewed through one of its history entries.
 *
 * A historical run keeps its node statuses but has no live URL, pid or actions —
 * the daemon strips the URLs server-side, and offering an action on an ended run
 * would spawn it against whatever is current. So those are nulled here rather
 * than rendered as stale truth.
 */
export function nodeRows(run: RunInfo, selected: HistoryEntry | null): NodeRow[] {
  if (selected) {
    return selected.nodes.map((n) => ({
      name: n.name,
      variant: n.variant,
      status: n.status,
      url: null,
      endpoints: [],
      pid: null,
      actions: [],
      recovery_count: 0,
      consecutive_failures: 0,
      last_liveness_error: null,
    }));
  }
  return run.nodes.map((n) => ({
    name: n.name,
    variant: n.variant,
    status: n.status,
    url: n.url ?? null,
    endpoints: n.endpoints ?? [],
    pid: n.pid ?? null,
    actions: n.actions ?? [],
    recovery_count: n.recovery_count ?? 0,
    consecutive_failures: n.consecutive_failures ?? 0,
    last_liveness_error: n.last_liveness_error ?? null,
  }));
}

function Spark(props: { points: number[] }) {
  const pts = props.points;
  if (pts.length < 2) return null;
  const min = Math.min(...pts);
  const max = Math.max(...pts);
  const range = max - min || 1;
  const w = 56;
  const h = 14;
  const step = w / (pts.length - 1);
  const line = pts
    .map((v, i) => `${(i * step).toFixed(1)},${(h - 2 - ((v - min) / range) * (h - 4)).toFixed(1)}`)
    .join(" ");
  return (
    <svg width={w} height={h} className="spark" aria-hidden>
      <polyline points={line} fill="none" stroke="var(--accent)" strokeWidth="1.2" />
    </svg>
  );
}

/**
 * Memory, CPU and the memory trend — and the handle that expands the full
 * resource view.
 *
 * The memory figure is the **footprint**, not RSS: summing RSS over a process
 * tree counts every page shared inside it once per process, so a five-process
 * `npm run dev` reported far more than it occupied. The sparkline plots the same
 * quantity, so the collapsed line and the expanded chart never disagree.
 */
function NodeStatsLine(props: { stats?: NodeStats; expanded: boolean; onToggle?: () => void }) {
  const s = props.stats;
  // No live sample. That is not the same as no data: a `command` step — a build,
  // an install — is sampled only while it runs, so the moment it succeeds its
  // live reading goes away and its memory curve is all that is left. Offer the
  // curve whenever the caller says this run can be graphed.
  const body = !s ? (
    <Text size="xs" c="dimmed">
      {props.onToggle ? "resource history" : "no stats yet"}
    </Text>
  ) : (
    <>
      <Tooltip label="Memory footprint of the whole process tree (PSS / phys_footprint). Click for the breakdown.">
        <Text size="xs" ff="monospace" aria-label={`Memory ${fmtBytes(s.footprint)}`}>
          {fmtBytes(s.footprint)}
        </Text>
      </Tooltip>
      <Tooltip
        label={`CPU, % of one core (whole process tree) · ${fmtCpuTime(s.cpu_seconds)} total`}
      >
        <Text size="xs" ff="monospace" c="dimmed" aria-label={`CPU ${Math.round(s.cpu)} percent`}>
          {Math.round(s.cpu)}%
        </Text>
      </Tooltip>
      <Spark points={s.spark} />
    </>
  );
  // Historical runs pass no toggle — there is no live node to graph — so the
  // stats stay plain text rather than becoming a button that does nothing.
  if (!props.onToggle) return <span className="node-stats">{body}</span>;
  return (
    <UnstyledButton
      className="node-stats node-stats-toggle"
      onClick={props.onToggle}
      aria-expanded={props.expanded}
      aria-label={`${props.expanded ? "Hide" : "Show"} resource detail`}
    >
      {body}
      {props.expanded ? <IconChevronDown size={12} /> : <IconChevronRight size={12} />}
    </UnstyledButton>
  );
}

/**
 * One port of a node: what it is reachable at, and what you can do with it.
 *
 * Two shapes, because there are two kinds of reachable. A routed (`http`) port
 * is a link and gets the three launchers. A raw (`tcp`) port is an address —
 * `db.app.localhost:5432` — and gets copy alone: there is no route in front of
 * it, so opening it in a browser reaches nothing, and offering the button anyway
 * would be an invitation to a dead end. The `tcp` badge says which one you are
 * looking at without the reader having to notice the missing scheme.
 *
 * The port name is shown only where it disambiguates. On a single-port node it
 * is noise — `http` under a node called `web` says nothing the card did not
 * already say.
 */
function EndpointRow(props: {
  endpoint: EndpointInfo;
  nodeName: string;
  /** Whether to label the row with its port name. */
  labelled: boolean;
  onOpenPane?: (name: string, url: string) => void;
}) {
  const e = props.endpoint;
  const { flash, copy } = useCopyFlash();
  // A synthesised legacy entry has no recorded port number; `hostname` is the
  // whole address in that case, so there is nothing to append.
  const address = e.port > 0 ? `${e.hostname}:${e.port}` : e.hostname;
  const label = props.labelled ? <span className="node-port-name">{e.name}</span> : null;

  if (!e.url) {
    return (
      <div className="node-url-row">
        {label}
        <Tooltip label="Raw TCP port — reachable at this address, not over HTTP">
          <span className="node-address" title={address}>
            {address}
          </span>
        </Tooltip>
        <span className="node-proto">tcp</span>
        <Tooltip label={flash === "addr" ? "Copied" : "Copy the address"} openDelay={250}>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label={`Copy the ${e.name} address for ${props.nodeName}`}
            onClick={() => copy(address, "addr")}
          >
            {flash === "addr" ? <IconCheck size={13} /> : <IconCopy size={13} />}
          </ActionIcon>
        </Tooltip>
      </div>
    );
  }

  const url = e.url;
  return (
    <div className="node-url-row">
      {label}
      <a href={url} target="_blank" rel="noreferrer" className="node-url" title={url}>
        {shortUrl(url)}
      </a>
      {/* First of the three, because in a window that *has* panes this is the
          one you want: the service opens beside the terminal rather than in
          another application. Absent in runs mode, which has nowhere to put
          it — the same reason the URL launcher takes this as a prop. */}
      {props.onOpenPane && (
        <Tooltip label="Open in a browser pane" openDelay={250}>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label={`Open ${props.nodeName} in a browser pane`}
            onClick={() => props.onOpenPane?.(props.nodeName, url)}
          >
            <IconWorld size={13} />
          </ActionIcon>
        </Tooltip>
      )}
      <Tooltip label={flash === "url" ? "Copied" : "Copy the URL"} openDelay={250}>
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          aria-label={`Copy the URL for ${props.nodeName}`}
          onClick={() => copy(url, "url")}
        >
          {flash === "url" ? <IconCheck size={13} /> : <IconCopy size={13} />}
        </ActionIcon>
      </Tooltip>
      <Tooltip label="Open in the system browser" openDelay={250}>
        <ActionIcon
          size="sm"
          variant="subtle"
          color="gray"
          component="a"
          href={url}
          target="_blank"
          rel="noreferrer"
          aria-label={`Open ${props.nodeName} in the system browser`}
        >
          <IconExternalLink size={13} />
        </ActionIcon>
      </Tooltip>
    </div>
  );
}

/** What is wrong with this node, if anything. */
function healthNote(n: NodeRow): string | null {
  // Label-colon form, as the old table had it: count-agnostic, so a single
  // failure does not read as "1 consecutive failures".
  const parts = [
    n.consecutive_failures > 0 ? `failures: ${n.consecutive_failures}` : null,
    n.recovery_count > 0 ? `recoveries: ${n.recovery_count}` : null,
    n.last_liveness_error,
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(" · ") : null;
}

function NodeCard(props: {
  node: NodeRow;
  run: RunRef;
  stats?: NodeStats;
  /** Whether this run's recorded resource history can be charted at all. */
  graphable: boolean;
  canAct: boolean;
  onChanged: () => void;
  /** Open this node's URL in a browser pane. Absent where there are no panes. */
  onOpenPane?: (name: string, url: string) => void;
}) {
  const n = props.node;
  const [busy, setBusy] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const endpoints = nodeEndpoints(n);
  const note = healthNote(n);
  const bucket = statusBucket(n.status);
  // Gating the chart on a *live* sample meant a build's curve became unreachable
  // the instant the build finished, which is the one moment you want it. Gating
  // instead on node status was wrong in both directions: a `command` node's row
  // stays `pending` until its whole parallel stage is saved (only `start_server`
  // nodes checkpoint on spawn), so a fast build in a slow stage still hid its
  // chart; and a step too short to sample offers one with nothing in it either
  // way. So the run is the only question asked here, and the panel says for
  // itself when a window holds no samples.
  const canGraph = props.graphable;

  const act = async (label: string, context: string, fn: () => Promise<unknown>) => {
    setBusy(label);
    try {
      await fn();
      props.onChanged();
    } catch (e) {
      notifyError(context, e);
    } finally {
      setBusy(null);
    }
  };

  return (
    <li className="node-card">
      <div className="node-head">
        <span
          className="dot"
          style={{ background: bucketColor(bucket), animation: "none" }}
          aria-hidden
        />
        {/* `title`, because the card ellipsises a long name where the old table
            cell just widened — a truncated name with no way to read it is a fact
            lost, not a fact compressed. */}
        <span className="node-name" title={n.name}>
          {n.name}
        </span>
        {/* The variant qualifies the name (`web:local`), so it travels with it
            rather than living in a field of its own. */}
        <span className="node-variant">{n.variant}</span>
        <span className={`node-status ${bucket}`}>{n.status}</span>
        {n.pid != null && (
          <Tooltip label="Process id of the node's root process">
            <span className="node-pid">pid {n.pid}</span>
          </Tooltip>
        )}
        {/* Holds the stats against the opposite edge at any width — and lets them
            wrap to their own line instead of squeezing the name when there is no
            room for both. */}
        <span className="node-head-gap" />
        <NodeStatsLine
          stats={props.stats}
          expanded={expanded}
          onToggle={canGraph ? () => setExpanded((e) => !e) : undefined}
        />
      </div>

      {expanded && canGraph && <ResourcePanel run={props.run} nodeKey={`${n.name}:${n.variant}`} />}

      {endpoints.map((e) => (
        <EndpointRow
          key={e.name}
          endpoint={e}
          nodeName={n.name}
          labelled={endpoints.length > 1}
          onOpenPane={props.onOpenPane}
        />
      ))}

      {note && <div className={`node-health${bucket === "red" ? " bad" : ""}`}>{note}</div>}

      {props.canAct && n.actions.length > 0 && (
        <div className="node-actions">
          {n.actions.map((a) => (
            <Button
              key={a.name}
              size="compact-xs"
              variant="default"
              loading={busy === a.name}
              onClick={() =>
                void act(a.name, `${a.label} on ${n.name}`, () =>
                  api.runAction(props.run, a.name, n.name),
                )
              }
            >
              {a.label}
            </Button>
          ))}
        </div>
      )}
    </li>
  );
}

export function NodeList(props: {
  /** Project-scoped run address for node actions. */
  run: RunRef;
  nodes: NodeRow[];
  /** Stats for this run, keyed `node:variant`. */
  stats?: Record<string, NodeStats>;
  /**
   * Whether this run's recorded resource history can be charted.
   *
   * Separate from `stats` because "no live sample" and "nothing to show" are
   * different: a finished build stops being sampled but keeps its curve, so the
   * chart has to stay reachable after the live reading is gone.
   */
  graphable?: boolean;
  /** Whether node actions may fire — a stopped run has nothing to act on. */
  canAct: boolean;
  onChanged: () => void;
  /** Open a node's URL in a browser pane, where the host has panes. */
  onOpenPane?: (name: string, url: string) => void;
}) {
  if (props.nodes.length === 0) {
    return (
      <div className="node-list">
        <Text size="xs" c="dimmed">
          No services in this run.
        </Text>
      </div>
    );
  }
  return (
    <ul className="node-list">
      {props.nodes.map((n) => (
        <NodeCard
          key={`${n.name}:${n.variant}`}
          node={n}
          run={props.run}
          stats={props.stats?.[`${n.name}:${n.variant}`]}
          graphable={props.graphable ?? false}
          canAct={props.canAct}
          onChanged={props.onChanged}
          onOpenPane={props.onOpenPane}
        />
      ))}
    </ul>
  );
}
