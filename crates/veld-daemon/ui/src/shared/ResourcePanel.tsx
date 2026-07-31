/**
 * The expanded resource view for one node: a scrubbable chart with a metric
 * picker, three split modes, and the live process table.
 *
 * Two dimensions, **memory** and **CPU**, on their own axes in their own units —
 * never together. Bytes and percent on one plot needs two y-scales, and a
 * dual-axis chart lets the author decide which line looks higher, so the reader
 * cannot. The toggle is the honest version of that request.
 *
 * Split modes are the reason this exists rather than one more sparkline:
 *
 * - **total** — one line for the chosen metric. "Is it growing?" Wide buckets
 *   also get a peak line, because a mean over a 6-minute bucket hides the spike
 *   that a 5s sample caught.
 * - **by type** — the page classes stacked. "Is that growth private dirty pages
 *   (a leak) or shared clean ones (mapped binaries)?" Hidden where the platform
 *   cannot split them, rather than drawn as an empty stack. Memory only: CPU has
 *   no page classes.
 * - **by process** — one band per subprocess. "Which child is it?" Stacked in
 *   both dimensions, because per-process CPU and per-process memory both sum to
 *   the tree's own figure.
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
import {
  ChartSeries,
  PALETTE_SLOTS,
  TimeSeriesChart,
  assignSlots,
} from "./charts/TimeSeriesChart";

type SplitMode = "total" | "type" | "process";

/** Which resource the chart plots. Separate charts, never one with two axes. */
type Dimension = "memory" | "cpu";

/** Candidate window presets, in seconds. Filtered against the retention the
 * daemon actually reports (`retention_secs`) — offering a range that returns
 * nothing looks like a bug, and a hardcoded cap here silently stops matching the
 * GC the first time retention is retuned. */
const WINDOW_CHOICES = [
  { value: "300", label: "5m" },
  { value: "900", label: "15m" },
  { value: "3600", label: "1h" },
  { value: "21600", label: "6h" },
  { value: "86400", label: "24h" },
];

/** Retention assumed before the first response arrives. Only ever narrows the
 * picker for one poll, and the server clamps regardless. */
const ASSUMED_RETENTION_SECS = 86400;

function windowPresets(retentionSecs: number) {
  const usable = WINDOW_CHOICES.filter((w) => Number(w.value) <= retentionSecs);
  // Never present an empty picker, however short retention gets.
  return usable.length > 0 ? usable : [WINDOW_CHOICES[0]];
}

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
export const STACK_METRICS = [
  "private_dirty",
  "private_clean",
  "shared_dirty",
  "shared_clean",
  "swap",
] as const satisfies readonly MemoryMetric[];

/** The stackable page classes, as a narrow union rather than all of
 * `MemoryMetric` — so a lookup table keyed by it is exhaustively checked. */
export type StackMetric = (typeof STACK_METRICS)[number];

/** Categorical slots for the page classes — fixed per class, so a class that is
 * absent on this platform leaves a hole in the palette rather than shifting the
 * colours of the ones that remain.
 *
 * Keyed by the narrow [`StackMetric`] union, NOT by `string`: with a `string` key
 * an engineer who adds a page class and follows the compiler through
 * `METRIC_LABELS`/`METRIC_HELP` (which do force every key) would find this one
 * compiles clean while `CLASS_SLOT[m]` returns `undefined`, yielding
 * `var(--series-undefined)` — a series silently missing from the chart with no
 * compile or runtime error. */
const CLASS_SLOT: Record<StackMetric | "wired", number> = {
  private_dirty: 1,
  private_clean: 2,
  shared_dirty: 3,
  shared_clean: 4,
  swap: 5,
  wired: 6,
};

/** CPU as a percentage of one core. Can exceed 100% on a multi-threaded tree,
 * which is correct — clamping it would hide the most interesting case. */
export function fmtPercent(v: number): string {
  return `${v < 10 ? v.toFixed(1) : Math.round(v)}%`;
}

/**
 * Read a metric off a bucket. Mirrors Rust's `MemoryMetric::read` — including its
 * absence gate on `virtual`, which is the whole reason this is a function and not
 * `b[m]`.
 *
 * A live process cannot occupy zero bytes of address space, so `virtual: 0` means
 * "not recorded" — a bucket built from rows written before the breakdown columns
 * existed. Rust reports that as absent; without the same gate here the chart drew
 * a real line down to zero for exactly the samples `veld stats` printed as `-`,
 * two readers of one artifact disagreeing about absent-vs-zero.
 */
export function bucketValue(b: StatsHistory["buckets"][number], m: MemoryMetric): number | null {
  switch (m) {
    case "footprint":
      return b.footprint;
    case "resident":
      return b.resident;
    case "virtual":
      return b.virtual > 0 ? b.virtual : null;
    default:
      return b[m];
  }
}

