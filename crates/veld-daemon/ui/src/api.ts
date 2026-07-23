// Typed client for the daemon's management + desktop APIs (same origin).

export type RunStatus =
  | "starting"
  | "running"
  | "recovering"
  | "stopping"
  | "stopped"
  | "failed";

export interface ActionInfo {
  name: string;
  label: string;
}

export interface NodeInfo {
  name: string;
  variant: string;
  status: string;
  url?: string | null;
  pid?: number | null;
  recovery_count?: number;
  consecutive_failures?: number;
  last_liveness_error?: string | null;
  actions?: ActionInfo[];
}

export interface HistoryNode {
  name: string;
  variant: string;
  status: string;
  exit_code?: number | null;
}

export interface HistoryEntry {
  run_id: string;
  short_id: string;
  status: RunStatus;
  outcome?: string | null;
  created_at: string;
  ended_at?: string | null;
  nodes: HistoryNode[];
}

export interface RunInfo {
  /** Environment name (what `--name` addresses). */
  name: string;
  /** Status of the environment's latest run. */
  status: RunStatus;
  /**
   * Whether the latest run occupies the live slot. History runs (stopped,
   * crashed) keep their status but are not live — never treat them as
   * running, and their URLs are stripped server-side.
   */
  live: boolean;
  run_id: string;
  short_id: string;
  outcome?: string | null;
  ended_at?: string | null;
  urls: Record<string, string>;
  nodes: NodeInfo[];
  history?: HistoryEntry[];
}

export interface ProjectInfo {
  name: string;
  project_root: string;
  runs: RunInfo[];
}

export interface EnvironmentList {
  projects: ProjectInfo[];
}

export interface Worktree {
  id: number;
  repo_root: string;
  path: string;
  branch: string;
  alias: string;
  is_main: boolean;
  created_at: string;
  has_veld_config: boolean;
  presets: string[];
}

export interface Repo {
  root: string;
  name: string;
  created_at: string;
  /** False when the repo can't be listed on disk right now (moved/deleted). */
  available: boolean;
  worktrees: Worktree[];
}

export interface RepoList {
  repos: Repo[];
}

export interface GatewayPublicUrl {
  node: string;
  hostname: string;
  public_url: string;
  access?: string | null;
}

export interface ShareConnectionInfo {
  node_id: string;
  label?: string | null;
  transport: "direct" | "relayed" | "none";
  via?: string | null;
  rtt_ms?: number | null;
}

export interface ShareInfo {
  id: string;
  run: string;
  approve?: "first" | "manual" | "auto" | null;
  nodes: string[];
  urls: string[];
  ticket?: string | null;
  join_url?: string | null;
  joiners: number;
  public_urls: GatewayPublicUrl[];
  web_password?: string | null;
  connections: ShareConnectionInfo[];
}

export interface PendingInfo {
  id: string;
  share_id: string;
  label?: string | null;
  node_id: string;
}

export interface SharesList {
  shares: ShareInfo[];
  joins: ShareInfo[];
  pending: PendingInfo[];
}

export interface NodeLogs {
  node: string;
  variant: string;
  source: string;
  lines: string[];
}

export interface LogResponse {
  nodes: NodeLogs[];
}

export interface NodeStats {
  cpu: number;
  mem: number;
  procs: number;
  spark: number[];
}

/** project_root → run name → "node:variant" → stats */
export interface StatsResponse {
  projects: Record<string, Record<string, Record<string, NodeStats>>>;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const mutating = init?.method && init.method !== "GET";
  const res = await fetch(path, {
    ...init,
    headers: {
      ...(init?.body ? { "Content-Type": "application/json" } : {}),
      // CSRF gate: the daemon rejects mutations without this custom header.
      ...(mutating ? { "X-Veld-Request": "1" } : {}),
      ...init?.headers,
    },
  });
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const body = await res.json();
      if (body && typeof body.error === "string") message = body.error;
    } catch {
      // non-JSON error body — keep the status text
    }
    throw new Error(message);
  }
  if (res.status === 204 || res.status === 202) return undefined as T;
  return (await res.json()) as T;
}

