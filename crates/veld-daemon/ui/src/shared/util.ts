// Pure helpers ported from the v1 dashboard (assets/management-ui.html).

import type { LogTimeZone } from "./settings";

/** Compound card key — run names collide across projects. */
export function runKey(projectRoot: string, run: string): string {
  return `${projectRoot}::${run}`;
}

/**
 * Ids of hosted shares that belong to no known run (#171).
 *
 * A share is attached to its run card by `run_id`, so one whose run is gone — a
 * crash the GC pass hasn't swept yet, possibly a public web share with a live
 * URL — has no card to live on and would be invisible and un-stoppable.
 *
 * `knownRunIds` must cover EVERY run the dashboard knows about, not just the
 * ones currently rendered: filtered to the History view, every live run's share
 * would otherwise look orphaned. `null` means the environment list hasn't loaded
 * yet and yields nothing, so a slow first fetch can't flash every share here.
 */
export function unattachedShareIds<T extends { id: string; run_id?: string | null }>(
  shares: T[],
  knownRunIds: Set<string> | null,
): Set<string> {
  if (knownRunIds === null) return new Set();
  return new Set(
    shares.filter((s) => !s.run_id || !knownRunIds.has(s.run_id)).map((s) => s.id),
  );
}

/**
 * Share ids unattached on this poll AND on the previous one — the set safe to
 * render as "without a run".
 *
 * The runs and the shares arrive from two requests, so a share minted between
 * them — or one whose run appeared just after the environment list was read —
 * looks unattached for a single poll. Rendering that immediately would put a
 * destructive Unshare button next to a live share, so it has to hold across two
 * observations.
 *
 * Both arguments must come from *polls*, never from renders: advancing the
 * previous set once per render would let an unrelated re-render (a stats tick)
 * confirm a single poll against itself.
 */
export function confirmedUnattached(
  now: ReadonlySet<string>,
  prev: ReadonlySet<string>,
): Set<string> {
  return new Set([...now].filter((id) => prev.has(id)));
}

export function fmtBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

const pad = (n: number, w = 2) => String(n).padStart(w, "0");

/**
 * `HH:MM:SS.mmm` for a log timestamp, in `tz`.
 *
 * A log row is dense and repetitive, so the visible form stays time-of-day only —
 * the date and the zone live in the tooltip ([`fmtTsFull`]), which is the only place
 * a reader ever needs them and the only place there is room.
 *
 * `tz` is **required and has no default**, for the same reason `LogsPanel`'s prop is:
 * a defaulted zone here would let the next caller silently render local whatever
 * `logs.timeZone` says, with nothing failing to compile. Defaulting at the leaf would
 * reopen exactly the hole the required prop closes one layer up.
 */
export function fmtTs(iso: string, tz: LogTimeZone): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const [h, m, s, ms] =
    tz === "utc"
      ? [
          d.getUTCHours(),
          d.getUTCMinutes(),
          d.getUTCSeconds(),
          d.getUTCMilliseconds(),
        ]
      : [d.getHours(), d.getMinutes(), d.getSeconds(), d.getMilliseconds()];
  return `${pad(h)}:${pad(m)}:${pad(s)}.${pad(ms, 3)}`;
}

/**
 * `+02:00` / `-05:30` / `+00:00` for the browser's offset at this instant.
 *
 * Always signed and always two fields, including at zero: it is interpolated after
 * the literal `UTC`, and a bare `Z` there would read as `UTCZ`.
 */
function offsetLabel(d: Date): string {
  // `getTimezoneOffset` counts minutes the local zone is *behind* UTC, so the sign
  // is the opposite of the one an ISO offset carries. Read at `d`, not at now, so a
  // line from before a DST change reports the offset that was in force then.
  const mins = -d.getTimezoneOffset();
  const sign = mins < 0 ? "-" : "+";
  const abs = Math.abs(mins);
  return `${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}

/**
 * The unambiguous form of a log timestamp, for a `title` tooltip: the rendered zone
 * with its date and offset, the same instant in the other zone, and the exact stored
 * value on a third line.
 *
 * Each line earns its place. The row itself shows neither a date nor a zone, so
 * `09:12:33.123` answers neither "which day" — real once a history run or *All runs*
 * is selected — nor "whose clock", which is the question this whole setting is about.
 *
 * The stored line is not redundant with the UTC one: `new Date` holds **milliseconds**,
 * so both rendered lines truncate the microseconds veld actually stored, and two rows
 * 200 µs apart would tooltip identically. The CLI keeps that precision
 * (`logging::format_ts`), so without this line `/ide` would show `.123` where
 * `veld logs --utc` shows `.123456`, and a reader correlating the two — or pasting a
 * timestamp into a bug report — would be working from a rounded value.
 */
export function fmtTsFull(iso: string, tz: LogTimeZone): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const date = (utc: boolean) =>
    utc
      ? `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`
      : `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const local = `${date(false)} ${fmtTs(iso, "local")} (local, UTC${offsetLabel(d)})`;
  const utc = `${date(true)} ${fmtTs(iso, "utc")} (UTC)`;
  // Rendered zone first: the tooltip's job is to expand the number on screen, and
  // only then to offer the counterpart.
  const both = tz === "utc" ? `${utc}\n${local}` : `${local}\n${utc}`;
  return `${both}\nstored: ${iso}`;
}

/** Relative "when" for history entries / outcome lines. */
export function fmtWhen(iso?: string | null): string {
  if (!iso) return "";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const mins = Math.round((Date.now() - t) / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

export function shortUrl(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
}

/** Badge / dot color bucket for a run or node status string. */
export function statusBucket(
  status: string,
): "green" | "yellow" | "red" | "dim" {
  switch (status) {
    case "running":
    case "healthy":
      return "green";
    case "failed":
    case "crashed":
      return "red";
    case "stopped":
    case "skipped":
      return "dim";
    default:
      // starting, stopping, health_checking, pending, unhealthy, recovering
      return "yellow";
  }
}

export function bucketColor(bucket: ReturnType<typeof statusBucket>): string {
  switch (bucket) {
    case "green":
      return "var(--live)";
    case "red":
      return "var(--danger)";
    case "yellow":
      return "var(--warn)";
    case "dim":
      return "var(--faint)";
  }
}

/** Stable 8-color cycle for log node tags (v1 `nc()`). */
const NODE_COLORS = [
  "#6c8cff",
  "#3dd68c",
  "#f0c040",
  "#f06060",
  "#c084fc",
  "#22d3ee",
  "#fb923c",
  "#f472b6",
];
export function nodeColor(name: string, order: Map<string, number>): string {
  if (!order.has(name)) order.set(name, order.size);
  return NODE_COLORS[order.get(name)! % NODE_COLORS.length];
}

/** Extract the leading `[ISO8601]` timestamp a log line carries server-side. */
export function extractTs(line: string): string {
  const m = /^\[([^\]]+)\]/.exec(line);
  return m ? m[1] : "";
}

export function extractMsg(line: string): string {
  return line.replace(/^\[[^\]]+\]\s?/, "");
}