export function ResourcePanel(props: { run: RunRef; nodeKey: string }) {
  const [windowSecs, setWindowSecs] = useState("900");
  const [dimension, setDimension] = useState<Dimension>("memory");
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
  // CPU has no page classes, so "by type" belongs to the memory dimension only.
  const canSplitByType =
    dimension === "memory" && STACK_METRICS.some((m) => available.includes(m));
  const isCpu = dimension === "cpu";

  /**
   * Whether a bucket covers more than one raw sample. When it doesn't — a 5m
   * window at 1s resolution — the mean and the peak are the same number, and
   * plotting both would draw one line twice and pad the legend with a duplicate.
   */
  const bucketsAggregate = useMemo(
    () => (data?.buckets ?? []).some((b) => b.samples > 1),
    [data],
  );

  // A metric that stops being available (window scrolled past a platform change)
  // must not leave the picker pointing at nothing.
  useEffect(() => {
    if (data && !available.includes(metric)) setMetric("footprint");
  }, [data, available, metric]);

  useEffect(() => {
    if (split === "type" && !canSplitByType) setSplit("total");
  }, [split, canSplitByType]);

  /**
   * PID → colour slot, carried across polls.
   *
   * A ref rather than state: assigning a slot must not trigger a render, and the
   * map has to survive every poll so a process keeps its colour. The allocation
   * itself lives in `assignSlots`, which also guarantees no two charted PIDs
   * share a slot at the same time.
   */
  const slots = useRef(new Map<number, number>());

  /** The value this dimension plots, per bucket. */
  const readBucket = useMemo(
    () =>
      isCpu
        ? (b: StatsHistory["buckets"][number]) => b.cpu
        : (b: StatsHistory["buckets"][number]) => bucketValue(b, metric),
    [isCpu, metric],
  );

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
      // The palette is eight fixed hues and a ninth is never generated, so past
      // that the tail folds into one "Other" band. Folding rather than dropping
      // matters here: these bands are STACKED, so discarding the tail would make
      // the stack silently sum to less than the node's own figure.
      const foldTail = data.processes.length > PALETTE_SLOTS;
      const individual = foldTail
        ? data.processes.slice(0, PALETTE_SLOTS - 1)
        : data.processes;
      const tail = foldTail ? data.processes.slice(PALETTE_SLOTS - 1) : [];

      // Bucket times differ per process (a process that started late has fewer
      // buckets), so index each series into the node's own time axis by `t`.
      const assigned = assignSlots(
        individual.map((p) => p.pid),
        slots.current,
      );
      slots.current = assigned;

      const out: ChartSeries[] = individual.map((p) => {
        const byTime = new Map(p.buckets.map((b) => [b.t, readBucket(b)]));
        return {
          key: String(p.pid),
          label: `${p.name} (${p.pid})`,
          slot: assigned.get(p.pid) ?? PALETTE_SLOTS,
          points: times.map((t) => byTime.get(t) ?? null),
        };
      });

      if (tail.length > 0) {
        const byTime = new Map<number, number>();
        const seen = new Set<number>();
        for (const p of tail) {
          for (const b of p.buckets) {
            const v = readBucket(b);
            if (v == null) continue;
            seen.add(b.t);
            byTime.set(b.t, (byTime.get(b.t) ?? 0) + v);
          }
        }
        out.push({
          key: "other",
          label: `Other (${tail.length} processes)`,
          // The last slot, always — "Other" is not an entity competing for a
          // colour, so it never participates in the stable assignment.
          slot: PALETTE_SLOTS,
          // A bucket where none of the tail existed is absent, not zero.
          points: times.map((t) => (seen.has(t) ? (byTime.get(t) ?? 0) : null)),
        });
      }
      return out;
    }
    const mean: ChartSeries = {
      key: isCpu ? "cpu" : metric,
      label: isCpu ? "CPU" : METRIC_LABELS[metric],
      slot: 1,
      points: data.buckets.map(readBucket),
    };
    // The peak within each bucket, where a bucket actually spans several
    // samples. Same unit and same axis as the mean, so this is one measure at
    // two statistics — not a second scale.
    const peak: ChartSeries | null =
      !bucketsAggregate || (!isCpu && metric !== "footprint")
        ? null
        : {
            key: isCpu ? "cpu_peak" : "footprint_peak",
            label: isCpu ? "CPU (peak)" : "Footprint (peak)",
            slot: 2,
            points: data.buckets.map((b) => (isCpu ? b.cpu_peak : b.footprint_peak)),
          };
    // Peak first so the mean draws over it rather than under.
    return peak ? [peak, mean] : [mean];
  }, [data, split, metric, available, times, isCpu, readBucket, bucketsAggregate]);

  const noData = !loading && times.length === 0;

  return (
    <div className="res-panel">
      <div className="res-controls">
        <SegmentedControl
          size="xs"
          value={dimension}
          onChange={(v) => setDimension(v as Dimension)}
          data={[
            { value: "memory", label: "Memory" },
            { value: "cpu", label: "CPU" },
          ]}
          aria-label="Resource"
        />
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
            // `disabled` above covers both reasons it can be off — CPU has no
            // page classes, and macOS cannot measure them — which the note under
            // the controls tells apart.

            { value: "process", label: "By process" },
          ]}
        />
        {split !== "type" && !isCpu && (
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
          data={windowPresets(data?.retention_secs ?? ASSUMED_RETENTION_SECS)}
          aria-label="History window"
        />
        {loading && <Loader size="xs" />}
        {data && (
          <Text size="xs" c="dimmed" className="res-resolution">
            {data.bucket_secs}s buckets
          </Text>
        )}
      </div>

      {isCpu ? (
        <Text size="xs" c="dimmed">
          CPU is a percentage of one core, so a multi-threaded tree legitimately
          exceeds 100%.
        </Text>
      ) : (
        !canSplitByType &&
        split !== "process" && (
          <Text size="xs" c="dimmed">
            This platform reports totals only — the private/shared page split
            needs Linux&apos;s <code>smaps_rollup</code>.
          </Text>
        )
      )}

      {noData ? (
        <Text size="xs" c="dimmed">
          No samples in this window. The daemon samples every 5s while a run is
          up; totals are kept for{" "}
          {formatDuration(data?.retention_secs ?? ASSUMED_RETENTION_SECS)} and
          per-process detail for{" "}
          {formatDuration(data?.process_retention_secs ?? 7200)}.
        </Text>
      ) : (
        <TimeSeriesChart
          times={times}
          series={series}
          mode={split === "total" ? "line" : "stacked"}
          // Processes: an absent PID did not exist then, so it contributes zero
          // and the rest of the stack still draws. Page classes: an absent class
          // is unmeasurable, so a partial stack would understate the total.
          stackPresence={split === "process" ? "any" : "all"}
          format={isCpu ? fmtPercent : fmtBytes}
          windowStart={data?.start ?? Date.now() - secs * 1000}
          windowEnd={data?.end ?? Date.now()}
          ariaLabel={
            split === "type"
              ? "Memory by page class over time"
              : split === "process"
                ? `${isCpu ? "CPU" : METRIC_LABELS[metric]} by process over time`
                : `${isCpu ? "CPU" : METRIC_LABELS[metric]} over time`
          }
        />
      )}

      {/* The by-process view is legitimately empty before the per-process
          horizon, which is shorter than the aggregate one. Saying so beats
          leaving most of the axis blank with no explanation. */}
      {split === "process" && data && secs > data.process_retention_secs && (
        <Text size="xs" c="dimmed">
          Per-process history covers the last{" "}
          {formatDuration(data.process_retention_secs)} — the rest of this{" "}
          {formatDuration(secs)} window has node totals only.
        </Text>
      )}

      {/* F7: peaks exist for footprint and CPU only, so on another metric with
          wide buckets the mean is all there is — and the spike-hiding the peak
          line exists to counter still applies. */}
      {split === "total" &&
        bucketsAggregate &&
        !isCpu &&
        metric !== "footprint" && (
          <Text size="xs" c="dimmed">
            Buckets average {data?.bucket_secs}s of samples here; peaks are
            recorded for footprint and CPU only, so a brief spike in{" "}
            {METRIC_LABELS[metric]} may not show.
          </Text>
        )}

      {split === "process" && data && data.processes_omitted > 0 && (
        <Text size="xs" c="dimmed">
          {data.processes_omitted} more process
          {data.processes_omitted === 1 ? "" : "es"} not charted — the ones with the
          largest memory footprint are shown, which on the CPU view may not be the
          busiest.
        </Text>
      )}

      {data && data.tree.length > 0 && (
        // On the CPU view the memory-metric picker is hidden, so its last value
        // would be an extra column nobody chose — pass null and drop it.
        <ProcessTable data={data} classMetric={isCpu ? null : metric} />
      )}
    </div>
  );
}

