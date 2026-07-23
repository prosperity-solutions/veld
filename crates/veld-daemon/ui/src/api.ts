// Typed client for the daemon's management + desktop APIs (same origin).

export type RunStatus =
  | "starting"
  | "running"
  | "recovering"
  | "stopping"
  | "stopped"
  | "failed";

export interface NodeInfo {
  name: string;
  variant: string;
  status: string;
  url?: string | null;
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
  urls: Record<string, string>;
  nodes: NodeInfo[];
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
  worktrees: Worktree[];
}

export interface RepoList {
  repos: Repo[];
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
  repos: () => request<RepoList>("/api/repos"),
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
};
