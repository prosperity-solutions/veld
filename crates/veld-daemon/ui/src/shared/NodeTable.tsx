/**
 * Per-node health, URLs, actions and resource usage — one table, two hosts.
 *
 * Runs mode shows it as an environment card's Services tab; IDE mode shows it as
 * a `nodes` pane scoped to the selected worktree's run. Extracted rather than
 * forked: the pair would have drifted on the first change to what a node row
 * says, and the health sub-line (failures / recoveries / last liveness error) is
 * the part a second implementation reliably forgets.
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

export function NodeTable(props: {
  /** Project-scoped run address for node actions. */
  run: RunRef;
  nodes: NodeRow[];
  /** Stats for this run, keyed `node:variant`. */
  stats?: Record<string, NodeStats>;
  /** Whether node actions may fire — a stopped run has nothing to act on. */
  canAct: boolean;
  onChanged: () => void;
  /** How the host reports a failed action (an alert, a banner…). */
  onError: (message: string) => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const { flash, copy } = useCopyFlash();

  const act = async (label: string, fn: () => Promise<unknown>) => {
    setBusy(label);
    try {
      await fn();
      props.onChanged();
    } catch (e) {
      props.onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Table
      withRowBorders={false}
      verticalSpacing={4}
      horizontalSpacing="sm"
      className="node-table"
    >
      <Table.Tbody>
        {props.nodes.length === 0 && (
          <Table.Tr>
            <Table.Td>
              <Text size="xs" c="dimmed">
                No services
              </Text>
            </Table.Td>
          </Table.Tr>
        )}
        {props.nodes.map((n) => (
          <Table.Tr key={`${n.name}:${n.variant}`}>
            <Table.Td>
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
              {(n.recovery_count > 0 ||
                n.consecutive_failures > 0 ||
                n.last_liveness_error) && (
                <Text size="xs" pl={13} c="dimmed">
                  {[
                    n.consecutive_failures > 0 ? `failures: ${n.consecutive_failures}` : null,
                    n.recovery_count > 0 ? `recoveries: ${n.recovery_count}` : null,
                    n.last_liveness_error,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </Text>
              )}
            </Table.Td>
            <Table.Td>
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
            <Table.Td>
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
                        void act(`act-${a.name}-${n.name}`, () =>
                          api.runAction(props.run, a.name, n.name),
                        )
                      }
                    >
                      {a.label}
                    </Button>
                  ))}
              </Group>
            </Table.Td>
            <Table.Td>
              <Text size="xs" c="dimmed">
                {n.variant}
              </Text>
            </Table.Td>
            <Table.Td>
              <Text size="xs" c="dimmed" ff="monospace">
                {n.pid ?? ""}
              </Text>
            </Table.Td>
            <Table.Td>
              <StatCell stats={props.stats?.[`${n.name}:${n.variant}`]} />
            </Table.Td>
          </Table.Tr>
        ))}
      </Table.Tbody>
    </Table>
  );
}
