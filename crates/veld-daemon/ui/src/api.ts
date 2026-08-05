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
  /**
   * The colour half of the marker: a literal `#rrggbb`, or `""` when it has not
   * been assigned yet (a row from before the column existed, filled in on the next
   * sync).
   *
   * The marker is a composite and the `worktree.markerStyle` setting picks which
   * face renders — so this and `emoji` are both always present and neither is
   * cleared by changing the style. Rendering reads one; the picker edits both.
   * Like `emoji`, it is not an identity: two worktrees of different repos routinely
   * share a colour.
   */
  marker_color: string;
  is_main: boolean;
  created_at: string;
  /**
   * The rail lane this worktree is grouped under, or `""` for ungrouped.
   *
   * The lane's **name**, not an id — see the v10 migration. A name this repo has
   * no lane for renders as ungrouped rather than as an error.
   */
  lane: string;
  /**
   * Manual position within the lane, or `null` for "the user has not placed this
   * one", which sorts to an alias-ordered tail rather than to position 0. A newly
   * discovered worktree is always `null`.
   */
  sort_position: number | null;
  /**
   * When this checkout was moved to the trash, or `""` if it is not in the trash.
   *
   * A timestamp rather than a flag because it is also the retention clock: the
   * checkout stays on disk until `worktree.trashRetentionDays` elapses from here, or
   * until the user deletes it explicitly. Nothing has been deleted while this is set.
   */
  trashed_at: string;
  /**
   * Why the last deletion attempt failed, or `""`. Set together with clearing
   * `trashed_at`, so a worktree carrying this is *out* of the trash and back in the
   * rail — the failure is neither silent nor still pending.
   */
  trash_error: string;
  has_veld_config: boolean;
  /** Presets in display order, as resolved by the daemon. */
  presets: Preset[];
  /** Startable nodes (hidden excluded) for custom selections. */
  nodes: NodeOption[];
  /**
   * The interpreted `ide` section of the checkout's config. Always sent, with
   * arrays that may be empty — a worktree with no config gets empty ones rather
   * than the key being omitted.
   */
  ide: IdeSection;
}

/**
 * The settings document as the daemon sends it: a flat map of dotted keys to
 * JSON scalars.
 *
 * Deliberately **not** a struct with one field per setting. The daemon preserves
 * keys it does not recognise so a preference written by a newer build survives a
 * downgrade, and a closed TypeScript interface would drop exactly those on the
 * next write. `settings.ts` is where keys get their types, at the point of use.
 */
export type SettingsDoc = Record<string, string | number | boolean>;

/** Mirrors `IdeView` in `crates/veld-daemon/src/desktop.rs`. */
export interface IdeSection {
  quicklinks: Quicklink[];
  /**
   * Permission pre-answers for browser panes. Only the desktop app can act on
   * these; the browser build has no panes and ignores them.
   */
  permissions: PermissionRule[];
  /**
   * Pane types the project adds to the pane menu.
   *
   * **No commands here, and no token.** The renderer names a pane by `id` and
   * the daemon resolves what that means from the project's own config, so
   * nothing the client holds can change what gets run.
   */
  panes: PaneSpec[];
}

/** Mirrors `PaneView` in `crates/veld-daemon/src/desktop.rs`. */
export interface PaneSpec {
  id: string;
  label: string;
  description?: string;
  icon?: PaneIcon;
  /** Which runtime pane kind this spec produces. Only `terminal` today. */
  kind: "terminal";
  /** False when a `requires_bin` executable is missing. Listed anyway, so the
   *  menu can explain the absence rather than silently omitting an entry the
   *  repo declares. */
  available: boolean;
  /** Whether the pane declares a resume command at all. */
  can_resume: boolean;
  /** Whether a restored pane whose shell is gone may resume without a click. */
  auto_resume: boolean;
  /** Whether a clean (status 0) exit closes the pane. Never applies to a
   *  non-zero exit, which keeps the pane so the error stays readable. */
  close_on_exit: boolean;
  /** The `requires_bin` executables that were not found. Omitted when empty.
   *  The pane's id is not a substitute — `claude-yolo` needs `claude`. */
  missing?: string[];
}

/** Mirrors `veld_core::ide::PaneIcon`. */
export type PaneIcon =
  | { kind: "name"; value: string }
  | { kind: "emoji"; value: string };

/** A project link that is not veld's own, shown on a browser pane's start page. */
export interface Quicklink {
  label: string;
  url: string;
}

/** Mirrors `veld_core::ide::PermissionRule`. */
export interface PermissionRule {
  origin: OriginPattern;
  allow: PermissionId[];
  deny: PermissionId[];
}

