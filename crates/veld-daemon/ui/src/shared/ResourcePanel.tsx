/**
 * The expanded resource view for one node: a scrubbable chart with a metric
 * picker, three split modes, and the live process table.
 *
 * Split modes are the reason this exists rather than one more sparkline:
 *
 * - **total** — one line for the chosen metric. "Is it growing?"
 * - **by type** — the page classes stacked. "Is that growth private dirty pages
 *   (a leak) or shared clean ones (mapped binaries)?" Hidden where the platform
 *   cannot split them, rather than drawn as an empty stack.
 * - **by process** — one band per subprocess. "Which child is it?"
 *
 * History is fetched from `/api/stats/history`, which buckets server-side, so
 * changing the window changes the resolution rather than the payload size. The
 * poll continues while the panel is open so the chart tracks live — at the
 * window's own bucket cadence, not the 5s sample cadence, since re-fetching a
 * six-hour window every 5s would be pointless work.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { Loader, SegmentedControl, Select, Text, Tooltip } from "@mantine/core";

import { api, type MemoryMetric, type RunRef, type StatsHistory } from "../api";
import { notifyError } from "./notify";
import { fmtBytes } from "./util";
import { ChartSeries, TimeSeriesChart } from "./charts/TimeSeriesChart";

type SplitMode = "total" | "type" | "process";

/** Window presets, in seconds. Capped at the daemon's 24h stats retention —
 * offering "7 days" would return four hours of data and look like a bug. */
const WINDOWS = [
  { value: "300", label: "5m" },
  { value: "900", label: "15m" },
  { value: "3600", label: "1h" },
  { value: "21600", label: "6h" },
  { value: "86400", label: "24h" },
];

/** Buckets requested. Roughly one per 3px of a wide panel. */
const POINTS = 240;

/** Human labels for the wire metric names. */
const METRIC_LABELS: Record<MemoryMetric, string> = {
  footprint: "Footprint",
  resident: "Resident (RSS)",
  virtual: "Virtual",
  private_clean: "Private clean",
  private_dirty: "Private dirty",
  shared_clean: "Shared clean",
  shared_dirty: "Shared dirty",
  swap: "Swap",
  wired: "Wired",
};

/** One-line explanation per metric — the difference between these is the whole
 * point of the picker, and it is not guessable from the name. */
const METRIC_HELP: Record<MemoryMetric, string> = {
  footprint:
    "Proportional set size (Linux) / phys_footprint (macOS). The only memory figure that sums correctly across a process tree.",
  resident: "Resident set size. Counts pages shared between the tree's processes once per process, so a multi-process node over-reports.",
  virtual: "Reserved address space. Huge and mostly meaningless for JVM/Go runtimes; a change in it still shows a mapping leak.",
  private_clean: "Pages only this tree maps, unmodified.",
  private_dirty: "Pages only this tree maps and has written — the heap. This is what grows when a node leaks.",
  shared_clean: "Resident pages shared with other processes, unmodified — mapped executables and libraries. Effectively free.",
  shared_dirty: "Pages shared and written: copy-on-write after fork, shared buffers.",
  swap: "Paged out to swap. Invisible in RSS and footprint — climbing here while RSS is flat means thrashing.",
  wired: "Cannot be paged out (mlock/wired).",
};

/** The page classes that stack into a total, in stack order. */
export const STACK_METRICS: MemoryMetric[] = [
  "private_dirty",
  "private_clean",
  "shared_dirty",
  "shared_clean",
  "swap",
];

/** Categorical slots for the page classes — fixed per class, so a class that is
 * absent on this platform leaves a hole in the palette rather than shifting the
 * colours of the ones that remain. */
const CLASS_SLOT: Record<string, number> = {
  private_dirty: 1,
  private_clean: 2,
  shared_dirty: 3,
  shared_clean: 4,
  swap: 5,
  wired: 6,
};

/** Read a metric off a bucket. Mirrors Rust's `MemoryMetric::read`. */
export function bucketValue(b: StatsHistory["buckets"][number], m: MemoryMetric): number | null {
  switch (m) {
    case "footprint":
      return b.footprint;
    case "resident":
      return b.resident;
    case "virtual":
      return b.virtual;
    default:
      return b[m];
  }
}

