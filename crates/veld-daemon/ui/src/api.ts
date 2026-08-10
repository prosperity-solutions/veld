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

/**
 * One of a node's ports.
 *
 * `url` present means the port is routed (`protocol: "http"`) and can be opened;
 * absent means a raw `tcp` port, reachable at `hostname:port` and nowhere else.
 * Never synthesise a URL for one — a scheme in front of a raw port produces a
 * link that looks right and reaches nothing.
 */
export interface EndpointInfo {
  name: string;
  hostname: string;
  url?: string | null;
  port: number;
  /** The node's primary port — the one {@link NodeInfo.url} repeats. */
  primary: boolean;
}

export interface NodeInfo {
  name: string;
  variant: string;
  status: string;
  url?: string | null;
  /**
   * Every port the node claimed, primary first. Omitted by the daemon when
   * empty, and absent entirely from runs recorded before per-port endpoints —
   * so a client must fall back to {@link NodeInfo.url}, not assume a list.
   */
  endpoints?: EndpointInfo[];
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
  /**
   * What that run was started from. Carried per history entry, not inherited
   * from the environment: an earlier run of the same name may well have come
   * from a different preset or from explicit tokens.
   */
  started_from?: StartOrigin | null;
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
  /** When this environment's latest run started (RFC 3339). */
  created_at: string;
  ended_at?: string | null;
  urls: Record<string, string>;
  nodes: NodeInfo[];
  /**
   * What this run was started from. Absent for runs recorded before the daemon
   * kept the record — absent means "not known", never "started from nothing".
   */
  started_from?: StartOrigin | null;
  history?: HistoryEntry[];
}

/**
 * The invocation a run came from: a preset name, plus the `node:variant`
 * expansion that name meant **at start time**.
 *
 * Both halves matter. Presets are re-read from disk on every use, so the name
 * alone can become false while the run is live; comparing `selections` against
 * the current expansion (`Preset.expanded`) is what lets a surface say
 * `redefined since start` instead of asserting something stale. `preset: null`
 * with tokens present is an explicit-selection start, which is a fact about the
 * run rather than missing data.
 */
export interface StartOrigin {
  preset?: string | null;
  selections: string[];
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
  /**
   * The identifier: `[A-Za-z0-9._-]`, unique among the repo's checkouts, and the
   * default run name — which is why it is bounded, since the run name reaches a
   * hostname. Use it as a key, not as a label; `worktreeLabel(w)` is the label.
   */
  alias: string;
  /**
   * What the rail renders, or `""` to render the `alias` instead.
   *
   * Free text — this is the thing the user actually typed into the create
   * dialog's Name field, spaces and capitals intact, while the alias is the
   * lossy slug derived from it. Not unique, deliberately: two rows sharing a
   * label collide in nothing, and the branch renders beside it.
   *
   * **Optional because it can genuinely be absent, not as a convenience.** A
   * daemon older than schema v13 has no such column and sends no such key, and
   * `just dev-ui` proxies `/api` to whatever daemon is *installed* — so the dev
   * loop is exactly that pairing. Typed as required, `w.display_name.trim()`
   * compiled clean and crashed there; the `?` is what makes the type checker ask.
   *
   * Read it through `worktreeLabel(w)` rather than testing it inline, so
   * "which name does this surface show" has exactly one answer, and so the
   * absent case is handled in one place instead of at every call site.
   */
  display_name?: string;
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
  /**
   * Whether this checkout's removal is past the point of no return — it is not
   * in the trash any more, it is actively being deleted and cannot be restored.
   *
   * Backed by the daemon's in-memory deletion guard, not a column: it is only
   * true while `git worktree remove` is actually running. A row that has merely
   * been *queued* for removal still reports `trashed_at` (and no `deleting`), so
   * that the user can undo it up to the moment the removal starts.
   */
  deleting: boolean;
  has_veld_config: boolean;
  /**
   * Presets in display order, as resolved by the daemon.
   *
   * **`null` = the config could not be read; `[]` = it declares no presets.**
   * `has_veld_config` says only that the file exists. The nullability is the
   * enforcement: comparing a run's recorded preset against an empty list concludes
   * the preset was *deleted*, so a mid-edit `veld.json` made every healthy run in
   * that worktree read "preset dev (no longer defined)" — which shipped once. A
   * boolean beside an always-present array let a caller not notice; a nullable type
   * makes the compiler ask. Pass it straight to `startOriginLabel`, which takes
   * `null` to mean "cannot compare".
   */
  presets: Preset[] | null;
  /** Startable nodes (hidden excluded) for custom selections. */
  nodes: NodeOption[];
  /**
   * How many vars this checkout's config declares machine-overridable.
   *
   * **`null` means the config could not be read**, like `presets` — not zero.
   * Treating the two the same would disable the one control able to show the
   * reader why their config is broken.
   */
  machine_vars: number | null;
  /**
   * The interpreted `ide` section of the checkout's config. Always sent, with
   * arrays that may be empty — a worktree with no config gets empty ones rather
   * than the key being omitted.
   */
  ide: IdeSection;
}

