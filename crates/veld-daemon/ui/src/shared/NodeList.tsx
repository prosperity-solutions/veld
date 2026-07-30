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

import { ActionIcon, Button, Text, Tooltip } from "@mantine/core";
import { IconCheck, IconCopy, IconExternalLink, IconWorld } from "@tabler/icons-react";
import { useState } from "react";
import {
  api,
  type ActionInfo,
  type HistoryEntry,
  type NodeStats,
  type RunInfo,
  type RunRef,
} from "../api";
import { useCopyFlash } from "./copy";
import { notifyError } from "./notify";
import { bucketColor, fmtBytes, shortUrl, statusBucket } from "./util";

/** One card's worth of node state, live or historical. */
export interface NodeRow {
  name: string;
  variant: string;
  status: string;
  url: string | null;
  pid: number | null;
  actions: ActionInfo[];
  recovery_count: number;
  consecutive_failures: number;
  last_liveness_error: string | null;
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

/** Memory, CPU and the memory trend. */
function NodeStatsLine(props: { stats?: NodeStats }) {
  const s = props.stats;
  if (!s) {
    return (
      <span className="node-stats">
        <Text size="xs" c="dimmed">
          no stats yet
        </Text>
      </span>
    );
  }
  return (
    <span className="node-stats">
      <Tooltip label="Resident memory (whole process tree)">
        <Text size="xs" ff="monospace" aria-label={`Memory ${fmtBytes(s.mem)}`}>
          {fmtBytes(s.mem)}
        </Text>
      </Tooltip>
      <Tooltip label="CPU, % of one core (whole process tree)">
        <Text size="xs" ff="monospace" c="dimmed" aria-label={`CPU ${Math.round(s.cpu)} percent`}>
          {Math.round(s.cpu)}%
        </Text>
      </Tooltip>
      <Spark points={s.spark} />
    </span>
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
  canAct: boolean;
  onChanged: () => void;
  /** Open this node's URL in a browser pane. Absent where there are no panes. */
  onOpenPane?: (name: string, url: string) => void;
}) {
  const n = props.node;
  const [busy, setBusy] = useState<string | null>(null);
  const { flash, copy } = useCopyFlash();
  const note = healthNote(n);
  const bucket = statusBucket(n.status);

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
        <NodeStatsLine stats={props.stats} />
      </div>

      {n.url && (
        <div className="node-url-row">
          <a href={n.url} target="_blank" rel="noreferrer" className="node-url" title={n.url}>
            {shortUrl(n.url)}
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
                aria-label={`Open ${n.name} in a browser pane`}
                onClick={() => props.onOpenPane?.(n.name, n.url!)}
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
              aria-label={`Copy the URL for ${n.name}`}
              onClick={() => copy(n.url!, "url")}
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
              href={n.url}
              target="_blank"
              rel="noreferrer"
              aria-label={`Open ${n.name} in the system browser`}
            >
              <IconExternalLink size={13} />
            </ActionIcon>
          </Tooltip>
        </div>
      )}

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
          canAct={props.canAct}
          onChanged={props.onChanged}
          onOpenPane={props.onOpenPane}
        />
      ))}
    </ul>
  );
}
