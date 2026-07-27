// Pure helpers ported from the v1 dashboard (assets/management-ui.html).

/** Compound card key — run names collide across projects. */
export function runKey(projectRoot: string, run: string): string {
  return `${projectRoot}::${run}`;
}

export function fmtBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** Local HH:MM:SS.mmm for a log timestamp. */
export function fmtTs(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
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
