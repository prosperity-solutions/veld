//! The engine's wire types: what a face (CLI, IDE, a phone) sends the engine and
//! what it gets back.
//!
//! Every type here crosses a process boundary — face → daemon over HTTP, daemon →
//! worker on stdin, worker → face through the action's event log — and the two
//! ends are not always the same build: an update replaces the daemon while a
//! worker started by the old one is still writing events, and a CLI can be a
//! version behind the daemon it talks to. So:
//!
//! - **No `deny_unknown_fields`**, anywhere. A newer writer may add a field.
//! - **Every enum a reader matches on has an `Unknown` arm** (`#[serde(other)]`)
//!   where a newer writer could add a variant, so an old reader degrades to "I do
//!   not know this event" instead of failing the whole stream.
//! - **Secrets never live in a type that is persisted.** [`ActionRequest`] is
//!   stored on the action's row; the caller's environment and per-run var answers
//!   travel separately in [`ActionSecrets`], which only ever goes down a pipe.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use veld_core::presets::ResolvedPreset;
use veld_core::state::{NodeStatus, RunState};
use veld_core::stats::ProcessStats;
use veld_core::values::UnansweredVar;

use crate::progress::ProgressEvent;

/// One thing the engine is asked to do to a project's runs.
///
/// Persisted verbatim on the action's row (`engine_actions.request_json`), which
/// is why nothing secret is in it — see [`ActionSecrets`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRequest {
    /// The project the action is scoped to. The daemon resolves this from its own
    /// registry or the caller's worktree; it is never a path the worker trusts to
    /// mean anything but "the directory whose `veld.json` to load".
    pub project_root: PathBuf,
    pub kind: ActionKind,
    /// Verbose orchestrator diagnostics (`veld orchestrator start --debug`).
    #[serde(default)]
    pub debug: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionKind {
    /// Start (or replace) a run. `selections` are already resolved `node:variant`
    /// pairs — resolution from presets, keys and pickers is [`PreflightRequest`]'s
    /// job, so what is stored here is exactly what was started.
    Start {
        run: String,
        selections: Vec<String>,
        /// The preset the selections came from, recorded on the run as its origin.
        #[serde(default)]
        origin_preset: Option<String>,
        /// `--oneshot`: the one `command` node whose exit ends the run. Its
        /// stdout/stderr come back as [`EngineEvent::Output`].
        #[serde(default)]
        oneshot: Option<String>,
        /// `--oneshot --all-logs`: also stream the dependencies' server output.
        #[serde(default)]
        all_logs: bool,
    },
    /// Stop one run (also the daemon's crash teardown: an ended run's `stop`
    /// runs what it still owes).
    Stop { run: String },
    /// Stop every run of the project.
    StopAll,
    /// Stop a run and start it again from what it was started with.
    Restart { run: String },
    /// Run a configured action against a run, unattended (the IDE's action
    /// buttons). The CLI's `veld orchestrator action` resolves and runs in the user's own
    /// terminal instead — see [`ResolveActionRequest`].
    Action {
        run: String,
        action: String,
        #[serde(default)]
        node: Option<String>,
    },
    /// A kind a newer face sent that this engine does not know. Refused by the
    /// runner; here so an older daemon answers it instead of failing to parse.
    #[serde(other)]
    Unknown,
}

impl ActionKind {
    /// The run this action is about, when it is about one.
    pub fn run(&self) -> Option<&str> {
        match self {
            Self::Start { run, .. }
            | Self::Stop { run }
            | Self::Restart { run }
            | Self::Action { run, .. } => Some(run),
            Self::StopAll | Self::Unknown => None,
        }
    }

    /// Whether an action of this kind owns its run's lifecycle while it runs.
    /// While one does, the daemon's crash recovery and GC's stale-`stopping`
    /// reaper leave the run alone. A node action changes no run's lifecycle, so a
    /// long one must not switch recovery off. Exhaustive on purpose: a new kind
    /// has to decide.
    pub fn gates_recovery(&self) -> bool {
        match self {
            Self::Start { .. } | Self::Stop { .. } | Self::StopAll | Self::Restart { .. } => true,
            Self::Action { .. } | Self::Unknown => false,
        }
    }

