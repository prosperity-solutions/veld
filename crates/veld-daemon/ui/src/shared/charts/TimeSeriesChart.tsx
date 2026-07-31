/**
 * A small, dependency-free time-series chart: line or stacked area, with a
 * scrubbable crosshair.
 *
 * Why hand-rolled rather than a charting library: this UI is bundled by
 * `vite-plugin-singlefile` into one HTML file that the daemon `include_str!`s
 * into its binary, so every kilobyte of chart library is a kilobyte in
 * `veld-daemon`. The interaction here (crosshair over a gap-aware series, a
 * pinned cursor, values in the legend) is also most of what a library would be
 * used for.
 *
 * # The gap rule
 *
 * A bucket the daemon never sampled is **absent**, not zero. The API omits it
 * and this chart draws nothing there — the line breaks and the stack ends. A
 * chart that joined across the gap would show a smooth line through a period
 * when the daemon was down, which is a fabricated measurement. Every path is
 * therefore built per contiguous run of present points, not per series.
 *
 * # Colour
 *
 * Series take `--series-1..8` in fixed slot order, keyed to the entity (a memory
 * class, a PID) rather than to its rank, so filtering or reordering never
 * repaints the survivors. The palette is validated for both themes in
 * `styles.css`; slots past 8 are folded into "Other" by the caller rather than
 * generated here.
 */
import { useCallback, useMemo, useRef, useState } from "react";

export interface ChartSeries {
  /** Stable identity — drives the colour slot, so it must not depend on order. */
  key: string;
  label: string;
  /** 1-based categorical slot (`--series-N`). */
  slot: number;
  /** One entry per `times` index; `null` where the metric was not sampled. */
  points: (number | null)[];
}

/** Where the crosshair currently is, and whether the reader pinned it. */
interface Cursor {
  index: number;
  pinned: boolean;
}

const PAD = { top: 8, right: 8, bottom: 18, left: 46 };

/** Nice-ish upper bound so the axis label is a round number. */
export function axisMax(raw: number): number {
  if (raw <= 0) return 1;
  const pow = 10 ** Math.floor(Math.log10(raw));
  for (const step of [1, 1.5, 2, 2.5, 3, 4, 5, 7.5, 10]) {
    if (raw <= step * pow) return step * pow;
  }
  return 10 * pow;
}

/**
 * How a stack decides an index is drawable — and it genuinely differs by what
 * the bands mean, which is why it is a parameter and not a constant.
 *
 * - `"all"` — every band must have a value. For **page classes**: an absent
 *   class is *unmeasurable*, not zero, so a partial stack would draw a total
 *   that is silently too small. Same rule as `MemoryBreakdown::add` in Rust.
 * - `"any"` — one band with a value is enough, and an absent band contributes
 *   zero. For **processes**: an absent PID did not exist at that instant, so the
 *   sum over the processes that did exist *is* the tree's figure. Requiring all
 *   of them would blank the whole chart wherever any child was missing — and
 *   children come and go constantly, so that is most of a real window.
 */
export type StackPresence = "all" | "any";

/** Split indices into runs where `present` holds. */
export function contiguousRuns(length: number, present: (i: number) => boolean): number[][] {
  const runs: number[][] = [];
  let run: number[] = [];
  for (let i = 0; i < length; i++) {
    if (present(i)) {
      run.push(i);
    } else if (run.length) {
      runs.push(run);
      run = [];
    }
  }
  if (run.length) runs.push(run);
  return runs;
}

/**
 * Build the stacked band geometry: one SVG path per series, laid on a running
 * baseline, broken into contiguous runs.
 *
 * Extracted from the component (and pure) because this is the only chart
 * geometry that can be wrong *silently* — a baseline accumulated over the wrong
 * index set makes bands overlap or float off the axis, which renders as a
 * plausible chart rather than as an error.
 *
 * `x`/`y` are the scales; `presence` is the policy above.
 */
export function stackBands(
  series: ChartSeries[],
  length: number,
  presence: StackPresence,
  x: (i: number) => number,
  y: (v: number) => number,
): { key: string; slot: number; d: string }[] {
  const present =
    presence === "all"
      ? (i: number) => series.every((s) => s.points[i] != null)
      : (i: number) => series.some((s) => s.points[i] != null);
  const runs = contiguousRuns(length, present);
  const baselines = new Array(length).fill(0);
  const out: { key: string; slot: number; d: string }[] = [];
  for (const s of series) {
    const parts: string[] = [];
    for (const run of runs) {
      if (run.length === 1) {
        // A single-point run has no area to fill; a 1px-wide sliver would be
        // invisible anyway, so draw a short tick instead of dropping it.
        const i = run[0];
        const x0 = x(i);
        parts.push(
          `M${x0.toFixed(1)},${y(baselines[i]).toFixed(1)} L${x0.toFixed(1)},${y(
            baselines[i] + (s.points[i] ?? 0),
          ).toFixed(1)}`,
        );
        continue;
      }
      const top = run.map(
        (i) => `${x(i).toFixed(1)},${y(baselines[i] + (s.points[i] ?? 0)).toFixed(1)}`,
      );
      const bottom = [...run].reverse().map((i) => `${x(i).toFixed(1)},${y(baselines[i]).toFixed(1)}`);
      parts.push(`M${top.join(" L")} L${bottom.join(" L")} Z`);
    }
    out.push({ key: s.key, slot: s.slot, d: parts.join(" ") });
    // Accumulate over the drawn indices only — accumulating everywhere would
    // raise the baseline under gaps and float the next band off the axis.
    for (const i of runs.flat()) baselines[i] += s.points[i] ?? 0;
  }
  return out;
}