/**
 * An origin, already split by the daemon so the matcher never re-parses it.
 *
 * `port: null` means "any port" — written as `*` in the config. An omitted port
 * is normalised to the scheme's default before it gets here, so `null` never
 * means "unspecified".
 */
export interface OriginPattern {
  raw: string;
  scheme: string;
  /** The host, or — when `wildcard` — the suffix it must sit under, with the
   *  leading `*.` already removed. */
  host: string;
  /** A leading `*.` on the host: any subdomain, at any depth. Omitted by the
   *  daemon when false, so a missing field means no wildcard. */
  wildcard?: boolean;
  port: number | null;
}

/**
 * Veld's permission ids. The list is the `$defs.permissionId` enum in
 * `schema/v3/veld.schema.json`; `veld_core::ide::PERMISSION_IDS` and the desktop
 * app's mapping table are both gated against it by tests.
 */
export type PermissionId =
  | "camera"
  | "clipboard-read"
  | "clipboard-write"
  | "display-capture"
  | "file-system"
  | "fullscreen"
  | "geolocation"
  | "hid"
  | "idle-detection"
  | "keyboard-lock"
  | "microphone"
  | "midi"
  | "notifications"
  | "open-external"
  | "pointer-lock"
  | "protected-media"
  | "serial"
  | "speaker-selection"
  | "storage-access"
  | "usb"
  | "window-management";

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
  /**
   * The repo's rail lanes, in their own order.
   *
   * Sent with the repo rather than fetched separately so the rail never has a
   * frame where a worktree's `lane` names a group it has not heard of.
   */
  lanes: Lane[];
}

/**
 * Longest lane name the daemon accepts.
 *
 * Mirrors `MAX_LANE_NAME_LEN` in `crates/veld-core/src/db/worktrees.rs`. Nothing
 * pins the pair, and it does not need pinning: this only stops the input
 * accepting a name the daemon would reject, and the daemon is the enforcement. If
 * it drifts low the field truncates early; if it drifts high the user gets the
 * server's error, which is the behaviour without this constant at all.
 */
export const MAX_LANE_NAME_LEN = 32;

