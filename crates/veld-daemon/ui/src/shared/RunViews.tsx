/**
 * The two views of a run — Nodes and Logs — as the components **both** modes
 * render.
 *
 * Runs mode is the run's controls, a Nodes|Logs switcher, and these; IDE mode is
 * the same two behind pane tabs. Everything specific to a host is a prop:
 *
 * - `fill` — take the parent's whole height (a pane) instead of sitting at a fixed
 *   height inside a scrolling card.
 * - `visible` — card mode keeps a hidden view mounted so its filters and scroll
 *   survive a tab switch; a pane unmounts the tab it is not showing.
 * - `selected` / `onSelected` — who owns the history selection. Runs mode's card
 *   head has the picker (it also drives the card's outcome line), so it passes the
 *   entry down; a pane has no head, so the view shows its own picker. One switch,
 *   not two implementations of "which run am I looking at".
 *
 * When the nodes view grows (scrubbable resource timelines are the obvious next
 * step) it grows once, here.
 */

import { Badge, Group, NativeSelect, Text } from "@mantine/core";
import { IconActivityHeartbeat, IconLogs } from "@tabler/icons-react";
import { useState } from "react";
import type { HistoryEntry, NodeStats, RunInfo, RunRef } from "../api";
import { LogsPanel } from "./LogsPanel";
import { NodeList, nodeRows } from "./NodeList";
import { fmtWhen, statusBucket } from "./util";

/** Everything a run view needs about the run it is showing. */
export interface RunViewTarget {
  /** Project-scoped run address — a bare name is ambiguous across repos. */
  ref: RunRef;
  run: RunInfo;
  /** Stats for this run, keyed `node:variant`. */
  stats?: Record<string, NodeStats>;
  /** Re-poll after an action landed. */
  onChanged: () => void;
}

interface HostProps {
  /** Fill the parent (a pane) rather than sit at a fixed height in a card. */
  fill?: boolean;
  /** Card mode: false keeps the view mounted but hidden. Defaults to visible. */
  visible?: boolean;
  /**
   * The history entry being viewed, when the *host* owns that choice. `undefined`
   * means "you own it" and the view renders its own picker.
   */
  selected?: HistoryEntry | null;
}

export function badgeColor(status: string): string {
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

/** Options for a run-history picker: the live run, then each ended one. */
function historyOptions(run: RunInfo): Array<{ value: string; label: string }> {
  return [
    {
      value: "",
      label: run.live
        ? `current run · ${run.status}`
        : `${run.outcome || run.status} · ${fmtWhen(run.ended_at)}`,
    },
    ...(run.history ?? []).map((h) => ({
      value: h.run_id,
      label: `${h.outcome || h.status} · ${fmtWhen(h.created_at)}`,
    })),
  ];
}

/**
 * Per-node health, resource usage and actions.
 *
 * The header carries what a pane has no other way to know — which run these nodes
 * belong to and what state it is in — and, when the view owns the history
 * selection, the picker for reading an earlier run's final node states. A card
 * suppresses the header (`fill` off): its own head already says all of that.
 */
export function NodesView(props: { target: RunViewTarget } & HostProps) {
  const { ref, run } = props.target;
  const [ownSel, setOwnSel] = useState<string>("");
  const hostOwnsHistory = props.selected !== undefined;
  const history = run.history ?? [];
  const selected = hostOwnsHistory
    ? (props.selected ?? null)
    : (history.find((h) => h.run_id === ownSel) ?? null);
  const shownStatus = selected?.status ?? run.status;

  return (
    <div className={props.fill ? "run-view fill" : "run-view"}>
      {props.fill && (
        <Group gap="xs" px={12} pt={8} pb={2} wrap="wrap">
          <Text size="xs" ff="monospace" fw={700}>
            {run.name}
          </Text>
          <Badge size="xs" variant="light" color={badgeColor(shownStatus)}>
            {shownStatus}
          </Badge>
          {!hostOwnsHistory && history.length > 0 && (
            <NativeSelect
              size="xs"
              title="Which run's nodes"
              aria-label="Run"
              value={ownSel}
              onChange={(e) => setOwnSel(e.currentTarget.value)}
              data={historyOptions(run)}
            />
          )}
          {selected ? (
            <Text size="xs" c={statusBucket(shownStatus) === "red" ? "red" : "dimmed"}>
              {/* An ended run has no live URLs, pids or actions — say why the row
                  is thinner rather than letting it look broken. */}
              past run · final node states
            </Text>
          ) : (
            !run.live && (
              <Text size="xs" c="dimmed">
                last run — start it again for live health
              </Text>
            )
          )}
        </Group>
      )}
      <NodeList
        run={ref}
        nodes={nodeRows(run, selected)}
        stats={selected ? undefined : props.target.stats}
        canAct={!selected && run.status === "running"}
        onChanged={props.target.onChanged}
      />
    </div>
  );
}

/**
 * The run's logs.
 *
 * `LogsPanel` already has a run picker of its own (latest / each ended run / all
 * interleaved), so history needs nothing extra here — the host's selection only
 * scopes what "latest" means.
 */
export function LogsView(props: { target: RunViewTarget } & HostProps) {
  const { ref, run } = props.target;
  const selected = props.selected ?? null;
  return (
    <LogsPanel
      // Keyed by run instance: the filters and the accumulated node list belong to
      // the run being read, so a worktree switch — or a restart minting a new run —
      // must not carry another run's node filter in.
      key={`${ref.projectRoot}::${ref.name}::${run.run_id}`}
      run={ref}
      history={run.history ?? []}
      histSel={selected?.run_id ?? null}
      visible={props.visible ?? true}
      fill={props.fill}
    />
  );
}

/** A view with no run to show, centred in a pane. */
export function NoRunView(props: { kind: "logs" | "nodes"; hint: string }) {
  return (
    <div className="pane-empty">
      <div className="links-empty">
        {props.kind === "logs" ? <IconLogs size={26} /> : <IconActivityHeartbeat size={26} />}
        <p className="pane-screen-title">
          {props.kind === "logs" ? "No logs yet" : "Nothing running"}
        </p>
        <p className="faint">{props.hint}</p>
      </div>
    </div>
  );
}