    /// Whether it acts on every run of its project, so an in-flight one gates
    /// recovery for all of them, not only a run of its own name.
    pub fn gates_whole_project(&self) -> bool {
        match self {
            Self::StopAll => true,
            Self::Start { .. }
            | Self::Stop { .. }
            | Self::Restart { .. }
            | Self::Action { .. }
            | Self::Unknown => false,
        }
    }

    /// Whether a face's Ctrl-C may cancel it. Only what can be undone: a start
    /// (stopped again) and a restart's start phase. A stop or a teardown runs to
    /// its end — cutting one short leaves containers behind.
    pub fn cancellable(&self) -> bool {
        match self {
            Self::Start { .. } | Self::Restart { .. } => true,
            Self::Stop { .. } | Self::StopAll | Self::Action { .. } | Self::Unknown => false,
        }
    }

    /// A short stable name for logs and the `engine_actions.kind` column.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Stop { .. } => "stop",
            Self::StopAll => "stop_all",
            Self::Restart { .. } => "restart",
            Self::Action { .. } => "action",
            Self::Unknown => "unknown",
        }
    }
}

/// What a face hands the engine for one action and nothing else may see.
///
/// Goes from face to daemon in the request body and from daemon to worker on the
/// worker's stdin. Never stored, never logged, never in argv or a process
/// environment (`ps eww` shows both).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSecrets {
    /// The caller's values for the env names veld.json (and this machine's stored
    /// var sources) declare with `{ "env": … }` — and only those. See
    /// [`Preflight::env_names`]. A face with no terminal sends none.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Answers to machine-overridable vars for this action.
    #[serde(default)]
    pub var_answers: Vec<VarAnswer>,
}

/// One answer to a machine-overridable var.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarAnswer {
    pub name: String,
    pub value: String,
    /// Where it came from: `flag` (`--var`, never stored) or `prompt`.
    pub provenance: AnswerProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerProvenance {
    /// `--var NAME=VALUE`, or a prompt answer the human chose not to save.
    PerRun,
    /// A prompt answer that was saved for the machine in the same breath. The
    /// worker uses the value it was given (the store may not be readable yet) but
    /// records it as stored, so the teardown warning does not fire for it.
    Saved,
}

/// The body of `POST /api/engine/actions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitAction {
    pub request: ActionRequest,
    #[serde(default)]
    pub secrets: ActionSecrets,
}

/// The answer to a submit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    pub action_id: String,
}

/// How a stored or streamed event says it is the final one. Shared by the writer
/// (serde's tag for [`EngineEvent::Finished`]) and the daemon's stream, which
/// ends a face's stream when it passes one; a test pins that the two agree.
pub const FINISHED_TAG: &str = r#""type":"finished""#;

/// One line of an action's event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Monotonic across all actions. A reader resumes with `?after=<seq>`.
    pub seq: i64,
    pub event: EngineEvent,
}

