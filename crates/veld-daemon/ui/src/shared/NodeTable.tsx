/**
 * Per-node health, URLs, actions and resource usage — one table, every host.
 *
 * Rendered by runs mode's environment card and by IDE mode's `nodes` pane through
 * the same [`NodesView`](./RunViews.tsx) wrapper. Extracted rather than forked: the
 * pair would have drifted on the first change to what a node row says, and the
 * health sub-line (failures / recoveries / last liveness error) is the part a
 * second implementation reliably forgets.
 *
 * **Column widths are declared, not left to the browser.** A plain auto-layout
 * table spreads six short cells across a wide pane, which reads as six unrelated
 * things floating in a row; the URL column absorbs the slack instead. And the table
 * is a **container query** context, because the same table lives in a 400px pane and
 * in a 1080px card: variant, PID and the sparkline drop out as it narrows, and what
 * they said moves into a sub-line under the node's name so nothing is merely lost.
 */

import { Button, Group, Table, Text, Tooltip } from "@mantine/core";
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
import { useState } from "react";

/** One row's worth of node state, live or historical. */
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
  const w = 64;
  const h = 16;
  const step = w / (pts.length - 1);
  const line = pts
    .map((v, i) => `${(i * step).toFixed(1)},${(h - 2 - ((v - min) / range) * (h - 4)).toFixed(1)}`)
    .join(" ");
  return (
    <svg width={w} height={h} className="spark">
      <polyline points={line} fill="none" stroke="var(--accent)" strokeWidth="1.2" />
    </svg>
  );
}

function StatCell(props: { stats?: NodeStats }) {
  const s = props.stats;
  if (!s) {
    return (
      <Text size="xs" c="dimmed">
        –
      </Text>
    );
  }
  return (
    <Group gap={8} wrap="nowrap">
      <Tooltip label="Resident memory (whole process tree)">
        <Text size="xs" ff="monospace">
          {fmtBytes(s.mem)}
        </Text>
      </Tooltip>
      <Tooltip label="CPU, % of one core (whole process tree)">
        <Text size="xs" ff="monospace" c="dimmed">
          {Math.round(s.cpu)}%
        </Text>
      </Tooltip>
      <Spark points={s.spark} />
    </Group>
  );
}

/** The health line under a node's name, when there is anything to say. */
function healthNote(n: NodeRow): string | null {
  const parts = [
    n.consecutive_failures > 0 ? `failures: ${n.consecutive_failures}` : null,
    n.recovery_count > 0 ? `recoveries: ${n.recovery_count}` : null,
    n.last_liveness_error,
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(" · ") : null;
}

export function NodeTable(props: {
  /** Project-scoped run address for node actions. */
  run: RunRef;
  nodes: NodeRow[];
  /** Stats for this run, keyed `node:variant`. */
  stats?: Record<string, NodeStats>;
  /** Whether node actions may fire — a stopped run has nothing to act on. */
  canAct: boolean;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const { flash, copy } = useCopyFlash();

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
    <div className="node-table-wrap">
      <Table
        withRowBorders={false}
        verticalSpacing={4}
        horizontalSpacing="sm"
        className="node-table"
      >
        <Table.Thead>
          <Table.Tr>
            <Table.Th className="col-node">Node</Table.Th>
            <Table.Th className="col-url">URL</Table.Th>
            {/* The actions column's header would name three different things
                depending on the row (Copy, Open, a node's own actions), so it
                stays blank rather than lying. */}
            <Table.Th className="col-actions" aria-label="Actions" />
            <Table.Th className="col-variant">Variant</Table.Th>
            <Table.Th className="col-pid">PID</Table.Th>
            <Table.Th className="col-res">Memory · CPU</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {props.nodes.length === 0 && (
            <Table.Tr>
              <Table.Td colSpan={6}>
                <Text size="xs" c="dimmed">
                  No services
                </Text>
              </Table.Td>
            </Table.Tr>
          )}
          {props.nodes.map((n) => {
            const note = healthNote(n);
            return (
              <Table.Tr key={`${n.name}:${n.variant}`}>
                <Table.Td className="col-node">
                  <Group gap={6} wrap="nowrap">
                    <span
                      className="dot"
                      style={{
                        background: bucketColor(statusBucket(n.status)),
                        animation: "none",
                      }}
                      title={n.status}
                    />
                    <Text size="xs" ff="monospace" fw={600}>
                      {n.name}
                    </Text>
                  </Group>
                  {/* What the dropped columns said, for a narrow pane. Each is
                      hidden while its own column is on screen, so one fact is
                      never shown twice — see the container queries in styles.css. */}
                  <Text size="xs" pl={13} c="dimmed" className="node-compact-meta">
                    {[n.variant, n.pid != null ? `pid ${n.pid}` : null]
                      .filter(Boolean)
                      .join(" · ")}
                  </Text>
                  {n.url && (
                    <Text size="xs" pl={13} className="node-compact-url">
                      <a href={n.url} target="_blank" rel="noreferrer" className="node-url">
                        {shortUrl(n.url)}
                      </a>
                    </Text>
                  )}
                  {note && (
                    <Text size="xs" pl={13} c="dimmed">
                      {note}
                    </Text>
                  )}
                </Table.Td>
                <Table.Td className="col-url">
                  {n.url ? (
                    <a href={n.url} target="_blank" rel="noreferrer" className="node-url">
                      {shortUrl(n.url)}
                    </a>
                  ) : (
                    <Text size="xs" c="dimmed">
                      –
                    </Text>
                  )}
                </Table.Td>
                <Table.Td className="col-actions">
                  <Group gap={4} wrap="nowrap">
                    {n.url && (
                      <>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          onClick={() => copy(n.url!, `url-${n.name}`)}
                        >
                          {flash === `url-${n.name}` ? "Copied" : "Copy"}
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          onClick={() => window.open(n.url!, "_blank")}
                        >
                          Open
                        </Button>
                      </>
                    )}
                    {props.canAct &&
                      n.actions.map((a) => (
                        <Button
                          key={a.name}
                          size="compact-xs"
                          variant="subtle"
                          loading={busy === `act-${a.name}-${n.name}`}
                          onClick={() =>
                            void act(
                              `act-${a.name}-${n.name}`,
                              `${a.label} on ${n.name}`,
                              () => api.runAction(props.run, a.name, n.name),
                            )
                          }
                        >
                          {a.label}
                        </Button>
                      ))}
                  </Group>
                </Table.Td>
                <Table.Td className="col-variant">
                  <Text size="xs" c="dimmed">
                    {n.variant}
                  </Text>
                </Table.Td>
                <Table.Td className="col-pid">
                  <Text size="xs" c="dimmed" ff="monospace">
                    {n.pid ?? ""}
                  </Text>
                </Table.Td>
                <Table.Td className="col-res">
                  <StatCell stats={props.stats?.[`${n.name}:${n.variant}`]} />
                </Table.Td>
              </Table.Tr>
            );
          })}
        </Table.Tbody>
      </Table>
    </div>
  );
}
