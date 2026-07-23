import { useEffect, useMemo, useRef, useState } from "react";
import {
  Button,
  Group,
  NativeSelect,
  NumberInput,
  SegmentedControl,
  Text,
  TextInput,
} from "@mantine/core";
import { api, type HistoryEntry, type LogResponse } from "../api";
import { extractMsg, extractTs, fmtTs, fmtWhen, nodeColor } from "./util";

interface Entry {
  node: string;
  variant: string;
  source: string;
  ts: string;
  msg: string;
}

/**
 * Per-run log viewer (v1 logs tab): run picker over history, node filter
 * (client-side), source filter (server-side), search with ±N context lines
 * and <mark> highlighting, auto-scroll that disables when the user scrolls
 * up and re-arms near the bottom. The component stays mounted while its tab
 * is hidden so filters/scroll/cache survive tab switches.
 */
export function LogsPanel(props: {
  run: string;
  history: HistoryEntry[];
  /** Card's history selection — scopes the default run picker option. */
  histSel: string | null;
  visible: boolean;
}) {
  const [runFilter, setRunFilter] = useState<string>("");
  const [nodeFilter, setNodeFilter] = useState<string>("");
  const [sourceFilter, setSourceFilter] = useState<string>("all");
  const [search, setSearch] = useState("");
  const [ctxLines, setCtxLines] = useState(5);
  const [autoScroll, setAutoScroll] = useState(true);
  const [data, setData] = useState<LogResponse | null>(null);
  const areaRef = useRef<HTMLDivElement>(null);
  const knownNodes = useRef(new Map<string, string>()); // key → label
  const colorOrder = useRef(new Map<string, number>());

  // The card's history selection scopes which run "latest" means.
  const effectiveRunId = runFilter || props.histSel || "";

  useEffect(() => {
    if (!props.visible) return;
    let cancelled = false;
    const fetchLogs = async () => {
      try {
        const d = await api.logs(props.run, {
          source: sourceFilter,
          runId: effectiveRunId || undefined,
        });
        if (!cancelled) {
          for (const n of d.nodes) {
            const key = `${n.node}:${n.variant}`;
            knownNodes.current.set(key, key);
          }
          setData(d);
        }
      } catch {
        // poll again; transient errors keep the last view
      }
    };
    void fetchLogs();
    const t = window.setInterval(() => void fetchLogs(), 2000);
    return () => {
      cancelled = true;
      window.clearInterval(t);
    };
  }, [props.visible, props.run, sourceFilter, effectiveRunId]);

  // Auto-scroll management: stick to bottom while armed; manual scroll-up
  // disarms; returning within 40px of the bottom re-arms (v1 behavior).
  useEffect(() => {
    const el = areaRef.current;
    if (el && autoScroll) el.scrollTop = el.scrollHeight;
  });
  const onScroll = () => {
    const el = areaRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    if (nearBottom !== autoScroll) setAutoScroll(nearBottom);
  };

  const { rows, matchCount } = useMemo(() => {
    const entries: Entry[] = [];
    for (const n of data?.nodes ?? []) {
      if (nodeFilter && `${n.node}:${n.variant}` !== nodeFilter) continue;
      for (const raw of n.lines) {
        entries.push({
          node: n.node,
          variant: n.variant,
          source: n.source || "server",
          ts: extractTs(raw),
          msg: extractMsg(raw),
        });
      }
    }
    entries.sort((a, b) => a.ts.localeCompare(b.ts) || a.node.localeCompare(b.node));

    const term = search.trim();
    if (!term) {
      return { rows: entries.map((e) => ({ e, dim: false, gap: false })), matchCount: 0 };
    }
    const re = new RegExp(term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "i");
    const matchIdx = new Set<number>();
    entries.forEach((e, i) => {
      if (re.test(e.msg) || re.test(e.node)) matchIdx.add(i);
    });
    const keep = new Set<number>();
    for (const i of matchIdx) {
      for (let j = Math.max(0, i - ctxLines); j <= Math.min(entries.length - 1, i + ctxLines); j++) {
        keep.add(j);
      }
    }
    const rows: Array<{ e: Entry; dim: boolean; gap: boolean }> = [];
    let prev = -1;
    for (const i of [...keep].sort((a, b) => a - b)) {
      rows.push({ e: entries[i], dim: !matchIdx.has(i), gap: prev !== -1 && i > prev + 1 });
      prev = i;
    }
    return { rows, matchCount: matchIdx.size };
  }, [data, nodeFilter, search, ctxLines]);

  const multi = knownNodes.current.size > 1;
  const term = search.trim();
  const highlight = (text: string) => {
    if (!term) return text;
    const re = new RegExp(`(${term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, "ig");
    const parts = text.split(re);
    return parts.map((p, i) => (i % 2 === 1 ? <mark key={i}>{p}</mark> : p));
  };

  return (
    <div style={{ display: props.visible ? "block" : "none" }}>
      <Group gap="xs" px={10} py={6} wrap="wrap">
        <NativeSelect
          size="xs"
          title="Run"
          value={runFilter}
          onChange={(e) => {
            setRunFilter(e.currentTarget.value);
            setAutoScroll(true);
            setData(null);
          }}
          data={[
            { value: "", label: "Latest run" },
            ...props.history.map((h) => ({
              value: h.run_id,
              label: `${h.outcome || h.status} · ${fmtWhen(h.created_at)}`,
            })),
            { value: "all", label: "All runs" },
          ]}
        />
        <NativeSelect
          size="xs"
          title="Node"
          value={nodeFilter}
          onChange={(e) => setNodeFilter(e.currentTarget.value)}
          data={[
            { value: "", label: "All nodes" },
            ...[...knownNodes.current.keys()].sort().map((k) => ({ value: k, label: k })),
          ]}
        />
        <SegmentedControl
          size="xs"
          value={sourceFilter}
          onChange={(v) => {
            setSourceFilter(v);
            setData(null);
          }}
          data={[
            { value: "all", label: "All" },
            { value: "server", label: "Server" },
            { value: "client", label: "Client" },
            { value: "internal", label: "Internal" },
          ]}
        />
        <TextInput
          size="xs"
          placeholder="Search…"
          value={search}
          onChange={(e) => setSearch(e.currentTarget.value)}
          style={{ width: 150 }}
        />
        {term && (
          <>
            <Text size="xs" c="dimmed">
              {matchCount} matches
            </Text>
            <NumberInput
              size="xs"
              title="Context lines"
              value={ctxLines}
              onChange={(v) => setCtxLines(Math.max(0, Math.min(50, Number(v) || 0)))}
              min={0}
              max={50}
              style={{ width: 70 }}
            />
          </>
        )}
        <Button
          size="compact-xs"
          variant={autoScroll ? "light" : "default"}
          onClick={() => setAutoScroll((v) => !v)}
        >
          Auto-scroll {autoScroll ? "ON" : "OFF"}
        </Button>
      </Group>
      <div className="log-area" ref={areaRef} onScroll={onScroll}>
        {data === null && <div className="log-empty">Loading logs…</div>}
        {data !== null && rows.length === 0 && (
          <div className="log-empty">{term ? "No matching lines" : "No log output yet"}</div>
        )}
        {rows.map(({ e, dim, gap }, i) => (
          <div key={i}>
            {gap && <div className="log-ctx-sep">···</div>}
            <div className={`log-line${dim ? " ctx" : ""}${e.msg.startsWith("[VELD]") ? " ann" : ""}`}>
              {e.ts && <span className="ts">{fmtTs(e.ts)}</span>}
              {multi && (
                <span
                  className="node-tag"
                  style={{ color: nodeColor(e.node, colorOrder.current) }}
                >
                  {e.node}:{e.variant}
                  {e.source === "client" ? ":client" : ""}
                </span>
              )}
              {!multi && e.source === "client" && <span className="node-tag">client</span>}
              <span className="msg">{highlight(e.msg)}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