/// Something that happened during an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    /// The orchestrator's own progress, unchanged.
    Progress { progress: ProgressEvent },
    /// A line the user must see that belongs to no node: a share released, a
    /// teardown hook skipped, a run interrupted. Rendered as-is.
    Notice { message: String, level: NoticeLevel },
    /// `--oneshot` output: a line of the terminal node's stdout or stderr, for the
    /// face to write back to its own stream of the same name.
    Output { stream: OutputStream, line: String },
    /// `--oneshot --all-logs`: a dependency's server output line.
    DependencyLog {
        node: String,
        variant: String,
        line: String,
    },
    /// The action is over. Always the last event.
    Finished { outcome: ActionOutcome },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    /// Something was left undone that the user may need to clean up.
    Warning,
    Error,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// How an action ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ActionOutcome {
    /// A start (or restart) brought the run up.
    Started { run: Box<RunState> },
    /// A `--oneshot` start ran its terminal node to completion and tore down.
    OneshotExited {
        exit_code: i32,
        /// The terminal node, `node:variant`, for the final line.
        #[serde(default)]
        label: String,
    },
    /// A stop finished. `already` = there was nothing running to stop.
    Stopped {
        stopped: Vec<String>,
        #[serde(default)]
        already: bool,
    },
    /// An action button ran.
    ActionRan { exit_code: i32 },
    /// The face cancelled (Ctrl-C) and the engine cleaned up what it had started.
    Cancelled,
    /// It failed. `message` is what to print; `kind` lets a face special-case.
    Failed {
        message: String,
        #[serde(default)]
        kind: FailureKind,
        /// The node whose failure ended a start, `node:variant` — so a face can
        /// show that node's last log lines.
        #[serde(default)]
        failed_node: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

impl ActionOutcome {
    /// The exit code a CLI reports for this outcome.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Started { .. } | Self::Stopped { .. } => 0,
            Self::OneshotExited { exit_code, .. } | Self::ActionRan { exit_code } => *exit_code,
            Self::Cancelled => 130,
            Self::Failed { .. } | Self::Unknown => 1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    #[default]
    Error,
    /// A machine-overridable var has no answer. Preflight should have caught it;
    /// it reaches here when veld.json changed between preflight and the worker.
    NeedsAnswers,
    /// The worker died (or the daemon lost it) before it said how it ended.
    WorkerLost,
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

/// The body of `POST /api/engine/preflight`: what `veld orchestrator start`/`restart` was
/// asked, before anything is spawned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightRequest {
    pub project_root: PathBuf,
    pub plan: PreflightPlan,
    /// `--var NAME=VALUE` pairs, unparsed — the engine validates them against the
    /// declarations, so the refusal wording lives in one place.
    #[serde(default)]
    pub var_flags: Vec<String>,
    /// Whether a human is present to pick a preset. Decides between "take the
    /// declared default" and "ask".
    #[serde(default)]
    pub interactive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreflightPlan {
    Start {
        /// Positional `node[:variant]` tokens as typed.
        #[serde(default)]
        selections: Vec<String>,
        #[serde(default)]
        preset: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        oneshot: bool,
        #[serde(default)]
        all_logs: bool,
    },
    Restart {
        #[serde(default)]
        name: Option<String>,
    },
    Stop {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        all: bool,
    },
}

/// The engine's answer to a preflight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PreflightResponse {
    /// Ready to submit. The face prompts for `unanswered` (if it can), gathers
    /// `env_names` from its environment, and submits `action`.
    Ready(Box<Preflight>),
    /// Nothing was selected and nothing is the default: a human must pick.
    Choose(Box<Choice>),
    /// The request cannot be carried out. `message` is what to print. `payload`
    /// is a machine-readable body for the unattended no-selection case.
    Refused {
        message: String,
        #[serde(default)]
        payload: Option<serde_json::Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preflight {
    /// The resolved action, run name included.
    pub action: ActionKind,
    /// Machine-overridable vars this plan needs and this machine cannot answer.
    #[serde(default)]
    pub unanswered: Vec<UnansweredVarWire>,
    /// Every env name the config or this machine's stored var sources read with
    /// `{ "env": … }`. A superset of what this plan reaches, deliberately: the
    /// face sends the values it has for these and nothing else.
    #[serde(default)]
    pub env_names: Vec<String>,
    /// Per-run answers the teardown will need but will not have (see the
    /// "stranded" warning). Names only.
    #[serde(default)]
    pub stranded_at_teardown: Vec<String>,
    /// Lines to show before starting (e.g. "Starting default preset `x`.").
    #[serde(default)]
    pub notices: Vec<String>,
    /// Answers parsed from `--var`, to send back as [`VarAnswer`]s.
    #[serde(default)]
    pub flag_answers: Vec<VarAnswer>,
}

/// [`UnansweredVar`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnansweredVarWire {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub choices: Option<Vec<String>>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub stale: Option<String>,
}

impl From<UnansweredVar> for UnansweredVarWire {
    fn from(v: UnansweredVar) -> Self {
        Self {
            name: v.name,
            description: v.description,
            prompt: v.prompt,
            choices: v.choices,
            secret: v.secret,
            stale: v.stale,
        }
    }
}

impl From<UnansweredVarWire> for UnansweredVar {
    fn from(v: UnansweredVarWire) -> Self {
        Self {
            name: v.name,
            description: v.description,
            prompt: v.prompt,
            choices: v.choices,
            secret: v.secret,
            stale: v.stale,
        }
    }
}

/// What a picker shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub presets: Vec<ResolvedPreset>,
    pub show_groups: bool,
    /// `(node, sorted variants)`, sorted by node — for a project with no presets.
    pub nodes: Vec<(String, Vec<String>)>,
}

/// The body of `POST /api/engine/var-answers`: answers the human chose to save
/// for this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveVarAnswers {
    pub project_root: PathBuf,
    pub answers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedVarAnswers {
    /// Names that reached the database.
    pub saved: Vec<String>,
    /// `(name, error)` for each that did not.
    #[serde(default)]
    pub failed: Vec<(String, String)>,
    /// The project id they were saved under, for the confirmation line.
    pub project_id: String,
}

// ---------------------------------------------------------------------------
// `veld orchestrator action` in the user's terminal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveActionRequest {
    pub project_root: PathBuf,
    /// The caller's values for the declared env names (see
    /// `GET /api/engine/env-names`), for `{ "env": … }` sources in the node's env.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub action: String,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    /// `--print`/`--json`: show the command even when the node's env cannot be
    /// resolved right now — nothing will run.
    #[serde(default)]
    pub display_only: bool,
}