/**
 * The live process tree. Also the table view the chart's accessibility story
 * leans on: every number on screen is readable here without colour.
 */
function ProcessTable(props: { data: StatsHistory; classMetric: MemoryMetric | null }) {
  const { data, classMetric: metric } = props;
  const showClass =
    metric != null && metric !== "footprint" && metric !== "resident" && metric !== "virtual";
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
            {showClass && <th className="num">{METRIC_LABELS[metric!]}</th>}
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
                <td className="num">
                  {p[metric!] == null ? "—" : fmtBytes(p[metric!] as number)}
                </td>
              )}
              <td className="num">{fmtCpuTime(p.cpu_seconds)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** A retention/window length as a human phrase ("2h", "24h", "15m"). */
function formatDuration(secs: number): string {
  if (secs % 3600 === 0) return `${secs / 3600}h`;
  if (secs % 60 === 0) return `${secs / 60}m`;
  return `${secs}s`;
}

/** Cumulative CPU time. Mirrors the CLI's `output::fmt_cpu_time`. */
export function fmtCpuTime(seconds: number): string {
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)}m${String(Math.floor(seconds % 60)).padStart(2, "0")}s`;
  }
  return `${Math.floor(seconds / 3600)}h${String(Math.floor((seconds % 3600) / 60)).padStart(2, "0")}m`;
}
