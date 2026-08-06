import React, { useEffect, useMemo, useRef, useState } from "react";
import {
  Button,
  Group,
  NativeSelect,
  NumberInput,
  SegmentedControl,
  Text,
  TextInput,
  useComputedColorScheme,
} from "@mantine/core";
import { api, type HistoryEntry, type LogResponse, type RunRef } from "../api";
import { extractMsg, extractTs, fmtTs, fmtTsFull, fmtWhen, nodeColor } from "./util";
import type { LogTimeZone } from "./settings";
import { ansiCss, markAnsiSpans, parseAnsi, type AnsiSpan } from "./ansi";

interface Entry {
  node: string;
  variant: string;
  source: string;
  ts: string;
  /** The message as escape-free text — what is on screen, and therefore what
   *  search matches against. */
  msg: string;
  /** The same message as styled runs. Parsed once here rather than per render:
   *  the poll is every 2s and a search keystroke re-renders every row. */
  spans: AnsiSpan[];
}

/**
 * Per-run log viewer: run picker over history, node filter (client-side),
 * source filter (server-side), search with ±N context lines and <mark>
 * highlighting, auto-scroll that disables when the user scrolls up and re-arms
 * near the bottom.
 *
 * Two hosts, one implementation: an environment card's Logs tab in runs mode
 * (`fill` off — a fixed-height area inside a scrolling card, kept mounted while
 * hidden so filters and scroll survive a tab switch) and a `logs` pane in IDE
 * mode (`fill` on — it takes the whole dock body and is unmounted when its tab
 * is not the active one, like every other pane).
 */