/// What to exec. The face runs it itself, in its own terminal, with its own
/// environment underneath `env` (`veld orchestrator action` is interactive — a DB shell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAction {
    /// A `shell` action's script, run with `$SHELL -c`; for an `argv` action, the
    /// quoted display form only.
    pub command: String,
    /// An `argv` action's elements, exec'd directly — never through a shell.
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    pub cwd: PathBuf,
    /// Overlaid on the face's own environment.
    pub env: BTreeMap<String, String>,
    /// The action's display label, for the "Running X" line.
    #[serde(default)]
    pub label: String,
    /// The run it resolved against, for the "Running X on Y" line.
    pub run: String,
    /// "Using environment …" when the run was inferred, for the face to show.
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

/// The engine-relevant part of `GET /api/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineHealth {
    pub version: String,
    /// The database this daemon's engine reads and writes. A face built against a
    /// different one must not drive this daemon — it would stop and start runs it
    /// cannot see (#306's class of bug).
    #[serde(default)]
    pub db_path: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Queries: `veld orchestrator status` and `veld orchestrator logs`
// ---------------------------------------------------------------------------

/// What `GET /api/engine/status` answers: everything `veld orchestrator status` prints, in
/// human and `--json` mode, so a face only renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    /// The environment the report is about (resolved when the caller gave none).
    pub name: String,
    /// The "Using environment …" line to show when the name was inferred.
    #[serde(default)]
    pub note: Option<String>,
    /// The run as stored. `effective` says which nodes' recorded status is no
    /// longer true.
    pub run: RunState,
    /// Per node key (`node:variant`), the status after a process-liveness check:
    /// a node recorded as running whose process is gone reads `Failed`.
    pub effective: std::collections::HashMap<String, NodeStatus>,
    /// Latest fresh resource sample per node key. Stale samples are dropped.
    #[serde(default)]
    pub stats: std::collections::HashMap<String, ProcessStats>,
    /// Per-node outputs, sensitive values masked. Only when `outputs=true`.
    #[serde(default)]
    pub outputs: Option<Vec<NodeOutputs>>,
}

/// One node's recorded outputs, sorted by key, sensitive values already `***`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeOutputs {
    pub node: String,
    pub variant: String,
    pub values: Vec<(String, String)>,
}

/// Which stored log streams `--source` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceFilter {
    #[default]
    All,
    Server,
    #[serde(alias = "teardown")]
    Setup,
    #[serde(alias = "veld")]
    Internal,
}

/// The query of `GET /api/engine/logs` (the fields of `veld orchestrator logs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsQuery {
    pub project_root: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    /// Lines per source (per node and stream), not in total.
    #[serde(default = "default_log_lines")]
    pub lines: usize,
    /// `5m`, `1h`, `30s`, `2d` or plain seconds; takes the place of `lines`.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub source: SourceFilter,
    /// Case-insensitive; with `context`, matches plus the lines around them.
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub context: usize,
    /// A past run by id prefix, project-wide.
    #[serde(default)]
    pub run: Option<String>,
    /// The run before the latest one.
    #[serde(default)]
    pub previous: bool,
    /// Every run under the name interleaved.
    #[serde(default)]
    pub all_runs: bool,
    /// Follow mode: only rows with a greater id (filtered by `search`), no
    /// context, no snapshot. `None` is the historical snapshot.
    #[serde(default)]
    pub after_id: Option<i64>,
}

