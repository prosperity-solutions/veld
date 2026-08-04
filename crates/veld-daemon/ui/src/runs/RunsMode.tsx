import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Badge,
  Button,
  Group,
  SegmentedControl,
  Stack,
  Text,
} from "@mantine/core";
import {
  api,
  type EnvironmentList,
  type SharesList,
  type StatsResponse,
} from "../api";
import { EnvCard } from "./EnvCard";
import { JoinRequestRow, runOfShare } from "../shared/Sharing";
import { notifyError } from "../shared/notify";
import { confirmedUnattached, unattachedShareIds } from "../shared/util";
import { topbarClass } from "../shell";
import type { ReactNode } from "react";

/**
 * Runs mode — the v1 management dashboard rebuilt on React/Mantine:
 * environments across all projects with Active|History, node health,
 * logs, stats, and sharing. Polls: envs+shares 3s, stats 5s (stats live in
 * separate state so their churn re-renders only the stat cells).
 */
export function RunsMode(props: { modeSwitch: ReactNode; themeButton: ReactNode;
  settingsButton: ReactNode;
}) {
  const [envs, setEnvs] = useState<EnvironmentList | null>(null);
  const [shares, setShares] = useState<SharesList | null>(null);
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [offline, setOffline] = useState(false);
  /** Share ids that looked unattached on the previous poll (see `confirmedUnattached`). */
  const prevUnattached = useRef<Set<string>>(new Set());
  /** Shares confirmed unattached across two polls — the ones safe to render. */
  const [orphanIds, setOrphanIds] = useState<Set<string>>(new Set());
  const [view, setView] = useState<string>(
    () => window.localStorage.getItem("veld-view") ?? "active",
  );

  const refresh = useCallback(async () => {
    try {
      const [e, s] = await Promise.all([api.environments(), api.shares()]);
      // Advance the orphan-share gate here, once per poll — see
      // `confirmedUnattached`. Doing it in a render effect instead would let an
      // unrelated re-render (the 5s stats tick) confirm one poll against itself.
      const known = new Set(
        e.projects.flatMap((p) => p.runs.map((r) => r.run_id)),
      );
      const nowUnattached = unattachedShareIds(s.shares, known);
      setOrphanIds(confirmedUnattached(nowUnattached, prevUnattached.current));
      prevUnattached.current = nowUnattached;
      setEnvs(e);
      setShares(s);
      setOffline(false);
    } catch {
      setOffline(true);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const t = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(t);
  }, [refresh]);

  useEffect(() => {
    const tick = async () => {
      try {
        setStats(await api.stats());
      } catch {
        // keep last stats
      }
    };
    void tick();
    const t = window.setInterval(() => void tick(), 5000);
    return () => window.clearInterval(t);
  }, []);

  /** A share mutation from this panel: report a failure, then re-poll. Without
   *  this an unshare that the daemon refused vanished into an unhandled rejection. */
  const shareAction = async (context: string, fn: () => Promise<unknown>) => {
    try {
      await fn();
    } catch (e) {
      notifyError(context, e);
    }
    await refresh();
  };

  const setViewPersist = (v: string) => {
    setView(v);
    window.localStorage.setItem("veld-view", v);
  };

  const projects = useMemo(() => envs?.projects ?? [], [envs]);
  const allRuns = projects.flatMap((p) => p.runs.map((r) => ({ p, r })));
  const liveCount = allRuns.filter(({ r }) => r.live).length;
  const endedCount = allRuns.length - liveCount;
  const joinCount = shares?.joins.length ?? 0;
  const pending = shares?.pending ?? [];
  const joins = shares?.joins ?? [];

  // Hosted shares whose run this dashboard doesn't list (#171) — the set is
  // decided per poll in `refresh` (see `unattachedShareIds` for why the run set
  // is every known run and not the filtered `shown` list).
  const orphanShares = (shares?.shares ?? []).filter((s) => orphanIds.has(s.id));

  const shown = allRuns
    .filter(({ r }) => (view === "active" ? r.live : !r.live))
    .sort(
      (a, b) =>
        Number(b.r.status === "running") - Number(a.r.status === "running") ||
        a.r.name.localeCompare(b.r.name),
    );

  const meta = offline
    ? "disconnected"
    : envs === null
      ? "connecting…"
      : liveCount > 0
        ? `${liveCount} running`
        : joinCount > 0
          ? `${joinCount} joined`
          : allRuns.length > 0
            ? "all stopped"
            : "no environments";

  return (
    <div className="runs-mode">
      <div className={topbarClass}>
        {props.modeSwitch}
        {allRuns.length > 0 && (
          <div className="topbar-center">
            <SegmentedControl
              size="xs"
              styles={{
                root: {
                  background: "var(--bg)",
                  border: "1px solid var(--border2)",
                },
              }}
              value={view}
              onChange={setViewPersist}
              data={[
                { value: "active", label: `Active (${liveCount})` },
                { value: "history", label: `History (${endedCount})` },
              ]}
            />
          </div>
        )}
        <div style={{ flex: 1 }} />
        <span
          className={`dot ${liveCount > 0 || joinCount > 0 ? "running" : "stopped"}`}
          style={{ animation: "none" }}
        />
        <Text size="xs" c="dimmed">
          {meta}
        </Text>
        {props.settingsButton}
        {props.themeButton}
      </div>

      <div className="runs-container" style={{ flex: "none" }}>
      {(pending.length > 0 || joins.length > 0 || orphanShares.length > 0) && (
        <Stack gap={6} px={14} pb={8}>
          {orphanShares.map((s) => (
            <Group key={s.id} gap="xs" className="share-row pending" p={8} wrap="wrap">
              <Badge size="xs" color="orange" variant="light">
                {s.public_urls.length > 0 ? "public web share" : "share"} without a run
              </Badge>
              <Text size="xs">
                run <b>{s.run || "(unknown)"}</b> is gone
              </Text>
              {s.public_urls.map((u) => (
                <a
                  key={u.node}
                  href={u.public_url}
                  target="_blank"
                  rel="noreferrer"
                  className="node-url"
                >
                  {u.public_url}
                </a>
              ))}
              <Text size="xs" c="dimmed" ff="monospace">
                {s.id}
              </Text>
              <div style={{ flex: 1 }} />
              <Button
                size="compact-xs"
                color="red"
                variant="light"
                onClick={() => void shareAction("Unshare", () => api.stopShare(s.id))}
              >
                Unshare
              </Button>
            </Group>
          ))}
          {pending.map((p) => (
            <JoinRequestRow
              key={p.id}
              pending={p}
              runLabel={runOfShare(shares?.shares ?? [], p.share_id)}
              onChanged={() => void refresh()}
            />
          ))}
          {joins.map((j) => (
            <Group key={j.id} gap="xs" className="share-row" p={8} wrap="wrap">
              <Badge size="xs" color="green" variant="light">
                joined
              </Badge>
              {j.urls.map((u) => (
                <a key={u} href={u} target="_blank" rel="noreferrer" className="node-url">
                  {u}
                </a>
              ))}
              <Text size="xs" c="dimmed" ff="monospace">
                {j.id}
              </Text>
              <div style={{ flex: 1 }} />
              <Button
                size="compact-xs"
                color="red"
                variant="subtle"
                onClick={() => void shareAction("Leave", () => api.leaveJoin(j.id))}
              >
                Leave
              </Button>
            </Group>
          ))}
        </Stack>
      )}

      </div>

      <div className="runs-scroll">
        <div className="runs-container">
        {envs !== null && projects.length === 0 && (
          <div className="center-page">
            <Text fw={600}>No environments yet</Text>
            <Text size="sm" c="dimmed">
              {joinCount > 0
                ? `You've joined ${joinCount} shared environment(s) — see the panel above.`
                : "Start one with `veld start` in any project with a veld.json."}
            </Text>
          </div>
        )}
        {envs !== null && projects.length > 0 && shown.length === 0 && (
          <div className="center-page">
            <Text fw={600}>
              {view === "active" ? "Nothing running" : "No ended environments"}
            </Text>
            <Text size="sm" c="dimmed">
              {view === "active"
                ? `${endedCount} ended environment(s) in History.`
                : "Stopped and crashed environments land here (kept for 7 days)."}
            </Text>
          </div>
        )}
        {shown.length > 0 && (
        <Stack gap={10} p={14} pt={14}>
          {shown.map(({ p, r }) => (
            <EnvCard
              key={`${p.project_root}::${r.name}`}
              project={p}
              run={r}
              shares={shares?.shares ?? []}
              stats={stats?.projects?.[p.project_root]?.[r.name]}
              onChanged={() => void refresh()}
            />
          ))}
        </Stack>
        )}
        </div>
      </div>
    </div>
  );
}