/** Per-index stack tops, for the y-axis maximum. */
export function stackedMax(series: ChartSeries[], length: number): number {
  let m = 0;
  for (let i = 0; i < length; i++) {
    let sum = 0;
    for (const s of series) sum += s.points[i] ?? 0;
    m = Math.max(m, sum);
  }
  return m;
}

export function TimeSeriesChart(props: {
  /** Bucket start times, epoch ms, ascending. */
  times: number[];
  series: ChartSeries[];
  mode: "line" | "stacked";
  /** Formats a value for the axis and the tooltip. */
  format: (v: number) => string;
  height?: number;
  /** Window bounds (epoch ms) — the x axis spans these, not the data extent, so
   * a run that only just started doesn't stretch three points across the width. */
  windowStart: number;
  windowEnd: number;
  /** Describes the chart for screen readers; the legend carries the values. */
  ariaLabel: string;
  /** Stacked-mode presence policy (see [`StackPresence`]). Defaults to `"all"`,
   * the conservative choice: it can only ever draw less than the truth. */
  stackPresence?: StackPresence;
}) {
  const { times, series, mode, format, windowStart, windowEnd } = props;
  const stackPresence = props.stackPresence ?? "all";
  const height = props.height ?? 150;
  // Fixed viewBox with `preserveAspectRatio="none"` would distort the strokes,
  // so the SVG scales via CSS width and a measured box instead.
  const [width, setWidth] = useState(600);
  const [cursor, setCursor] = useState<Cursor | null>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);

  const measure = useCallback((el: SVGSVGElement | null) => {
    svgRef.current = el;
    if (!el) return;
    const apply = () => setWidth(Math.max(120, el.clientWidth || 600));
    apply();
    // The panel is collapsible and the window resizes; a stale width would
    // squash the plot into the left edge.
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const plotW = Math.max(1, width - PAD.left - PAD.right);
  const plotH = Math.max(1, height - PAD.top - PAD.bottom);

  /** Per-index stack tops (stacked mode) or the max single value (line mode). */
  const max = useMemo(() => {
    if (mode === "stacked") return axisMax(stackedMax(series, times.length));
    let m = 0;
    for (const s of series) {
      for (const v of s.points) if (v != null) m = Math.max(m, v);
    }
    return axisMax(m);
  }, [series, times.length, mode]);

  const span = Math.max(1, windowEnd - windowStart);
  const x = (i: number) => PAD.left + ((times[i] - windowStart) / span) * plotW;
  const y = (v: number) => PAD.top + plotH - (v / max) * plotH;

  const bands =
    mode === "stacked" ? stackBands(series, times.length, stackPresence, x, y) : [];
  const lines: { key: string; slot: number; d: string }[] = [];

  if (mode !== "stacked") {
    for (const s of series) {
      const runs = contiguousRuns(times.length, (i) => s.points[i] != null);
      const parts = runs.map((run) => {
        const pts = run.map((i) => `${x(i).toFixed(1)},${y(s.points[i] as number).toFixed(1)}`);
        // A lone point gets a dot-sized segment so a single sample is visible.
        return pts.length === 1 ? `M${pts[0]} L${pts[0]}` : `M${pts.join(" L")}`;
      });
      lines.push({ key: s.key, slot: s.slot, d: parts.join(" ") });
    }
  }

  const nearestIndex = (clientX: number): number | null => {
    const el = svgRef.current;
    if (!el || times.length === 0) return null;
    const box = el.getBoundingClientRect();
    const px = clientX - box.left;
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < times.length; i++) {
      const d = Math.abs(x(i) - px);
      if (d < bestDist) {
        bestDist = d;
        best = i;
      }
    }
    // Don't snap from the far side of the plot — an off-data hover should read
    // as "nothing here" rather than silently reporting a distant bucket.
    return bestDist <= Math.max(24, plotW / Math.max(1, times.length)) ? best : null;
  };

  const onMove = (e: React.PointerEvent<SVGSVGElement>) => {
    if (cursor?.pinned) return;
    const i = nearestIndex(e.clientX);
    setCursor(i == null ? null : { index: i, pinned: false });
  };

  const onKeyDown = (e: React.KeyboardEvent<SVGSVGElement>) => {
    if (times.length === 0) return;
    const step = e.key === "ArrowLeft" ? -1 : e.key === "ArrowRight" ? 1 : 0;
    if (step === 0) {
      if (e.key === "Escape") setCursor(null);
      return;
    }
    e.preventDefault();
    const from = cursor?.index ?? times.length - 1;
    const next = Math.min(times.length - 1, Math.max(0, from + step));
    setCursor({ index: next, pinned: cursor?.pinned ?? false });
  };

  const cur = cursor ? cursor.index : null;
  const gridLines = [0.25, 0.5, 0.75, 1];

  return (
    <div className="chart">
      <svg
        ref={measure}
        className="chart-svg"
        height={height}
        role="img"
        tabIndex={0}
        aria-label={props.ariaLabel}
        onPointerMove={onMove}
        onPointerLeave={() => !cursor?.pinned && setCursor(null)}
        onClick={(e) => {
          const i = nearestIndex(e.clientX);
          if (i == null) return setCursor(null);
          // Pinning lets a reader move the pointer to the legend and still read
          // the values at the instant they picked.
          setCursor((c) => (c?.pinned && c.index === i ? null : { index: i, pinned: true }));
        }}
        onKeyDown={onKeyDown}
      >
        {gridLines.map((f) => (
          <line
            key={f}
            className="chart-grid"
            x1={PAD.left}
            x2={PAD.left + plotW}
            y1={y(max * f)}
            y2={y(max * f)}
          />
        ))}
        <line
          className="chart-axis"
          x1={PAD.left}
          x2={PAD.left + plotW}
          y1={PAD.top + plotH}
          y2={PAD.top + plotH}
        />
        {[max, max / 2].map((v) => (
          <text key={v} className="chart-tick" x={PAD.left - 6} y={y(v) + 3} textAnchor="end">
            {format(v)}
          </text>
        ))}
        {/* Stacked bands get a 2px surface-coloured stroke so adjacent fills read
            as separate bands rather than one shape with a colour change. */}
        {bands.map((b) => (
          <path
            key={b.key}
            d={b.d}
            fill={`var(--series-${b.slot})`}
            stroke="var(--panel)"
            strokeWidth={2}
            strokeLinejoin="round"
          />
        ))}
        {lines.map((l) => (
          <path
            key={l.key}
            d={l.d}
            fill="none"
            stroke={`var(--series-${l.slot})`}
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        ))}
        {cur != null && (
          <>
            <line
              className={`chart-cursor${cursor?.pinned ? " pinned" : ""}`}
              x1={x(cur)}
              x2={x(cur)}
              y1={PAD.top}
              y2={PAD.top + plotH}
            />
            {series.map((s) =>
              s.points[cur] == null ? null : (
                <circle
                  key={s.key}
                  cx={x(cur)}
                  cy={
                    mode === "stacked"
                      ? y(
                          series
                            .slice(0, series.findIndex((o) => o.key === s.key) + 1)
                            .reduce((a, o) => a + (o.points[cur] ?? 0), 0),
                        )
                      : y(s.points[cur] as number)
                  }
                  r={3}
                  fill={`var(--series-${s.slot})`}
                  stroke="var(--panel)"
                  strokeWidth={2}
                />
              ),
            )}
          </>
        )}
        <text className="chart-tick" x={PAD.left} y={height - 5}>
          {clockLabel(windowStart)}
        </text>
        <text className="chart-tick" x={PAD.left + plotW} y={height - 5} textAnchor="end">
          {clockLabel(windowEnd)}
        </text>
      </svg>

      <ChartLegend
        series={series}
        index={cur}
        pinned={cursor?.pinned ?? false}
        time={cur != null ? times[cur] : null}
        format={format}
      />
    </div>
  );
}

/**
 * Legend and readout in one. Identity is never colour-alone — every series has
 * its name here — and the values double as the light-theme relief the palette's
 * three sub-3:1 slots require.
 */
function ChartLegend(props: {
  series: ChartSeries[];
  index: number | null;
  pinned: boolean;
  time: number | null;
  format: (v: number) => string;
}) {
  const { series, index, format } = props;
  return (
    <div className="chart-legend">
      <span className="chart-legend-when">
        {props.time == null
          ? "hover or use ← → to scrub"
          : `${clockLabel(props.time)}${props.pinned ? " · pinned (click to release)" : ""}`}
      </span>
      {series.map((s) => {
        const v = index == null ? null : s.points[index];
        return (
          <span key={s.key} className="chart-legend-item">
            <span className="chart-swatch" style={{ background: `var(--series-${s.slot})` }} />
            <span className="chart-legend-label">{s.label}</span>
            {/* An absent value is "—", never 0: the metric was not measured. */}
            <span className="chart-legend-value">{v == null ? "—" : format(v)}</span>
          </span>
        );
      })}
    </div>
  );
}

function clockLabel(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