/**
 * The settings document as the daemon sends it: a flat map of dotted keys to
 * JSON values — scalars, except `browser.externalOrigins`, which is a list of
 * origin patterns.
 *
 * Deliberately **not** a struct with one field per setting. The daemon preserves
 * keys it does not recognise so a preference written by a newer build survives a
 * downgrade, and a closed TypeScript interface would drop exactly those on the
 * next write. `settings.ts` is where keys get their types, at the point of use.
 */
export type SettingsDoc = Record<string, string | number | boolean | string[]>;

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
  /** Whether the pane's process may rename its tab with an OSC 0/2 title.
   *  A plain terminal always may; this opts a config pane in. */
  allow_terminal_renaming: boolean;
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
  /**
   * What the daemon can say about what this preset expands to *right now* — three
   * states, because collapsing any two of them makes a surface state something
   * false.
   *
   * `ok` carries the sorted `node:variant` set, directly comparable to
   * `RunInfo.started_from.selections` (and **not** the same as `selections`, which
   * is the raw config entries with `@preset` refs unexpanded and default variants
   * unfilled). An empty `tokens` is a real `ok`: a preset whose selections are `[]`
   * expands to nothing.
   *
   * `failed` is a preset that exists and does not expand — the state `veld status`
   * calls "cannot be expanded — see `veld lint`", and lint does report it.
   *
   * `skipped` is a preset this *listing* did not expand (past its per-poll cap).
   * Nothing is wrong with it, so a surface must not send the reader to `veld lint`;
   * treat it exactly like an unreadable config — cannot compare.
   */
  expansion:
    | { state: "ok"; tokens: string[] }
    | { state: "failed" }
    | { state: "skipped" };
  /** Whether this is the project's `default_preset`. */
  is_default: boolean;
}

/**
 * A worktree holding a given emoji — id, because names repeat across repos.
 *
 * `label` is the rendered name (`worktreeLabel`), not the alias: this exists to
 * be printed in "already used by …", and printing a slug the user has never seen
 * beside the name they chose reads as a different worktree.
 */
