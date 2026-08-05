/**
 * One environment, as a card: head, run controls, share strips, and a
 * Nodes|Logs switcher over the **shared run views**.
 *
 * The card owns three things and no more — which run this is (head), what you can
 * do to it (controls), and which view you are looking at (switcher). The views
 * themselves are `shared/RunViews.tsx`, the same components IDE mode puts in panes,
 * so a change to what a node row or a log line shows lands in both places at once.
 */

import { useState } from "react";
import { Badge, Button, Group, NativeSelect, SegmentedControl, Text } from "@mantine/core";
import {
  api,
  runRef,
  type HistoryEntry,
  type NodeStats,
  type ProjectInfo,
  type RunInfo,
  type RunRef,
  type ShareInfo,
} from "../api";
import { LogsView, NodesView, badgeColor, type RunViewTarget } from "../shared/RunViews";
import { PeerShareStrip, ShareControls, WebShareStrip, sharesForRun } from "../shared/Sharing";
import { useCopyFlash } from "../shared/copy";
import { startOriginLabel } from "../shared/startOrigin";
import { notifyError } from "../shared/notify";
import { fmtWhen, statusBucket } from "../shared/util";

/** Restart / Stop / Terminal / Copy path — everything you can do to this run. */
function RunControls(props: {
  run: RunInfo;
  /** Not called `ref`: React treats that prop name as an element ref. */
  address: RunRef;
  projectRoot: string;
  onChanged: () => void;
}) {
  const { run } = props;
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
    <>
      <Button
        size="compact-xs"
        variant="default"
        loading={busy === "restart"}
        onClick={() => void act("restart", `Restart ${run.name}`, () => api.restartRun(props.address))}
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
              void act("stop", `Stop ${run.name}`, () => api.stopRun(props.address));
            }
          }}
        >
          Stop
        </Button>
      )}
      <Button
        size="compact-xs"
        variant="default"
        onClick={() =>
          void act("terminal", "Open a terminal", () => api.openTerminal(props.projectRoot))
        }
      >
        Terminal
      </Button>
      <Button
        size="compact-xs"
        variant="default"
        onClick={() => copy(props.projectRoot, "path")}
      >
        {flash === "path" ? "Copied" : "Copy path"}
      </Button>
    </>
  );
}

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
  const [view, setView] = useState<string>("nodes");
  const [logsEverOpened, setLogsEverOpened] = useState(false);
  const [histSel, setHistSel] = useState<string>("");

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

  // The card's head picker owns the history selection, so the views take it as a
  // prop rather than growing a second picker of their own.
  const target: RunViewTarget = {
    ref,
    run,
    stats: props.stats,
    onChanged: props.onChanged,
  };

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
            title="Which run"
            aria-label="Run"
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

      {/* What this run was started from — the answer to "is this the one I
          asked for, or the one something else started". `presets: null` because
          Runs mode polls /api/environments only and has no config to compare
          against: the line then states the preset in the past tense with the
          tokens that actually ran, instead of claiming the name still means
          this. The history selection deliberately carries its own origin, since
          an earlier run of the same environment may have come from elsewhere. */}
      {(() => {
        const from = startOriginLabel(
          selected ? selected.started_from : run.started_from,
          null,
        );
        return from ? (
          <Text size="xs" px={12} pt={4} c="dimmed">
            started from {from}
          </Text>
        ) : null;
      })()}

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
        <RunControls
          run={run}
          address={ref}
          projectRoot={project.project_root}
          onChanged={props.onChanged}
        />
        <ShareControls
          run={ref}
          running={run.status === "running"}
          peer={peerShare}
          web={webShares}
          onChanged={props.onChanged}
        />
      </Group>

      {peerShare && <PeerShareStrip share={peerShare} onChanged={props.onChanged} />}
      {/* Collapsed by default here, unlike in the sharing modal: this card sits in a
          list of every run, and a QR per service made one share taller than the card
          it hangs under — pushing the node list, which is what runs mode is for, off
          the screen. */}
      {webShares.map((w) => (
        <WebShareStrip key={w.id} share={w} collapsible onChanged={props.onChanged} />
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
        value={view}
        onChange={(v) => {
          setView(v);
          if (v === "logs") setLogsEverOpened(true);
        }}
        data={[
          { value: "nodes", label: "Nodes" },
          { value: "logs", label: "Logs" },
        ]}
      />

      {view === "nodes" && <NodesView target={target} selected={selected} />}

      {/* Mounted from the first time it is opened and kept mounted after: the
          filters, the scroll position and the fetched lines all survive a switch
          back to Nodes. */}
      {logsEverOpened && (
        <LogsView target={target} selected={selected} visible={view === "logs"} />
      )}
    </div>
  );
}
