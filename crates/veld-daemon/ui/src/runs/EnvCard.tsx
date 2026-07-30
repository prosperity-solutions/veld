import { useState } from "react";
import {
  Badge,
  Button,
  Group,
  NativeSelect,
  SegmentedControl,
  Text,
} from "@mantine/core";
import {
  api,
  runRef,
  type HistoryEntry,
  type NodeStats,
  type ProjectInfo,
  type RunInfo,
  type ShareInfo,
} from "../api";
import { LogsPanel } from "../shared/LogsPanel";
import { NodeTable, nodeRows } from "../shared/NodeTable";
import {
  PeerShareStrip,
  ShareControls,
  WebShareStrip,
  sharesForRun,
} from "../shared/Sharing";
import { useCopyFlash } from "../shared/copy";
import { fmtWhen, statusBucket } from "../shared/util";

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

/** One environment card: head, toolbar, share strips, Services|Logs tabs. */
export function EnvCard(props: {
  project: ProjectInfo;
  run: RunInfo;
  shares: ShareInfo[];
  stats?: Record<string, NodeStats>;
  onChanged: () => void;
}) {
  const { project, run } = props;
  // Every run-addressed call goes through this — the card's own project scope,
  // so a same-named run in another repo can never be the one acted on.
  const ref = runRef(project.project_root, run);
  const [tab, setTab] = useState<string>("services");
  const [logsEverOpened, setLogsEverOpened] = useState(false);
  const [histSel, setHistSel] = useState<string>("");
  const [busy, setBusy] = useState<string | null>(null);
  const { flash, copy } = useCopyFlash();

  const history: HistoryEntry[] = run.history ?? [];
  // History selection only applies to ended runs — guard against a future
  // always-mounted card showing a stale historical run for a live env.
  const selected = run.live
    ? null
    : (history.find((h) => h.run_id === histSel) ?? null);
  const shownStatus = selected?.status ?? run.status;
  const shownOutcome = selected ? (selected.outcome ?? selected.status) : run.outcome;
  const shownEndedAt = selected?.ended_at ?? run.ended_at;

  const { peer: peerShare, web: webShares } = sharesForRun(props.shares, run.run_id);

  // Runs mode reports a failed action where the click happened: an alert. (IDE
  // mode has a banner for the same job, which is why the shared components take
  // the reporter as a prop.) Wrapped rather than passed as `window.alert`, which
  // is only callable with `window` as its receiver.
  const onError = (message: string) => window.alert(message);
  const act = async (label: string, fn: () => Promise<unknown>) => {
    setBusy(label);
    try {
      await fn();
      props.onChanged();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const nodes = nodeRows(run, selected);

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
          onClick={() => void act("restart", () => api.restartRun(ref))}
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
                void act("stop", () => api.stopRun(ref));
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
        <ShareControls
          run={ref}
          running={run.status === "running"}
          peer={peerShare}
          web={webShares}
          onChanged={props.onChanged}
          onError={onError}
        />
      </Group>

      {peerShare && (
        <PeerShareStrip
          share={peerShare}
          onChanged={props.onChanged}
          onError={onError}
        />
      )}
      {webShares.map((w) => (
        <WebShareStrip
          key={w.id}
          share={w}
          onChanged={props.onChanged}
          onError={onError}
        />
      ))}

      <SegmentedControl
        size="xs"
        fullWidth
        mx={12}
        mb={6}
        styles={{
          root: {
            background: "var(--bg)",
            border: "1px solid var(--border)",
          },
        }}
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
        <NodeTable
          run={ref}
          nodes={nodes}
          stats={props.stats}
          canAct={run.status === "running"}
          onChanged={props.onChanged}
          onError={onError}
        />
      )}

      {logsEverOpened && (
        <LogsPanel
          run={ref}
          history={history}
          histSel={histSel || null}
          visible={tab === "logs"}
        />
      )}
    </div>
  );
}