export const api = {
  environments: () => request<EnvironmentList>("/api/environments"),
  /** Pure read (no reconciliation) — kept for consumers that must not
   *  trigger git; the app itself polls refreshRepos. */
  repos: () => request<RepoList>("/api/repos"),
  /**
   * Reconcile worktree rows with git and return the fresh list — the poll
   * target. A POST (CSRF-gated) because it spawns git server-side; debounced
   * by the daemon, so several clients polling stay cheap.
   */
  refreshRepos: () => request<RepoList>("/api/repos/refresh", { method: "POST" }),
  importRepo: (path: string) =>
    request<Repo>("/api/repos/import", {
      method: "POST",
      body: JSON.stringify({ path }),
    }),
  removeRepo: (root: string) =>
    request<void>("/api/repos", {
      method: "DELETE",
      body: JSON.stringify({ root }),
    }),
  createWorktree: (body: {
    repo_root: string;
    branch: string;
    create_branch: boolean;
    alias?: string;
  }) =>
    request<Worktree>("/api/worktrees", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  renameWorktree: (id: number, alias: string) =>
    request<Worktree>(`/api/worktrees/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ alias }),
    }),
  deleteWorktree: (id: number, force: boolean) =>
    request<void>(`/api/worktrees/${id}?force=${force}`, { method: "DELETE" }),
  /**
   * Open the OS folder picker (hosted by the daemon — it runs in the user's
   * GUI session). Resolves to the chosen absolute path, or null on cancel.
   * Throws on: no picker backend (501), backend failure (500), another
   * picker already open (409), or the 10-minute timeout (408).
   */
  pickDirectory: async (): Promise<string | null> => {
    const res = await fetch("/api/pick-directory", {
      method: "POST",
      headers: { "X-Veld-Request": "1" },
    });
    if (res.status === 204) return null;
    if (!res.ok) {
      let message = `${res.status} ${res.statusText}`;
      try {
        const body = await res.json();
        if (body && typeof body.error === "string") message = body.error;
      } catch {
        // keep status text
      }
      throw new Error(message);
    }
    return ((await res.json()) as { path: string }).path;
  },
  startRun: (worktreeId: number, preset: string | null) =>
    request<void>(`/api/worktrees/${worktreeId}/start`, {
      method: "POST",
      body: JSON.stringify(preset ? { preset } : {}),
    }),
  stopRun: (runName: string) =>
    request<void>(`/api/environments/${encodeURIComponent(runName)}/stop`, {
      method: "POST",
    }),
  restartRun: (runName: string) =>
    request<void>(`/api/environments/${encodeURIComponent(runName)}/restart`, {
      method: "POST",
    }),
  runAction: (runName: string, action: string, node?: string) =>
    request<void>(`/api/environments/${encodeURIComponent(runName)}/action`, {
      method: "POST",
      body: JSON.stringify(node ? { action, node } : { action }),
    }),
  openTerminal: (path: string) =>
    request<void>("/api/open-terminal", {
      method: "POST",
      body: JSON.stringify({ path }),
    }),
  stats: () => request<StatsResponse>("/api/stats"),
  logs: (run: string, opts: { source?: string; runId?: string } = {}) => {
    const q = new URLSearchParams({ lines: "500" });
    if (opts.source && opts.source !== "all") q.set("source", opts.source);
    if (opts.runId) q.set("run_id", opts.runId);
    return request<LogResponse>(
      `/api/logs/${encodeURIComponent(run)}?${q.toString()}`,
    );
  },
  shares: () => request<SharesList>("/api/shares"),
  startShare: (run: string, opts: { web?: boolean } = {}) =>
    request<{ join_url?: string }>("/api/shares", {
      method: "POST",
      body: JSON.stringify(
        opts.web ? { run, web: true } : { run, approve: "manual" },
      ),
    }),
  stopShare: (id: string) =>
    request<void>(`/api/shares/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
  setShareMode: (id: string, approve: "auto" | "manual") =>
    request<void>(`/api/shares/${encodeURIComponent(id)}/mode`, {
      method: "POST",
      body: JSON.stringify({ approve }),
    }),
  approveJoin: (requestId: string) =>
    request<void>(`/api/shares/requests/${encodeURIComponent(requestId)}/approve`, {
      method: "POST",
    }),
  denyJoin: (requestId: string) =>
    request<void>(`/api/shares/requests/${encodeURIComponent(requestId)}/deny`, {
      method: "POST",
    }),
  leaveJoin: (joinId: string) =>
    request<void>(`/api/shares/joins/${encodeURIComponent(joinId)}`, {
      method: "DELETE",
    }),
};