fn default_log_lines() -> usize {
    100
}

/// One log row as the engine hands it out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub id: i64,
    pub node: Option<String>,
    pub variant: Option<String>,
    pub stream: String,
    /// RFC 3339 UTC, as stored.
    pub ts: String,
    pub line: String,
    /// Under `search`: false for a context line that does not itself match.
    #[serde(default = "yes")]
    pub is_match: bool,
    /// Under `search`: lines were skipped between the previous row and this one.
    #[serde(default)]
    pub gap_before: bool,
}

fn yes() -> bool {
    true
}

/// What `GET /api/engine/logs` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsPage {
    /// The resolved environment name.
    pub run: String,
    /// The resolved run's full id; pin follow polls to it.
    pub run_id: String,
    pub short_id: String,
    /// Whether the run still occupies the live slot. Follow stops when it ends.
    pub live: bool,
    /// One-line outcome (`crashed (api:local pid died)`), for an ended run.
    pub outcome: String,
    pub rows: Vec<LogLine>,
    /// Snapshot: the highest log id *before* the read, so nothing written during
    /// it is lost; rows past it were already shown. Follow: the highest id seen.
    /// Pass it back as `after_id`.
    pub last_id: i64,
    /// `logs.timeZone`: `local` or `utc`.
    pub time_zone: String,
    /// The "Using environment …" line to show when the name was inferred.
    #[serde(default)]
    pub note: Option<String>,
}

/// One run that is up right now, as the bare `veld` overview lists it.
///
/// Deliberately thin: the overview answers "what is running", and a face that wants
/// URLs or node health asks `status` for that run. `run_id` is a string so a face
/// need not link `uuid` to print it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningRun {
    pub project_name: String,
    pub project_root: PathBuf,
    /// The environment's name (`dev`, a worktree alias, ...).
    pub name: String,
    pub run_id: String,
    /// A `RunStatus` spelling, as a string: this body crosses versions (an older
    /// CLI reads a newer daemon's), and a status that CLI does not know must not
    /// fail the whole overview.
    pub status: String,
}

/// The `GET /api/engine/overview` body: bare `veld`'s picture of the machine.
///
/// One type for both ends — the daemon serializes it, the CLI deserializes it — so
/// the two cannot drift into a body the CLI reads as "the daemon is older".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub version: String,
    /// When this daemon started (RFC 3339). Agent state lives in its memory, so an
    /// empty `agents` means "none reported since then", not "none".
    pub started_at: String,
    pub runs: Vec<RunningRun>,
    /// Needing the human first; see [`Attention`].
    pub agents: Vec<OverviewAgent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewAgent {
    pub session_id: String,
    pub worktree_id: i64,
    /// The title the IDE rail shows: the free-text name, else the alias. `null`
    /// when the worktree is no longer in the database.
    pub worktree_name: Option<String>,
    pub project_name: Option<String>,
    /// `claude`, `codex` or `pi`.
    pub tool: String,
    /// The `veld_core::agent::State` spelling the daemon holds for the session —
    /// the hook's report after the daemon's own reading of it (a `settled`
    /// during a delegated wait stays `delegated`, for one).
    pub state: String,
    /// What `state` asks of the human. Computed by the daemon so every face reads
    /// the same answer instead of re-deriving it from `state`.
    pub attention: Attention,
    pub held: bool,
    /// When `state`/`held` last changed (RFC 3339).
    pub since: String,
}

/// What an agent's state asks of the human, in the order the overview lists them.
///
/// The one definition of "needs you": `blocked` (a permission prompt, a question)
/// stalls the agent until a person answers. An agent sitting at its prompt — its
/// turn ended (`idle`), the reminder after an interrupted turn (`settled`), or just
/// launched (`ready`) — is the human's turn: nothing is stuck, but nothing moves
/// either. The IDE draws the same split: a blocked pane badges amber, an idle one
/// files "finished".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    NeedsYou,
    YourTurn,
    Working,
    Quiet,
    /// A kind a newer daemon added that this build does not know. Without it one
    /// new variant would fail the whole body, and an older CLI would report the
    /// running runs as unknown too.
    #[serde(other)]
    Other,
}