export function ResourcePanel(props: { run: RunRef; nodeKey: string }) {
  const [windowSecs, setWindowSecs] = useState("900");
  const [metric, setMetric] = useState<MemoryMetric>("footprint");
  const [split, setSplit] = useState<SplitMode>("total");
  const [data, setData] = useState<StatsHistory | null>(null);
  const [loading, setLoading] = useState(true);
  // One error toast per panel opening: a node whose history keeps failing must
  // not turn into a toast every poll interval.
  const warned = useRef(false);

  const secs = Number(windowSecs);
  const wantProcesses = split === "process";

  useEffect(() => {
    let alive = true;
    setLoading(true);
    const load = async () => {
      try {
        const h = await api.statsHistory(props.run, props.nodeKey, {
          windowSecs: secs,
          points: POINTS,
          processes: wantProcesses,
        });
        if (!alive) return;
        setData(h);
        warned.current = false;
      } catch (e) {
        if (!alive) return;
        if (!warned.current) {
          warned.current = true;
          notifyError(`Resource history for ${props.nodeKey}`, e);
        }
      } finally {
        if (alive) setLoading(false);
      }
    };
    void load();
    // Refresh at the window's own resolution (a bucket's worth), floored at 5s
    // — the sample cadence — and capped so a 24h window still feels live.
    const everyMs = Math.min(30_000, Math.max(5_000, (secs / POINTS) * 1000));
    const t = window.setInterval(() => void load(), everyMs);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [props.run.projectRoot, props.run.name, props.nodeKey, secs, wantProcesses]);

  const available = data?.available_metrics ?? ["footprint", "resident", "virtual"];
  const canSplitByType = STACK_METRICS.some((m) => available.includes(m));

  // A metric that stops being available (window scrolled past a platform change)
  // must not leave the picker pointing at nothing.
  useEffect(() => {
    if (data && !available.includes(metric)) setMetric("footprint");
  }, [data, available, metric]);

  useEffect(() => {
    if (split === "type" && !canSplitByType) setSplit("total");
  }, [split, canSplitByType]);

  const times = useMemo(() => (data?.buckets ?? []).map((b) => b.t), [data]);

  const series: ChartSeries[] = useMemo(() => {
    if (!data) return [];
    if (split === "type") {
      return STACK_METRICS.filter((m) => available.includes(m)).map((m) => ({
        key: m,
        label: METRIC_LABELS[m],
        slot: CLASS_SLOT[m],
        points: data.buckets.map((b) => bucketValue(b, m)),
      }));
    }
    if (split === "process") {
      // Bucket times differ per process (a process that started late has fewer
      // buckets), so index each series into the node's own time axis by `t`.
      return data.processes.map((p, i) => {
        const byTime = new Map(p.buckets.map((b) => [b.t, bucketValue(b, metric)]));
        return {
          key: String(p.pid),
          label: `${p.name} (${p.pid})`,
          // Slot by position in the (stably ordered) series list, folded into 8.
          slot: (i % 8) + 1,
          points: times.map((t) => byTime.get(t) ?? null),
        };
      });
    }
    return [
      {
        key: metric,
        label: METRIC_LABELS[metric],
        slot: 1,
        points: data.buckets.map((b) => bucketValue(b, metric)),
      },
    ];
  }, [data, split, metric, available, times]);

  const noData = !loading && times.length === 0;

  return (
    <div className="res-panel">
      <div className="res-controls">
        <SegmentedControl
          size="xs"
          value={split}
          onChange={(v) => setSplit(v as SplitMode)}
          data={[
            { value: "total", label: "Total" },
            // Disabled rather than hidden, with the reason in the tooltip: a
            // control that vanishes on macOS reads as a missing feature.
            {
              value: "type",
              label: canSplitByType ? "By type" : "By type (n/a)",
              disabled: !canSplitByType,
            },
            { value: "process", label: "By process" },
          ]}
        />
        {split !== "type" && (
          <Tooltip label={METRIC_HELP[metric]} multiline w={280} openDelay={300}>
            <Select
              size="xs"
              w={168}
              comboboxProps={{ withinPortal: true }}
              value={metric}
              onChange={(v) => v && setMetric(v as MemoryMetric)}
              data={available.map((m) => ({ value: m, label: METRIC_LABELS[m] }))}
              aria-label="Memory metric"
            />
          </Tooltip>
        )}
        <SegmentedControl
          size="xs"
          value={windowSecs}
          onChange={setWindowSecs}
          data={WINDOWS}
          aria-label="History window"
        />
        {loading && <Loader size="xs" />}
        {data && (
          <Text size="xs" c="dimmed" className="res-resolution">
            {data.bucket_secs}s buckets
          </Text>
        )}
      </div>

      {!canSplitByType && split !== "process" && (
        <Text size="xs" c="dimmed">
          This platform reports totals only — the private/shared page split needs
          Linux&apos;s <code>smaps_rollup</code>.
        </Text>
      )}

      {noData ? (
        <Text size="xs" c="dimmed">
          No samples in this window. The daemon samples every 5s while a run is
          up; per-process history is kept for 2h and totals for 24h.
        </Text>
      ) : (
        <TimeSeriesChart
          times={times}
          series={series}
          mode={split === "total" ? "line" : "stacked"}
          format={fmtBytes}
          windowStart={data?.start ?? Date.now() - secs * 1000}
          windowEnd={data?.end ?? Date.now()}
          ariaLabel={
            split === "type"
              ? "Memory by page class over time"
              : split === "process"
                ? `${METRIC_LABELS[metric]} by process over time`
                : `${METRIC_LABELS[metric]} over time`
          }
        />
      )}

      {split === "process" && data && data.processes_omitted > 0 && (
        <Text size="xs" c="dimmed">
          {data.processes_omitted} more process
          {data.processes_omitted === 1 ? "" : "es"} not charted — the heaviest are
          shown.
        </Text>
      )}

      {data && data.tree.length > 0 && <ProcessTable data={data} metric={metric} />}
    </div>
  );
}

/**
 * The live process tree. Also the table view the chart's accessibility story
 * leans on: every number on screen is readable here without colour.
 */
function ProcessTable(props: { data: StatsHistory; metric: MemoryMetric }) {
  const { data, metric } = props;
  const showClass = metric !== "footprint" && metric !== "resident" && metric !== "virtual";
  return (
    <div className="res-table-wrap">
      <table className="res-table">
        <thead>
          <tr>
            <th>PID</th>
            <th>Process</th>
            <th className="num">CPU</th>
            <th className="num">Footprint</th>
            <th className="num">RSS</th>
            {showClass && <th className="num">{METRIC_LABELS[metric]}</th>}
            <th className="num">CPU time</th>
          </tr>
        </thead>
        <tbody>
          {data.tree.map((p) => (
            <tr key={p.pid}>
              <td className="mono">{p.pid}</td>
              <td className="mono" title={p.cmd ?? undefined}>
                {/* Indent by depth: the parent may be absent from this list, so
                    the depth is the only reliable shape signal. */}
                <span style={{ paddingLeft: `${p.depth * 12}px` }}>{p.name}</span>
              </td>
              <td className="num">{Math.round(p.cpu)}%</td>
              <td className="num">{fmtBytes(p.footprint)}</td>
              <td className="num">{fmtBytes(p.resident)}</td>
              {showClass && (
                <td className="num">{p[metric] == null ? "—" : fmtBytes(p[metric] as number)}</td>
              )}
              <td className="num">{fmtCpuTime(p.cpu_seconds)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Cumulative CPU time. Mirrors the CLI's `output::fmt_cpu_time`. */
export function fmtCpuTime(seconds: number): string {
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)}m${String(Math.floor(seconds % 60)).padStart(2, "0")}s`;
  }
  return `${Math.floor(seconds / 3600)}h${String(Math.floor((seconds % 3600) / 60)).padStart(2, "0")}m`;
}
