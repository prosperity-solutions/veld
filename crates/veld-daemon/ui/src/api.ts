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
  /** Serialized as a string by the backend ("Exit code where observable"). */
  exit_code?: string | null;
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
  /**
   * One-glyph identifier from a curated animal set. Unique *when assigned*
   * — the picker lets the user choose a glyph another worktree already holds,
   * so never treat it as an identity: several worktrees can share one. (An
   * emoji-keyed index is fine as long as its values are collections.)
   */
  emoji: string;
  is_main: boolean;
  created_at: string;
  has_veld_config: boolean;
  /** Presets in display order, as resolved by the daemon. */
  presets: Preset[];
  /** Startable nodes (hidden excluded) for custom selections. */
  nodes: NodeOption[];
}

/**
 * A preset with its key assigned. Mirrors `veld_core::presets::ResolvedPreset`.
 *
 * `key` is the stable number the CLI picker shows for the same preset — it is
 * assigned by the daemon, not by this list's position, so the two surfaces
 * cannot disagree about which number means which preset.
 */
export interface Preset {
  name: string;
  key: number;
  /** Whether `key` was pinned in veld.json (an unpinned key can move). */
  pinned: boolean;
  label?: string | null;
  when_to_use?: string | null;
  group?: string | null;
  selections: string[];
  /** Whether this is the project's `default_preset`. */
  is_default: boolean;
}

/** A worktree holding a given emoji — id, because aliases repeat across repos. */
export interface EmojiHolder {
  id: number;
  alias: string;
}

