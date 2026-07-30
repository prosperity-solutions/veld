/**
 * The run's diagnostics, as panes: `logs` and `nodes`.
 *
 * Both are thin wrappers over the components runs mode uses
 * (`shared/LogsPanel.tsx`, `shared/NodeTable.tsx`) — the work here is the pane
 * shape (fill the dock body, scroll inside it) and the honest empty state, since
 * a worktree may have no veld.json and a worktree that has one may have nothing
 * running.
 *
 * Neither pane holds a run identity: they render whatever run the *selected*
 * worktree has, so switching worktrees re-points every open diagnostics pane —
 * the same rule the top bar's run controls follow. That is why the run arrives as
 * a prop from `App` rather than being captured into the tab when it is opened;
 * capturing it would leave a pane showing a run whose worktree is no longer on
 * screen, with no way to tell from the tab strip.
 */

import { Badge, Group, Text } from "@mantine/core";
import { IconActivityHeartbeat, IconLogs } from "@tabler/icons-react";
import type { NodeStats, RunInfo, RunRef } from "../api";
import { LogsPanel } from "../shared/LogsPanel";
import { NodeTable, nodeRows } from "../shared/NodeTable";
import { statusBucket } from "../shared/util";

/** What the diagnostics panes need about the selected worktree's run. */
export interface RunPaneContext {
  /** Project-scoped run address; null when the worktree has no run. */
  ref: RunRef | null;
  run: RunInfo | null;
  /** Stats for this run, keyed `node:variant`. */
  stats?: Record<string, NodeStats>;
  /** Why there is no run — only the app knows (no veld.json, or nothing started). */
  emptyHint: string;
  /** Re-poll after an action landed. */
  onChanged: () => void;
  /** Report a failed action (IDE mode's banner). */
  onError: (message: string) => void;
}

/** The same empty screen the URL launcher uses, centred in a pane. */
function NoRun(props: { icon: React.ReactNode; title: string; hint: string }) {
  return (
    <div className="pane-empty">
      <div className="links-empty">
        {props.icon}
        <p className="pane-screen-title">{props.title}</p>
        <p className="faint">{props.hint}</p>
      </div>
    </div>
  );
}

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

export function LogsPane(props: { ctx: RunPaneContext }) {
  const { ref, run } = props.ctx;
  if (!ref || !run) {
    return (
      <NoRun
        icon={<IconLogs size={26} />}
        title="No logs yet"
        hint={props.ctx.emptyHint}
      />
    );
  }
  return (
    <LogsPanel
      // Keyed by run instance: the filters and the accumulated node list belong
      // to the run being read, and a worktree switch (or a restart minting a new
      // run) must not carry another run's node filter into this one.
      key={`${ref.projectRoot}::${ref.name}::${run.run_id}`}
      run={ref}
      history={run.history ?? []}
      histSel={null}
      visible
      fill
    />
  );
}

export function NodesPane(props: { ctx: RunPaneContext }) {
  const { ref, run } = props.ctx;
  if (!ref || !run) {
    return (
      <NoRun
        icon={<IconActivityHeartbeat size={26} />}
        title="Nothing running"
        hint={props.ctx.emptyHint}
      />
    );
  }
  return (
    <div className="nodes-pane">
      <Group gap="xs" px={12} pt={8} pb={2} wrap="wrap">
        <Text size="xs" ff="monospace" fw={700}>
          {run.name}
        </Text>
        <Badge size="xs" variant="light" color={badgeColor(run.status)}>
          {run.status}
        </Badge>
        {!run.live && (
          <Text size="xs" c="dimmed">
            last run — start it again for live health
          </Text>
        )}
      </Group>
      <NodeTable
        run={ref}
        // The live rows, never a history entry: picking a past run is the runs-mode
        // card's job, and this pane is about what is happening now.
        nodes={nodeRows(run, null)}
        stats={props.ctx.stats}
        canAct={run.status === "running"}
        onChanged={props.ctx.onChanged}
        onError={props.ctx.onError}
      />
    </div>
  );
}
