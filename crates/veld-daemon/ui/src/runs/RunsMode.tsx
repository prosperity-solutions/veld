import { useCallback, useEffect, useMemo, useState } from "react";
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

/**
 * Runs mode — the v1 management dashboard rebuilt on React/Mantine:
 * environments across all projects with Active|History, node health,
 * logs, stats, and sharing. Polls: envs+shares 3s, stats 5s (stats live in
 * separate state so their churn re-renders only the stat cells).
 */
export function RunsMode() {
  const [envs, setEnvs] = useState<EnvironmentList | null>(null);
  const [shares, setShares] = useState<SharesList | null>(null);
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [offline, setOffline] = useState(false);
  const [view, setView] = useState<string>(
    () => window.localStorage.getItem("veld-view") ?? "active",
  );

  const refresh = useCallback(async () => {
    try {
      const [e, s] = await Promise.all([api.environments(), api.shares()]);
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
      <Group gap="sm" px={14} py={8} wrap="wrap">
        {allRuns.length > 0 && (
          <SegmentedControl
            size="xs"
            value={view}
            onChange={setViewPersist}
            data={[
              { value: "active", label: `Active (${liveCount})` },
              { value: "history", label: `History (${endedCount})` },
            ]}
          />
        )}
        <div style={{ flex: 1 }} />
        <span
          className={`dot ${liveCount > 0 || joinCount > 0 ? "running" : "stopped"}`}
          style={{ animation: "none" }}
        />
        <Text size="xs" c="dimmed">
          {meta}
        </Text>
      </Group>

      {(pending.length > 0 || joins.length > 0) && (
        <Stack gap={6} px={14} pb={8}>
          {pending.map((p) => (
            <Group key={p.id} gap="xs" className="share-row pending" p={8} wrap="wrap">
              <Badge size="xs" color="yellow" variant="light">
                join request
              </Badge>
              <Text size="xs">
                <b>{p.label || "(no label)"}</b> wants to join
              </Text>
              <Text size="xs" c="dimmed" ff="monospace">
                {p.share_id} · {p.node_id.slice(0, 10)}
              </Text>
              <div style={{ flex: 1 }} />
              <Button
                size="compact-xs"
                variant="light"
                onClick={() => void api.approveJoin(p.id).then(refresh)}
              >
                Approve
              </Button>
              <Button
                size="compact-xs"
                color="red"
                variant="light"
                onClick={() => void api.denyJoin(p.id).then(refresh)}
              >
                Deny
              </Button>
            </Group>
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
                onClick={() => void api.leaveJoin(j.id).then(refresh)}
              >
                Leave
              </Button>
            </Group>
          ))}
        </Stack>
      )}

      <div className="runs-scroll">
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
        <Stack gap={10} p={14} pt={4}>
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
      </div>
    </div>
  );
}