export interface EmojiHolder {
  id: number;
  label: string;
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
  /**
   * Raw `host:port` endpoints (unrouted `tcp` ports), separate from `urls`
   * because they are not openable. On a join these are the **local** listener
   * addresses — the origin's port number is not reachable from here.
   */
  addresses: string[];
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
    addresses: s.addresses ?? [],
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
 * A worktree's stored panes, as the daemon holds them.
 *
 * The daemon never looks inside `layout` — see `migrate_v15_pane_layouts` — so
 * this is `unknown` here too rather than `PaneLayout`: it is data that has been
 * round-tripped through a database and possibly written by a different build,
 * and `parseLayout` is the gate it goes through.
 */
export interface PaneLayoutDoc {
  /** `0` means the worktree has no stored layout. No stored row carries it. */
  version: number;
  /** `null` exactly when `version` is 0. */
  layout: unknown | null;
}

/**
 * What a layout save did.
 *
 * A conflict is not an error and is deliberately not thrown: it is the hand-off
 * case, and it carries the winning document so the loser can adopt it in the
 * same round trip.
 */
export type LayoutSaveResult =
  | { ok: true; doc: PaneLayoutDoc }
  | { ok: false; conflict: PaneLayoutDoc };

/** Where a URL from a terminal is going. See `api.ptyOpenUrl`. */
export interface PtyOpenUrl {
  target: "pane" | "system";
  /** Why it is not a pane — the exempt list, the preference, or no attached
   *  window. Absent for `pane`. */
  reason?: string;
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

/** Where a machine-overridable var's effective value came from. */
export type ConfigVarScope = "project" | "worktree" | "default" | "unset";

/**
 * One machine-overridable var as the daemon reports it.
 *
 * `value` is already **display-safe**: a var declared `secret` arrives as a
 * description (`<secret, from environment variable PGPASS>`), never as the
 * value. There is no endpoint that returns the real one — its only egress is
 * into a child process's environment at run start.
 */
export interface ConfigVar {
  name: string;
  /** Which layer supplied `value`. `unset` means nothing did. */
  from: ConfigVarScope;
  /** Display string, or null when the var has no value at all. */
  value: string | null;
  secret: boolean;
  /** The config's own fallback, display-safe, or null when there is none. */
  default: string | null;
  choices: string[] | null;
  description: string | null;
  prompt: string | null;
}

export interface ConfigVarList {
  /** Identifies the project across all of its worktrees. */
  projectId: string;
  /** The checkout these values were read for. */
  worktree: string;
  vars: ConfigVar[];
}

/** A var a pending start needs and this machine has not answered. */
export interface RequiredConfigVar {
  name: string;
  /** The declared prompt, description, or a generic fallback. */
  question: string;
  choices: string[] | null;
  secret: boolean;
  /** A stored answer exists but the config's `choices` no longer allow it. */
  stale: boolean;
}

export interface SetConfigVarBody {
  project: string;
  name: string;
  value?: string;
  env?: string;
  file?: string;
  shell?: string;
  /** Scope to this checkout instead of every worktree of the project. */
  worktree?: boolean;
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
    /** What the rail shows. Omit (or send `""`) to render the alias. */
    display_name?: string;
    /**
     * The lane to file it under — sent with the create, not patched after, so a
     * "＋" in a lane header cannot produce a worktree that shows up in the wrong
     * section first (or stays there if the follow-up fails).
     */
    lane?: string;
    emoji?: string;
    marker_color?: string;
  }) =>
    request<Worktree>("/api/worktrees", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  /**
   * Partial update — any combination of alias, display_name, emoji, marker_color
   * and lane.
   *
   * Lane assignment rides here rather than on its own endpoint because
   * `Db::patch_worktree` is the one owner of worktree-row edits. Send `lane: ""`
   * to ungroup, and `display_name: ""` to go back to rendering the alias — for
   * both, omitting the key means "leave it alone" and sending `""` is a change.
   */
  patchWorktree: (
    id: number,
    patch: {
      alias?: string;
      display_name?: string;
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
  /**
   * Start a run in a worktree. `run_name` names the environment: the daemon
   * defaults it to the worktree alias when omitted, which is a *silent* choice —
   * every caller here sends it so the name can be shown before the start (see
   * `startRunName`).
   */
  startRun: (
    worktreeId: number,
    start: { preset?: string; selections?: string[]; run_name?: string },
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
   * Mint a single-use ticket for the IDE control socket.
   *
   * Same handshake as `ptyTicket` and for the same reason: a WebSocket upgrade
   * cannot carry the `X-Veld-Request` header, so this CSRF-gated POST is what
   * proves the request came from this page. One ticket per connection,
   * including every reconnect.
   */
  ideTicket: () =>
    request<{ ticket: string; expires_in_ms: number; client_id: string }>("/api/ide/tickets", {
      method: "POST",
    }),
  /**
   * A worktree's panes.
   *
   * `version: 0` with a `null` layout means nobody has arranged this worktree
   * yet, which is the only case in which a client seeds a default. An empty
   * layout is a different answer and is not one of these.
   */
  paneLayout: (worktreeId: number) =>
    request<PaneLayoutDoc>(`/api/worktrees/${worktreeId}/layout`),
  /**
   * Store a worktree's panes, if `version` is still the current one.
   *
   * Resolves with `{ ok: false, conflict }` when it is not — see
   * [`LayoutSaveResult`]. It does not *reject*, deliberately: a conflict is an
   * outcome to reconcile, not a failure. That is a hand-off, not a merge
   * conflict: one client shows a worktree at a time, so the only way to
   * see this is a debounced save from the client that just let go racing the
   * one that just took it — and the loser must adopt what it is told rather
   * than overwrite it.
   *
   * `layout: null` forgets the worktree's panes, so the next client to open it
   * seeds a default instead of restoring an empty screen.
   */
  putPaneLayout: async (
    worktreeId: number,
    version: number,
    layout: unknown | null,
    clientId: string | null,
  ): Promise<LayoutSaveResult> => {
    const res = await fetch(`/api/worktrees/${worktreeId}/layout`, {
      method: "PUT",
      headers: { "Content-Type": "application/json", "X-Veld-Request": "1" },
      body: JSON.stringify({
        version,
        layout,
        // So the daemon pushes the change to the *other* clients and not back
        // to this one, which already has the answer in this response.
        ...(clientId ? { client_id: clientId } : {}),
      }),
    });
    // Not routed through `request`, which collapses every failure into an
    // `Error` and drops the body — and the body is the whole value of a 409
    // here. Reconciling from it costs no second round trip, and a re-read
    // would race the write that just beat us all over again.
    if (res.status === 409) {
      const body = (await res.json().catch(() => ({}))) as Partial<PaneLayoutDoc>;
      return {
        ok: false,
        conflict: {
          version: typeof body.version === "number" ? body.version : 0,
          layout: body.layout ?? null,
        },
      };
    }
    if (!res.ok) throw new Error(await errorMessage(res));
    return { ok: true, doc: (await res.json()) as PaneLayoutDoc };
  },
  /**
   * Which of a worktree's config-declared panes have a session to resume.
   *
   * The layout says which panes exist; whether a pane ever launched is a
   * separate row in the daemon's database. One request per worktree answers it
   * for every pane at once, so a restored dock can label its buttons before
   * anything connects.
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
  /**
   * Ask where a URL a terminal produced should open.
   *
   * The **daemon** decides, not this page, and that is the point: half the exempt
   * list is a setting and half is `ide.externalOrigins` in the project's
   * `veld.json`, which the renderer does not read. A process in the shell reaches
   * the same endpoint through `$BROWSER` → `veld open-url`, so a click and a CLI
   * get the same answer from the same code.
   *
   * `pane` means the daemon has already pushed an `open_url` frame down this
   * session's socket — the pane is opened by the frame handler, not by the caller.
   * `system` means this page should open it externally, and `reason` says why.
   */
  ptyOpenUrl: (sessionId: string, url: string) =>
    request<PtyOpenUrl>(`/api/pty/sessions/${encodeURIComponent(sessionId)}/open-url`, {
      method: "POST",
      body: JSON.stringify({ url }),
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

  /**
   * Machine-overridable vars for one project, with each value's scope.
   *
   * `project` is a **registered worktree root**, exactly — the daemon refuses a
   * path it is not tracking, and a subdirectory of one does not resolve even
   * though the config lookup underneath walks upward. The answer is shared by every worktree of the repo unless a var was
   * set with `worktree: true`.
   */
  configVars: (project: string) =>
    request<ConfigVarList>(
      `/api/config/vars?project=${encodeURIComponent(project)}`,
    ),
  /**
   * Store one answer. Pass exactly one of `value` (a literal) or `env`/`file`/
   * `shell` (a pointer resolved at run start).
   *
   * A pointer is how a `secret` var is answered without the secret itself
   * landing in veld's database — the same choice `veld config set` offers.
   */
  setConfigVar: (body: SetConfigVarBody) =>
    request<{ name: string; scope: ConfigVarScope; value: string }>(
      "/api/config/vars",
      { method: "PUT", body: JSON.stringify(body) },
    ),
  /**
   * What *this* start would need and this machine has not answered.
   *
   * Scoped to the plan the given preset/selections expand to, including
   * transitive dependencies — so it asks for exactly the vars the run will
   * reach, and not for the ones it won't.
   */
  configVarsPreflight: (body: {
    project: string;
    preset?: string;
    selections?: string[];
  }) =>
    request<{ needed: RequiredConfigVar[] }>("/api/config/vars/preflight", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  /** Forget one answer, falling back to the next scope and then the default. */
  clearConfigVar: (project: string, name: string, worktree = false) =>
    request<{ name: string; scope: ConfigVarScope; removed: boolean }>(
      `/api/config/vars?project=${encodeURIComponent(project)}&name=${encodeURIComponent(name)}&worktree=${worktree}`,
      { method: "DELETE" },
    ),
};