/** A user-defined rail lane. Identified by `(repo root, name)` — there is no id. */
export interface Lane {
  repo_root: string;
  name: string;
  position: number;
  created_at: string;
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

/**
 * The page classes a platform may report, `null` where it cannot.
 *
 * `null` means "not measurable here", never zero — Linux reads them from
 * `smaps_rollup`, macOS has no unprivileged equivalent. Render an absent class
 * as unavailable; a 0-height bar claims the node holds no private memory.
 */
export interface MemoryClasses {
  private_clean: number | null;
  private_dirty: number | null;
  shared_clean: number | null;
  shared_dirty: number | null;
  swap: number | null;
  wired: number | null;
}

export interface NodeStats extends MemoryClasses {
  cpu: number;
  /** RSS, summed over the tree — double-counts pages shared inside it. */
  mem: number;
  procs: number;
  /** Recent footprint samples, oldest-first. */
  spark: number[];
  /** Summed PSS (Linux) / phys_footprint (macOS) — the figure to display. */
  footprint: number;
  virt: number;
  cpu_seconds: number;
}

/** project_root → run name → "node:variant" → stats */
export interface StatsResponse {
  projects: Record<string, Record<string, Record<string, NodeStats>>>;
}

/** Every memory metric the wire can name. Matches Rust's `MemoryMetric`. */
export type MemoryMetric =
  | "footprint"
  | "resident"
  | "virtual"
  | "private_clean"
  | "private_dirty"
  | "shared_clean"
  | "shared_dirty"
  | "swap"
  | "wired";

/**
 * One aggregated bucket of history.
 *
 * Buckets with no samples are **omitted**, so consecutive entries are not
 * necessarily adjacent in time — a chart must lay them out by `t` and break the
 * line across a gap. Joining them up would invent data.
 */
export interface StatsBucket extends MemoryClasses {
  /** Bucket start, epoch milliseconds. */
  t: number;
  /** Raw samples averaged into this bucket; always ≥ 1. */
  samples: number;
  cpu: number;
  cpu_peak: number;
  procs: number;
  resident: number;
  footprint: number;
  footprint_peak: number;
  virtual: number;
}

export interface ProcessSeries {
  pid: number;
  name: string;
  cmd: string | null;
  buckets: StatsBucket[];
}

export interface ProcessRow extends MemoryClasses {
  pid: number;
  parent_pid: number | null;
  /** Depth below the node's root process. Indent by this — the parent may be
   * absent from the list, since the sampler caps processes recorded per sample. */
  depth: number;
  name: string;
  cmd: string | null;
  cpu: number;
  cpu_seconds: number;
  resident: number;
  footprint: number;
  virtual: number;
  started_at: number | null;
}

export interface StatsHistory {
  /** Window actually served (epoch ms) — the server clamps, so trust this over
   * whatever was requested. */
  start: number;
  end: number;
  bucket_secs: number;
  /** Metrics these samples carry, in picker order. */
  available_metrics: MemoryMetric[];
  buckets: StatsBucket[];
  processes: ProcessSeries[];
  processes_omitted: number;
  tree: ProcessRow[];
  /** How long the daemon keeps node aggregates (seconds). Build window presets
   * from this rather than a local copy — a hardcoded cap silently stops matching
   * the daemon's GC. */
  retention_secs: number;
  /** How long per-process rows are kept (seconds) — shorter than
   * `retention_secs`, so a by-process chart over a longer window is legitimately
   * empty before that boundary. */
  process_retention_secs: number;
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
 * Which command a config-declared pane runs.
 *
 * `fresh` mints a new token and runs the pane's `argv`/`shell`; `resume` runs
 * its `resume` command under the token the pane launched with. The daemon
 * refuses `resume` for a pane that never launched rather than quietly starting
 * a fresh one — a silent fallback would begin a new billable conversation and
 * read to the user as the old one having been lost.
 */
export type PaneLaunchMode = "fresh" | "resume";

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
  // Long enough for the daemon's multi-sentence refusals, capped so a runaway body
  // can't become the whole toast. Backs off a code unit when the cut lands between
  // a surrogate pair — the daemon quotes paths and worktree aliases, and those can
  // hold an emoji, which would otherwise render as U+FFFD.
  if (raw.length <= 600) return raw;
  const cut = /[\uD800-\uDBFF]$/.test(raw.slice(0, 600)) ? 599 : 600;
  return `${raw.slice(0, cut)}…`;
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
  /**
   * Create a worktree.
   *
   * `emoji`/`marker_color` are the create dialog's marker pick. Sent with the create
   * rather than patched after it so the checkout never appears in the rail carrying
   * the daemon's assigned marker for a poll before changing to the chosen one; omit
   * them and the daemon assigns.
   */
  createWorktree: (body: {
    repo_root: string;
    branch: string;
    create_branch: boolean;
    alias?: string;
    emoji?: string;
    marker_color?: string;
  }) =>
    request<Worktree>("/api/worktrees", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  /**
   * Partial update — any combination of alias, emoji, marker_color and lane.
   *
   * Lane assignment rides here rather than on its own endpoint because
   * `Db::patch_worktree` is the one owner of worktree-row edits. Send `lane: ""`
   * to ungroup.
   */
  patchWorktree: (
    id: number,
    patch: {
      alias?: string;
      emoji?: string;
      marker_color?: string;
      lane?: string;
    },
  ) =>
    request<Worktree>(`/api/worktrees/${id}`, {
      method: "PATCH",
      body: JSON.stringify(patch),
    }),
  /**
   * What the marker picker may offer: the glyph allowlist and how many hues
   * exist. Served by the daemon rather than duplicated here so the picker and
   * the server-side allowlist can't drift.
   *
   * `colors` are the literal values the picker offers. Not the set of *storable*
   * values — the daemon accepts any `#rrggbb`, so a custom colour is a UI addition
   * rather than a schema change.
   */
  worktreeEmoji: () =>
    request<{ emoji: string[]; colors: string[] }>("/api/worktree-emoji"),
  /**
   * Move a checkout to the trash (202) — or, with `force`, delete it outright.
   *
   * Binning deletes nothing: the checkout stays on disk until its retention expires
   * or it is deleted explicitly, so this returns immediately and restoring is a real
   * undo. Forcing is inline (204/422/409) because it answers a refusal the user has
   * already been shown.
   */
  deleteWorktree: (id: number, force: boolean) =>
    request<void>(`/api/worktrees/${id}?force=${force}`, { method: "DELETE" }),
  /**
   * Take a worktree out of the trash. Fails with 404 only if an explicit deletion
   * already got there.
   */
  restoreWorktree: (id: number) =>
    request<Worktree>(`/api/worktrees/${id}/restore`, { method: "POST" }),
  /**
   * Delete a trashed worktree now instead of waiting for its retention (202).
   * `409` if it is not in the trash — this is not a shortcut past the confirmation.
   */
  deleteTrashedWorktree: (id: number) =>
    request<void>(`/api/worktrees/${id}/delete`, { method: "POST" }),
  /** Delete every trashed worktree of a repo now. */
  emptyTrash: (repo_root: string) =>
    request<{ queued: number }>(
      `/api/trash?repo_root=${encodeURIComponent(repo_root)}`,
      { method: "DELETE" },
    ),
  /** Clear a recorded deletion failure — the user has read it. */
  dismissTrashError: (id: number) =>
    request<void>(`/api/worktrees/${id}/trash-error`, { method: "DELETE" }),
  /**
   * Rewrite the manual worktree order for a repo.
   *
   * Send the **full order being displayed**, as paths: paths because
   * `worktrees.id` is a reused rowid, and the full list because that makes the
   * write idempotent — omitted paths go back to unplaced.
   */
  reorderWorktrees: (repo_root: string, order: string[]) =>
    request<void>("/api/worktree-order", {
      method: "POST",
      body: JSON.stringify({ repo_root, order }),
    }),
  createLane: (repo_root: string, name: string) =>
    request<{ lane: Lane }>("/api/lanes", {
      method: "POST",
      body: JSON.stringify({ repo_root, name }),
    }),
  renameLane: (repo_root: string, from: string, name: string) =>
    request<void>(`/api/lanes/${encodeURIComponent(from)}`, {
      method: "PATCH",
      body: JSON.stringify({ repo_root, name }),
    }),
  /** Delete a lane. Its members are ungrouped, never removed. */
  deleteLane: (repo_root: string, name: string) =>
    request<void>(
      `/api/lanes/${encodeURIComponent(name)}?repo_root=${encodeURIComponent(repo_root)}`,
      { method: "DELETE" },
    ),
  reorderLanes: (repo_root: string, order: string[]) =>
    request<void>("/api/lane-order", {
      method: "POST",
      body: JSON.stringify({ repo_root, order }),
    }),
  /**
   * Every setting's **effective** value — the daemon merges its own defaults
   * under whatever is stored, so this is always a complete document and the UI
   * holds no defaults of its own. That is deliberate: a default duplicated in
   * TypeScript is a default that drifts from the Rust one.
   */
  settings: () => request<{ settings: SettingsDoc }>("/api/settings"),
  /**
   * Write only the keys given. A patch rather than a replacement, so two windows
   * with the settings surface open cannot revert each other's unrelated edits.
   *
   * Returns the full effective document, which is the point: the daemon clamps
   * out-of-range numbers, and a control that kept showing the requested value
   * would sit at a number the daemon never stored.
   */
  patchSettings: (patch: SettingsDoc) =>
    request<{ settings: SettingsDoc }>("/api/settings", {
      method: "PATCH",
      body: JSON.stringify(patch),
    }),
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
  ptyTicket: (
    worktreeId: number,
    sessionId: string,
    pane?: { spec: string; mode: PaneLaunchMode },
  ) =>
    request<PtyTicket>("/api/pty/tickets", {
      method: "POST",
      body: JSON.stringify({
        worktree_id: worktreeId,
        session_id: sessionId,
        // A pane *name*, never a command: the daemon reads what to run from
        // the project's config. Ignored server-side when the session is
        // already live, since nothing is being spawned.
        ...(pane ? { pane: pane.spec, mode: pane.mode } : {}),
      }),
    }),
  /**
   * Which of a worktree's config-declared panes have a session to resume.
   *
   * The pane layout comes from browser storage; whether a pane ever launched
   * is in the daemon's database. One request per worktree answers it for every
   * pane at once, so a restored dock can label its buttons before anything
   * connects.
   */
  paneSessions: (worktreeId: number) =>
    request<{ resumable: { session_id: string; pane: string }[] }>(
      `/api/pty/panes/${worktreeId}`,
    ),
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
  /**
   * Bucketed history for one node. `windowSecs` is clamped server-side to the
   * retention horizon, and `points` bounds the returned bucket count — so the
   * payload stays the same size whether the window is a minute or a day.
   */
  statsHistory: (
    run: RunRef,
    nodeKey: string,
    opts: { windowSecs: number; points?: number; processes?: boolean },
  ) => {
    const q = new URLSearchParams({
      project_root: run.projectRoot,
      run: run.name,
      node: nodeKey,
      window: String(opts.windowSecs),
    });
    if (opts.points) q.set("points", String(opts.points));
    if (opts.processes) q.set("processes", "true");
    return request<StatsHistory>(`/api/stats/history?${q.toString()}`);
  },
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
