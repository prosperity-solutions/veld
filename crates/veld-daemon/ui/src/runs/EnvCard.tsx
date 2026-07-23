import { useState } from "react";
import {
  Badge,
  Button,
  Checkbox,
  Group,
  NativeSelect,
  SegmentedControl,
  Table,
  Text,
  Tooltip,
} from "@mantine/core";
import {
  api,
  type HistoryEntry,
  type NodeStats,
  type ProjectInfo,
  type RunInfo,
  type ShareInfo,
} from "../api";
import { LogsPanel } from "./LogsPanel";
import { bucketColor, fmtBytes, fmtWhen, shortUrl, statusBucket } from "./util";

function badgeColor(status: string): string {
  switch (statusBucket(status)) {
    case "green":
      return "green";
    case "red":
      return "red";
    case "yellow":
      return "yellow";
    case "dim":
      return "gray";
  }
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

function ConnBadges(props: { share: ShareInfo }) {
  return (
    <>
      {props.share.connections.map((c, i) => {
        const who = c.label || c.node_id.slice(0, 10);
        const rtt = c.rtt_ms != null ? ` ${c.rtt_ms}ms` : "";
        if (c.transport === "direct") {
          return (
            <Badge key={i} size="xs" color="green" variant="light">
              {who}: direct{rtt}
            </Badge>
          );
        }
        if (c.transport === "relayed") {
          return (
            <Tooltip key={i} label="Throughput is limited by the relay">
              <Badge size="xs" color="yellow" variant="light">
                {who}: relayed via {c.via ?? "?"}
                {rtt}
              </Badge>
            </Tooltip>
          );
        }
        return (
          <Badge key={i} size="xs" color="gray" variant="light">
            {who}: no open path
          </Badge>
        );
      })}
    </>
  );
}

/** One environment card: head, toolbar, share strips, Services|Logs tabs. */
export function EnvCard(props: {
  project: ProjectInfo;
  run: RunInfo;
  shares: ShareInfo[];
  stats?: Record<string, NodeStats>;
  onChanged: () => void;
}) {
  const { project, run } = props;
  const [tab, setTab] = useState<string>("services");
  const [logsEverOpened, setLogsEverOpened] = useState(false);
  const [histSel, setHistSel] = useState<string>("");
  const [busy, setBusy] = useState<string | null>(null);
  const [flash, setFlash] = useState<string | null>(null);

  const history: HistoryEntry[] = run.history ?? [];
  const selected = history.find((h) => h.run_id === histSel) ?? null;
  const shownStatus = selected?.status ?? run.status;
  const shownOutcome = selected ? (selected.outcome ?? selected.status) : run.outcome;
  const shownEndedAt = selected?.ended_at ?? run.ended_at;

  const runShares = props.shares.filter((s) => s.run === run.name);
  const peerShare = runShares.find((s) => s.public_urls.length === 0) ?? null;
  const webShares = runShares.filter((s) => s.public_urls.length > 0);

  const act = async (label: string, fn: () => Promise<unknown>) => {
    setBusy(label);
    try {
      await fn();
      props.onChanged();
    } catch (e) {
      window.alert(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };
  const copy = (text: string, tag: string) => {
    void navigator.clipboard.writeText(text);
    setFlash(tag);
    window.setTimeout(() => setFlash(null), 1500);
  };

  const nodes = selected
    ? selected.nodes.map((n) => ({
        name: n.name,
        variant: n.variant,
        status: n.status,
        url: null as string | null,
        pid: null as number | null,
        actions: [] as { name: string; label: string }[],
        recovery_count: 0,
        consecutive_failures: 0,
        last_liveness_error: null as string | null,
      }))
    : run.nodes.map((n) => ({
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

  return (
    <div className="env-card">
      <Group gap="sm" px={12} pt={10} wrap="wrap">
        <Text ff="monospace" fw={700} size="sm">
          {run.name}
        </Text>
        <Text size="xs" c="dimmed">
          {project.name}
        </Text>
        <Text size="xs" c="dimmed" className="path-ellipsis" title={project.project_root}>
          {project.project_root}
        </Text>
        <div style={{ flex: 1 }} />
        {!run.live && history.length > 0 && (
          <NativeSelect
            size="xs"
            value={histSel}
            onChange={(e) => setHistSel(e.currentTarget.value)}
            data={[
              {
                value: "",
                label: `${run.outcome || run.status} · ${fmtWhen(run.ended_at)}`,
              },
              ...history.map((h) => ({
                value: h.run_id,
                label: `${h.outcome || h.status} · ${fmtWhen(h.created_at)}`,
              })),
            ]}
          />
        )}
        <Badge size="sm" variant="light" color={badgeColor(shownStatus)}>
          {shownStatus}
        </Badge>
      </Group>

      {!run.live && shownOutcome && (
        <Text
          size="xs"
          px={12}
          pt={4}
          c={statusBucket(shownStatus) === "red" ? "red" : "dimmed"}
        >
          {shownOutcome}
          {shownEndedAt ? ` · ended ${fmtWhen(shownEndedAt)}` : ""}
        </Text>
      )}

      <Group gap={6} px={12} py={8} wrap="wrap">
        <Button
          size="compact-xs"
          variant="default"
          loading={busy === "restart"}
          onClick={() => void act("restart", () => api.restartRun(run.name))}
        >
          Restart
        </Button>
        {run.live && (
          <Button
            size="compact-xs"
            variant="default"
            loading={busy === "stop"}
            onClick={() => {
              if (window.confirm(`Stop environment "${run.name}"?`)) {
                void act("stop", () => api.stopRun(run.name));
              }
            }}
          >
            Stop
          </Button>
        )}
        <Button
          size="compact-xs"
          variant="default"
          onClick={() => void act("terminal", () => api.openTerminal(project.project_root))}
        >
          Terminal
        </Button>
        <Button
          size="compact-xs"
          variant="default"
          onClick={() => copy(project.project_root, "path")}
        >
          {flash === "path" ? "Copied" : "Copy path"}
        </Button>
        {run.status === "running" && !peerShare && (
          <Button
            size="compact-xs"
            variant="light"
            loading={busy === "share"}
            onClick={() =>
              void act("share", async () => {
                const r = await api.startShare(run.name);
                if (r?.join_url) copy(r.join_url, "join");
              })
            }
          >
            Share
          </Button>
        )}
        {peerShare && (
          <Button
            size="compact-xs"
            color="red"
            variant="light"
            loading={busy === "stop-share"}
            onClick={() => void act("stop-share", () => api.stopShare(peerShare.id))}
          >
            Stop sharing
          </Button>
        )}
        {run.status === "running" && webShares.length === 0 && (
          <Button
            size="compact-xs"
            variant="light"
            loading={busy === "web-share"}
            onClick={() => void act("web-share", () => api.startShare(run.name, { web: true }))}
          >
            Share to web
          </Button>
        )}
      </Group>

      {peerShare && (
        <Group gap={6} px={12} pb={6} wrap="wrap" className="share-strip">
          <span className="dot running" style={{ animation: "none" }} />
          <Text size="xs">Sharing</Text>
          {peerShare.joiners > 0 && (
            <Text size="xs" c="dimmed">
              · <b>{peerShare.joiners}</b> connected
            </Text>
          )}
          {peerShare.join_url && (
            <Button size="compact-xs" variant="default" onClick={() => copy(peerShare.join_url!, "join")}>
              {flash === "join" ? "Link copied!" : "Copy link"}
            </Button>
          )}
          {peerShare.ticket && (
            <Button
              size="compact-xs"
              variant="default"
              onClick={() => copy(`veld join ${peerShare.ticket}`, "join-cmd")}
            >
              {flash === "join-cmd" ? "Copied" : "Copy command"}
            </Button>
          )}
          <Checkbox
            size="xs"
            label="auto-accept"
            checked={peerShare.approve === "auto"}
            onChange={(e) =>
              void act("mode", () =>
                api.setShareMode(peerShare.id, e.currentTarget.checked ? "auto" : "manual"),
              )
            }
          />
          <ConnBadges share={peerShare} />
        </Group>
      )}
      {webShares.map((w) => (
        <Group key={w.id} gap={6} px={12} pb={6} wrap="wrap" className="share-strip">
          <span className="dot running" style={{ animation: "none" }} />
          <Text size="xs">Public web</Text>
          {w.public_urls.map((u) => {
            const withPassword = !!w.web_password && u.access !== "link";
            const link = withPassword
              ? `${u.public_url}/#veld-key=${encodeURIComponent(w.web_password!)}`
              : u.public_url;
            const tag = `web-${w.id}-${u.node}`;
            return (
              <Button key={u.node} size="compact-xs" variant="default" onClick={() => copy(link, tag)}>
                {flash === tag ? "Copied" : `${u.node} ${withPassword ? "link (with password)" : "URL"}`}
              </Button>
            );
          })}
          {w.web_password && (
            <Button
              size="compact-xs"
              variant="default"
              onClick={() => copy(w.web_password!, `pw-${w.id}`)}
            >
              {flash === `pw-${w.id}` ? "Copied" : "Copy password"}
            </Button>
          )}
          <Button
            size="compact-xs"
            color="red"
            variant="light"
            onClick={() => void act("stop-share", () => api.stopShare(w.id))}
          >
            Stop web
          </Button>
          <ConnBadges share={w} />
        </Group>
      ))}

      <SegmentedControl
        size="xs"
        ml={12}
        mb={6}
        value={tab}
        onChange={(v) => {
          setTab(v);
          if (v === "logs") setLogsEverOpened(true);
        }}
        data={[
          { value: "services", label: "Services" },
          { value: "logs", label: "Logs" },
        ]}
      />

      {tab === "services" && (
        <Table
          withRowBorders={false}
          verticalSpacing={4}
          horizontalSpacing="sm"
          className="node-table"
        >
          <Table.Tbody>
            {nodes.length === 0 && (
              <Table.Tr>
                <Table.Td>
                  <Text size="xs" c="dimmed">
                    No services
                  </Text>
                </Table.Td>
              </Table.Tr>
            )}
            {nodes.map((n) => (
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
                    {run.status === "running" &&
                      n.actions.map((a) => (
                        <Button
                          key={a.name}
                          size="compact-xs"
                          variant="subtle"
                          loading={busy === `act-${a.name}-${n.name}`}
                          onClick={() =>
                            void act(`act-${a.name}-${n.name}`, () =>
                              api.runAction(run.name, a.name, n.name),
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
      )}

      {logsEverOpened && (
        <LogsPanel
          run={run.name}
          history={history}
          histSel={histSel || null}
          visible={tab === "logs"}
        />
      )}
    </div>
  );
}