export function LogsPanel(props: {
  /** Project-scoped run address — a bare name is ambiguous across repos. */
  run: RunRef;
  history: HistoryEntry[];
  /** Card's history selection — scopes the default run picker option. */
  histSel: string | null;
  visible: boolean;
  /** Fill the parent (a pane) instead of sitting at a fixed height in a card. */
  fill?: boolean;
  /**
   * Which zone to render each line's timestamp in (`logs.timeZone`).
   *
   * Required rather than defaulted, so a host that forgets it is a type error rather
   * than a panel that silently disagrees with the setting the other host honours.
   * Passed down from the app's single `useSettings` like every other preference here.
   */
  tz: LogTimeZone;
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

  // Reset the accumulated node list when the viewed run changes — otherwise
  // the node filter (and the multi-node tag heuristic) carries nodes from
  // previously viewed runs.
  // Depend on the RunRef's FIELDS, not the object: `runRef()` mints a fresh
  // object on every parent render, so `[props.run]` would re-run this (and the
  // poll effect below) on every 3s environments poll and every 5s stats poll —
  // clearing the node filter and tearing down the 2s log interval each time.
  useEffect(() => {
    knownNodes.current.clear();
  }, [props.run.name, props.run.projectRoot, effectiveRunId]);

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
    // Scalar deps only — see the note on the effect above.
  }, [
    props.visible,
    props.run.name,
    props.run.projectRoot,
    sourceFilter,
    effectiveRunId,
  ]);

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
      // `_veld:internal` is the run's liveness stream (probes, recoveries —
      // daemon noise, not any node's output), so a node pick hides it — same
      // rule the daemon and `veld logs` follow. `_veld:setup`/`teardown` are
      // a node's real step output and stay under the old rule: kept even with
      // a node picked (filtering them out blanked the Setup view).
      const runLevel = n.node === "_veld";
      if (runLevel && n.variant === "internal" && nodeFilter) continue;
      if (nodeFilter && !runLevel && `${n.node}:${n.variant}` !== nodeFilter) continue;
      for (const raw of n.lines) {
        // The CLI colours its own output and dev servers colour far more, so a
        // line arrives with SGR sequences in it and, for progress output,
        // carriage returns. `parseAnsi` is the one place that decides what the
        // line *says*; `msg` is that text and never the raw bytes, or a search
        // for a word would miss it whenever a colour change sat inside it.
        const spans = parseAnsi(extractMsg(raw));
        entries.push({
          node: n.node,
          variant: n.variant,
          source: n.source || "server",
          ts: extractTs(raw),
          msg: spans.map((s) => s.text).join(""),
          spans,
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
  // The 16 ANSI slots are fixed colours, not design tokens, so they need to be
  // resolved per theme rather than left to CSS variables. `forceColorScheme` on
  // the provider is driven by the app's own toggle, so this tracks it.
  const scheme = useComputedColorScheme("dark");

  /** One message: its colour runs, with the search term marked inside them. */
  const message = (e: Entry) => {
    const pieces = markAnsiSpans(e.spans, term);
    // The common line has no styling and no match: render the string, so the
    // overwhelmingly common case adds no elements at all.
    if (pieces.length === 1 && !pieces[0].mark && Object.keys(pieces[0].style).length === 0) {
      return pieces[0].text;
    }
    return pieces.map((p, i) => {
      const css = ansiCss(p.style, scheme);
      const text = p.mark ? <mark>{p.text}</mark> : p.text;
      // A styled piece needs its own span; an unstyled one does not, and a
      // fragment keeps the row's DOM as small as the line deserves.
      return Object.keys(css).length === 0 ? (
        <React.Fragment key={i}>{text}</React.Fragment>
      ) : (
        <span key={i} style={css}>
          {text}
        </span>
      );
    });
  };

  return (
    // Display comes from the class so the two hosts can lay out differently
    // (block in a card, a flex column in a pane); the inline rule only hides.
    <div
      className={props.fill ? "logs-fill" : "logs-card"}
      style={props.visible ? undefined : { display: "none" }}
    >
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 6,
          padding: "6px 10px",
        }}
      >
        {/* Row 1 — which run, node and stream. Stable: never moves when a
            search starts. */}
        <Group gap="xs" wrap="wrap" align="center">
          {/* Which environment these lines belong to — pane mode only, since a
              card already has the name in its header.

              `NodesView` carries this and `LogsView` did not, which was survivable
              while a worktree had one environment and is not now: the control beside
              it is a *history* picker ("Latest run", "All runs") for the environment
              already bound, so a reader with two live runs would take that dropdown
              for the answer to "which run is this?" and be wrong. The title below
              says `Run history` for the same reason. */}
          {props.fill && (
            <Text size="xs" ff="monospace" fw={700} title="The environment these logs belong to">
              {props.run.name}
            </Text>
          )}
          <NativeSelect
            size="xs"
            title="Run history"
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
              { value: "setup", label: "Setup" },
              { value: "internal", label: "Internal" },
            ]}
          />
        </Group>
        {/* Row 2 — search and its affordances. Kept on their own row/group so
            the auto-scroll toggle and the context control never relocate or wrap
            onto orphaned lines when a search term appears: that reflow was making
            them hard to reach mid-debug. */}
        <Group gap="xs" wrap="wrap" align="center">
          <TextInput
            size="xs"
            placeholder="Search…"
            value={search}
            onChange={(e) => setSearch(e.currentTarget.value)}
            style={{ width: 180 }}
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
      </div>
      <div
        className={`log-area${props.fill ? " fill" : ""}`}
        ref={areaRef}
        onScroll={onScroll}
      >
        {data === null && <div className="log-empty">Loading logs…</div>}
        {data !== null && rows.length === 0 && (
          <div className="log-empty">{term ? "No matching lines" : "No log output yet"}</div>
        )}
        {rows.map(({ e, dim, gap }, i) => (
          <div key={i}>
            {gap && <div className="log-ctx-sep">···</div>}
            <div className={`log-line${dim ? " ctx" : ""}${e.msg.startsWith("[VELD]") ? " ann" : ""}`}>
              {e.ts && (
                <span className="ts" title={fmtTsFull(e.ts, props.tz)}>
                  {fmtTs(e.ts, props.tz)}
                </span>
              )}
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
              <span className="msg">{message(e)}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