export interface NodeOption {
  name: string;
  variants: string[];
  default_variant?: string | null;
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

/**
 * A share, **after normalisation** — see {@link normalizeShare}.
 *
 * The arrays are declared as always-present because that is what consumers get,
 * not because that is what the daemon sends: `veld_core::share::ShareInfo` marks
 * `public_urls` and `connections` `skip_serializing_if = "Vec::is_empty"`, so a
 * peer share with no joiners arrives with neither key. Reading `.length` off those
 * is a TypeError that takes the whole view down, so the client fills them in once,
 * at the boundary, instead of asking every call site to remember.
 */
export interface ShareInfo {
  id: string;
  /** Display only — attach a share to its run via {@link ShareInfo.run_id}. */
  run: string;
  /** Run instance the share was minted from; absent on joins. */
  run_id?: string | null;
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

/**
 * Fill in the arrays the daemon omits when they are empty.
 *
 * `joiners` gets the same treatment: it is `#[serde(default)]` on a `usize`, so a
 * `0` is serialised, but a payload from an older daemon that predates the field
 * would otherwise render "undefined connected".
 */
export function normalizeShare(s: ShareInfo): ShareInfo {
  return {
    ...s,
    nodes: s.nodes ?? [],
    urls: s.urls ?? [],
    joiners: s.joiners ?? 0,
    public_urls: s.public_urls ?? [],
    connections: s.connections ?? [],
  };
}

/** The share list with every entry normalised. */
export function normalizeShares(list: SharesList): SharesList {
  return {
    shares: (list.shares ?? []).map(normalizeShare),
    joins: (list.joins ?? []).map(normalizeShare),
    pending: list.pending ?? [],
  };
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

/** One-shot credential for opening a terminal WebSocket. */
export interface PtyTicket {
  ticket: string;
  expires_in_ms: number;
  /** True when a live session with this id was waiting — i.e. the shell
   *  survived whatever disconnected us, and attaching resumes it. */
  resumed: boolean;
}

/**
 * A run address. **The name alone is not one.**
 *
 * Environments are unique per project, not globally: two repos both checked
 * out on `main` each get an environment called `main` (the desktop start
 * endpoint derives the run name from the worktree alias, and aliases are only
 * de-duplicated within one repo). The daemon used to resolve a bare name
 * against every project and take the first hit, so stopping one repo's `main`
 * could tear down another's — it now requires the project and 404s on a
 * mismatch. Every run-addressed call takes this pair.
 *
 * `projectRoot` is the `project_root` of the `/api/environments` project the
 * run was read from — for a desktop worktree, its checkout path (see
 * `runsForWorktree`).
 */
export interface RunRef {
  name: string;
  projectRoot: string;
}

/** `{ name, projectRoot }` for a run listed under a project. */
export function runRef(projectRoot: string, run: { name: string }): RunRef {
  return { name: run.name, projectRoot };
}

/** Path segment for a run's name. */
export const runPath = (run: RunRef) => encodeURIComponent(run.name);

/** The `project_root=…` query string every run-addressed call must carry. */
export const runScope = (run: RunRef) =>
  new URLSearchParams({ project_root: run.projectRoot }).toString();

/**
 * The most useful message a failed response carries.
 *
 * **The daemon has two error shapes**, and reading only one of them threw the
 * actionable half away: the management and desktop routers answer with
 * `{"error": "…"}`, while the share router returns a bare `text/plain` body
 * (`(StatusCode, String)` in `crates/veld-daemon/src/share/api.rs`). Sharing's
 * refusals are exactly the ones worth reading — "run 'x' has no services opted
 * into peer sharing. Add `"share": { "expose": ["peer"] }` …" is the whole fix —
 * and they surfaced as `400 Bad Request`, which reads as a bug in Veld rather than
 * as a config that has not opted in.
 *
 * Read as text once (a body can only be consumed once), then try JSON on it.
 * Anything that looks like a page rather than a message falls back to the status,
 * since a proxy's HTML error page is not something to paste into a toast.
 */
export async function errorMessage(res: Response): Promise<string> {
  const status = `${res.status} ${res.statusText}`.trim();
  let raw = "";
  try {
    raw = (await res.text()).trim();
  } catch {
    return status;
  }
  if (!raw || raw.startsWith("<")) return status;
  try {
    const body = JSON.parse(raw);
    if (body && typeof body.error === "string" && body.error) return body.error;
  } catch {
    // Not JSON: the plain-text shape below.
  }
  // Long enough for the daemon's multi-sentence refusals, capped so a runaway
  // body can't become the whole toast.
  return raw.length > 600 ? `${raw.slice(0, 600)}…` : raw;
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
  if (!res.ok) throw new Error(await errorMessage(res));
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
  /** Partial update — send an alias, an emoji, or both. */
  patchWorktree: (id: number, patch: { alias?: string; emoji?: string }) =>
    request<Worktree>(`/api/worktrees/${id}`, {
      method: "PATCH",
      body: JSON.stringify(patch),
    }),
  /**
   * The glyphs the emoji picker may offer. Served by the daemon rather than
   * duplicated here so the picker and the server-side allowlist can't drift.
   */
  worktreeEmoji: () =>
    request<{ emoji: string[] }>("/api/worktree-emoji"),
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
    if (!res.ok) throw new Error(await errorMessage(res));
    return ((await res.json()) as { path: string }).path;
  },
  startRun: (
    worktreeId: number,
    start: { preset?: string; selections?: string[] },
  ) =>
    request<void>(`/api/worktrees/${worktreeId}/start`, {
      method: "POST",
      body: JSON.stringify(start),
    }),
  stopRun: (run: RunRef) =>
    request<void>(`/api/environments/${runPath(run)}/stop?${runScope(run)}`, {
      method: "POST",
    }),
  restartRun: (run: RunRef) =>
    request<void>(`/api/environments/${runPath(run)}/restart?${runScope(run)}`, {
      method: "POST",
    }),
  runAction: (run: RunRef, action: string, node?: string) =>
    request<void>(`/api/environments/${runPath(run)}/action?${runScope(run)}`, {
      method: "POST",
      body: JSON.stringify(node ? { action, node } : { action }),
    }),
  /** Launch the *operating system's* terminal app at a path. Unrelated to the
   *  in-app terminal panes below, which never leave the browser. */
  openTerminal: (path: string) =>
    request<void>("/api/open-terminal", {
      method: "POST",
      body: JSON.stringify({ path }),
    }),
  /**
   * Mint a single-use ticket for an in-app terminal in a worktree.
   *
   * The WebSocket that follows cannot carry the `X-Veld-Request` CSRF header
   * (handshakes can't set custom headers), so this CSRF-gated POST is what
   * proves the request came from this page — see `crates/veld-daemon/src/pty.rs`.
   * The ticket is good for one connection and expires in `expires_in_ms`;
   * mint a new one per connect, including every reconnect.
   *
   * `sessionId` names the shell. Passing the id of a session that is still
   * running reattaches to it — that is how a reload gets its terminal back —
   * and any other id starts a new one.
   */
  ptyTicket: (worktreeId: number, sessionId: string) =>
    request<PtyTicket>("/api/pty/tickets", {
      method: "POST",
      body: JSON.stringify({ worktree_id: worktreeId, session_id: sessionId }),
    }),
  /**
   * End a terminal session now.
   *
   * Required, because dropping the socket deliberately does *not* kill the
   * shell (that is what makes a reload survivable). Closing a tab without this
   * leaves the shell running until the daemon's detach grace expires.
   */
  closePtySession: (sessionId: string) =>
    request<void>(`/api/pty/sessions/${encodeURIComponent(sessionId)}`, {
      method: "DELETE",
    }),
  stats: () => request<StatsResponse>("/api/stats"),
  logs: (run: RunRef, opts: { source?: string; runId?: string } = {}) => {
    const q = new URLSearchParams({ lines: "500" });
    q.set("project_root", run.projectRoot);
    if (opts.source && opts.source !== "all") q.set("source", opts.source);
    if (opts.runId) q.set("run_id", opts.runId);
    return request<LogResponse>(`/api/logs/${runPath(run)}?${q.toString()}`);
  },
  shares: async () => normalizeShares(await request<SharesList>("/api/shares")),
  startShare: (run: RunRef, opts: { web?: boolean } = {}) =>
    request<{ join_url?: string }>("/api/shares", {
      method: "POST",
      body: JSON.stringify({
        run: run.name,
        project_root: run.projectRoot,
        ...(opts.web ? { web: true } : { approve: "manual" }),
      }),
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