impl Attention {
    pub fn of(state: veld_core::agent::State) -> Self {
        use veld_core::agent::State;
        match state {
            State::Blocked => Self::NeedsYou,
            State::Idle | State::Settled | State::Ready => Self::YourTurn,
            State::Working | State::Delegated => Self::Working,
            State::Done | State::Unknown => Self::Quiet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one definition of "needs you" (see [`Attention`]). `Attention::of` is
    /// an exhaustive match, so a new `State` variant fails to compile there until
    /// somebody decides what it asks of the human; this pins today's answers.
    #[test]
    fn only_blocked_needs_the_human() {
        use veld_core::agent::State;
        for (state, want) in [
            (State::Blocked, Attention::NeedsYou),
            (State::Idle, Attention::YourTurn),
            (State::Settled, Attention::YourTurn),
            (State::Ready, Attention::YourTurn),
            (State::Working, Attention::Working),
            (State::Delegated, Attention::Working),
            (State::Done, Attention::Quiet),
            (State::Unknown, Attention::Quiet),
        ] {
            assert_eq!(Attention::of(state), want, "{state:?}");
        }
        assert_eq!(
            serde_json::to_string(&Attention::NeedsYou).unwrap(),
            "\"needs_you\""
        );
        assert_eq!(
            serde_json::from_str::<Attention>("\"a_kind_from_the_future\"").unwrap(),
            Attention::Other
        );
    }

    #[test]
    fn an_old_reader_survives_a_new_event_kind() {
        let line = r#"{"seq":7,"event":{"type":"something_new","x":1}}"#;
        let env: EventEnvelope = serde_json::from_str(line).unwrap();
        assert_eq!(env.seq, 7);
        assert!(matches!(env.event, EngineEvent::Unknown));
    }

    #[test]
    fn an_old_reader_survives_a_new_outcome() {
        let line = r#"{"type":"finished","outcome":{"result":"teleported"}}"#;
        let ev: EngineEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(
            ev,
            EngineEvent::Finished {
                outcome: ActionOutcome::Unknown
            }
        ));
    }

    #[test]
    fn a_request_round_trips_and_carries_no_secret_field() {
        let req = ActionRequest {
            project_root: "/p".into(),
            kind: ActionKind::Start {
                run: "dev".into(),
                selections: vec!["api:local".into()],
                origin_preset: Some("dev".into()),
                oneshot: None,
                all_logs: false,
            },
            debug: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("env"), "{json}");
        assert!(!json.contains("answer"), "{json}");
        assert_eq!(serde_json::from_str::<ActionRequest>(&json).unwrap(), req);
    }

    #[test]
    fn the_finished_tag_is_what_serde_writes() {
        let json = serde_json::to_string(&EngineEvent::Finished {
            outcome: ActionOutcome::Cancelled,
        })
        .unwrap();
        assert!(json.contains(FINISHED_TAG), "{json}");
    }

    #[test]
    fn stop_all_round_trips() {
        let json = serde_json::to_string(&ActionKind::StopAll).unwrap();
        let back: Result<ActionKind, _> = serde_json::from_str(&json);
        assert_eq!(back.unwrap(), ActionKind::StopAll, "{json}");
    }

    #[test]
    fn an_old_engine_reads_a_new_action_kind_as_unknown() {
        let req: ActionRequest =
            serde_json::from_str(r#"{"project_root":"/p","kind":{"kind":"pause","run":"x"}}"#)
                .unwrap();
        assert_eq!(req.kind, ActionKind::Unknown);
        assert!(!req.kind.gates_recovery());
        assert!(!req.kind.cancellable());
    }

    #[test]
    fn exit_codes_match_the_cli_contract() {
        assert_eq!(ActionOutcome::Cancelled.exit_code(), 130);
        assert_eq!(
            ActionOutcome::OneshotExited {
                exit_code: 3,
                label: String::new()
            }
            .exit_code(),
            3
        );
        assert_eq!(
            ActionOutcome::Failed {
                message: String::new(),
                kind: FailureKind::Error,
                failed_node: None
            }
            .exit_code(),
            1
        );
    }
}
