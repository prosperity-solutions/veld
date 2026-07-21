# Environments × Runs — run history as a first-class concept

Status: **Draft RFC** · Owner: Peter · Branch: `split-runs-into-two-concepts`

Today a "run" in veld conflates two things: the **named slot** you start, stop,
share, and talk about (`--name dev`), and the **execution instance** that
actually lived, produced logs, and died. This RFC splits them — in OOP terms,
the environment is the class, a run is an object:

- **Environment** — the durable identity: `(project_root, name)`. Survives
  stop/start cycles. What `veld start --name dev` names, what the UI shows a
  card for, what feedback threads and shares attach to.
- **Run** — one execution instance of an environment: a `run_id`, a start and
  end time, an outcome, and its own log scope. Immutable once ended.
  Accumulates as **history**.

**Why:** when an environment stops — or worse, crashes — its run row is deleted
outright and the only trace left is orphaned log rows that no CLI or UI path can
reach. There is no way to answer "why did last night's run die?" The whole point
of this split is that *investigating a failure after the fact* becomes a normal,
supported workflow instead of a race against GC.

---

## 1. Where we are today

All facts verified against current code (2026-07-21):

| Concern | Today | File |
|---|---|---|
| Run identity | `UNIQUE(project_root, name)` on `runs`; `run_id: Uuid` exists on `RunState` but is regenerated every start and is never a lookup key **in the DB layer** — every query keys `(project_root, name)`. (The share subsystem does key teardown by `run_id` — `unshare_run`, `DELETE /api/shares/by-run/{run_id}` — so the UUID already has one consumer that treats it as instance identity.) | `crates/veld-core/src/db/mod.rs:279-385`, `crates/veld-core/src/state.rs:151-163`, `crates/veld-daemon/src/share/manager.rs:1078-1094` |
| `veld stop` | Hard-deletes the run row — "Remove the run from state entirely (no lingering stopped state)" | `crates/veld-core/src/orchestrator.rs:1068-1069` |
| Restart of same name | `cleanup_stale_run` kills PIDs and hard-deletes the old row; or the `ON CONFLICT(project_root, name) DO UPDATE` upsert overwrites every column including `run_id` | `orchestrator.rs:1080-1117`, `crates/veld-core/src/db/state.rs:204-278` |
| Orphan reaping | **Three divergent implementations**: orchestrator `cleanup_dead_runs` deletes the row; `veld gc` marks `Stopped` and keeps it; daemon GC + monitor mark `Stopped` and keep it | `orchestrator.rs:1122-1169`, `crates/veld/src/commands/gc.rs:55-84`, `crates/veld-daemon/src/gc.rs:99-143`, `crates/veld-daemon/src/monitor.rs:47-170` |
| Logs | Single `log_lines` table keyed by `(project_root, run_name)` **strings** — no run generation, no FK to `runs` (deliberately: logs must outlive the row). A reused name interleaves old and new lines; the comment at the write path acknowledges "a reused run name shows the previous run's log lines until then" | `crates/veld-core/src/db/mod.rs:322-333`, `db/state.rs:280-284` |
| Logs after stop | Rows survive `stop` (nothing deletes them), but both read paths (`veld logs`, `GET /api/logs/{run}`) resolve the run **by state entry first** → once the row is gone, logs are unreachable, though they physically live up to 168 h | `crates/veld/src/commands/logs.rs:97`, `crates/veld-daemon/src/management.rs:342-348` |
| Exit codes | Command/oneshot exits land in `node.outputs["exit_code"]` while the run lives, then die with the row; server processes are never `waitpid`ed (death detected via `kill(pid,0)`), so their exit status is genuinely unobservable | `orchestrator.rs:763,813,2143`, `monitor.rs:99-124` |
| History surface | None. No CLI command, no UI view, no API shape for past runs | — |
| Run statuses | `Recovering` and `Failed` are defined but never assigned to a persisted run — recovery is tracked per-node, failure evaporates with the deleted row | `state.rs:19-27`, per workspace grep |

The asymmetry worth naming: **logs already survive stop; run identity doesn't.**
The data for post-mortems is mostly there — it's the *addressability* that gets
destroyed. That's why this is an identity-model fix, not a logging feature.

## 2. Goals / non-goals

**Goals**

- A stopped, failed, or crashed run leaves a permanent (retention-bounded)
  record: when it ran, how it ended, what failed, and its logs — reachable by
  CLI and UI without racing GC.
- Logs are scoped per run instance. Restarting `dev` no longer interleaves last
  week's lines into today's tail.
- One ending semantics. All lifecycle-ending paths (stop, start-failure, crash
  detection, orphan reap, same-name replacement) converge on a single guarded
  "finalize run into history" operation with an explicit end reason.
- History is discoverable: `veld runs` lists it, `veld logs` can target it, the
  management UI exposes it per environment card.
- Coding-agent ergonomics: everything above has `--json`, and a run's outcome
  (end reason + failure detail) is machine-readable at the **run** level — an
  agent can diagnose "why did the env I started die?" without a human.

**Non-goals (this RFC)**

- Snapshotting resolved config into the run record (sketched as increment 3;
  config stays re-read-from-disk as today).
- Re-keying feedback threads to runs — feedback stays environment-scoped; a
  conversation about the app outlives any one process lifetime.
- Historical resource stats (`node_stats` stays a live-sampling concern; rows
  die with their run's retention).
- Recovering exit codes for crashed **servers** — veld never `waitpid`s
  detached servers, so their OS exit status does not exist to record. A crash
  record says *which node's PID died when*, not its exit code.
- Share/history integration (share teardown already keys by `run_id` and keeps
  working; a stopped share's gateway UX was handled in #147).

## 3. Data model

### 3.1 Schema (migration v3, append-only per `db/mod.rs:253-257`)

```sql
-- The class: durable named slot.
CREATE TABLE environments (
    id           INTEGER PRIMARY KEY,
    project_root TEXT NOT NULL REFERENCES projects(root) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    created_at   TEXT NOT NULL,          -- first time this name was started
    UNIQUE(project_root, name)
);

-- The object: one execution instance.
CREATE TABLE runs (
    id              INTEGER PRIMARY KEY,             -- keeps nodes.run_row FK shape
    environment_id  INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    run_id          TEXT NOT NULL UNIQUE,            -- the existing UUID, now a real key
    status          TEXT NOT NULL,                   -- starting|running|stopping|stopped|failed|crashed
    end_reason      TEXT,                            -- NULL while live; see §3.3
    end_detail      TEXT,                            -- JSON: {failed_step?, failed_node?, exit_code?, message?}
    execution_order TEXT NOT NULL DEFAULT '[]',
    created_at      TEXT NOT NULL,
    ended_at        TEXT
);
CREATE INDEX idx_runs_env ON runs(environment_id, created_at);

-- The one-live-run invariant, enforced by the engine, not by application code:
CREATE UNIQUE INDEX idx_runs_one_live ON runs(environment_id)
    WHERE status IN ('starting','running','stopping');

-- log_lines: gains instance scoping. Legacy rows stay NULL.
ALTER TABLE log_lines ADD COLUMN run_id TEXT;
CREATE INDEX idx_log_lines_run_id ON log_lines(run_id, id);
```

**Migration mechanics (the hazardous part, spelled out).** SQLite can't alter
constraints, so `runs` must be rebuilt — and `PRAGMA foreign_keys=OFF` is a
no-op inside the migration's `BEGIN IMMEDIATE` transaction (`db/mod.rs:146,
213`), so a naive `DROP TABLE runs` would cascade-delete every `nodes` and
`node_stats` row through their `ON DELETE CASCADE` FKs. The migration therefore
rebuilds **all three tables** in dependency order: create `environments` +
`runs_new` + `nodes_new` + `node_stats_new`, copy rows (each old `runs` row
becomes one `environments` row plus one run row keeping its existing
`run_id`/status/timestamps; `nodes`/`node_stats` copy with remapped `run_row`),
then drop old and rename. `nodes` additionally loses nothing and gains nothing
— its shape is unchanged. Row copy normalizes statuses: any legacy row whose
status falls outside the new value set (`recovering`, which is never assigned
in practice but is parseable) becomes `stopped` with
`end_detail.message = "normalized by v3 migration"` — otherwise such a row
would sit outside both the live set and every reaper's gate forever. Old `log_lines` keep `run_id = NULL` and remain
reachable via the legacy `(project_root, run_name)` filters until age-pruned —
no retroactive generation attribution (the data isn't there). The
`idx_log_lines_run_id` build is a one-time scan of a table that can hold a week
of logs; acceptable inside the existing 60 s migration busy budget, but the
migration runs `prune_logs_older_than` *first* to shrink the table before
indexing.

**Upgrade window:** the daemon opens the DB per pass and fails closed on
`NewerSchema` (`db/mod.rs:200-205`), so between `veld update` migrating to v3
and the daemon binary being restarted, crash detection is blind. `veld update`
must restart the daemon after a successful migration — same deterministic
restart discipline as the privileged helper in #153.

### 3.2 In-memory types

`RunState` splits accordingly:

- `EnvironmentState { name, project_root, created_at, current_run: Option<RunState> }`
  — what name-based lookups resolve to.
- `RunState` keeps its fields but `run_id` becomes the durable key; gains
  `end_reason: Option<EndReason>` and `end_detail: Option<EndDetail>`;
  `stopped_at` → `ended_at` internally (JSON compat in §7).
- `GlobalRegistry` stays a derived view (`db/state.rs:320-369` pattern), now
  `projects → environments → current run + last-N history summaries`.

**The write path is the load-bearing change, not the schema.** Today's single
writer `Db::save_run` is an `INSERT ... ON CONFLICT(project_root, name) DO
UPDATE` (`db/state.rs:220-238`) — it stops even *parsing* once that unique
constraint moves to `environments`. Increment 1 rewrites it as: upsert the
`environments` row, then upsert the run **keyed by `run_id`**, with two guards:

1. `save_run` refuses to touch a terminal run — and "refuses" means it bails
   out of the **whole transaction**, not just the status column. `save_run`
   wholesale-rewrites node rows (`DELETE FROM nodes WHERE run_row=?` +
   re-insert, `db/state.rs:240-273`); if only the run-row upsert were guarded,
   a monitor read-modify-write landing after a finalize (`monitor.rs:186` …
   `save_run` at 402) would no-op the status yet still overwrite the ended
   run's final node states with stale live-era data — corrupting exactly the
   forensic snapshot this feature exists to keep. The transaction checks the
   stored status first and no-ops entirely when it's terminal.
2. Ending a run is a **two-phase protocol**, and the intent is persisted
   *before* anything is killed:
   - `begin_ending(run_id, end_reason, end_detail)` — guarded
     `UPDATE runs SET status='stopping', end_reason=?, end_detail=?
     WHERE run_id=? AND status IN ('starting','running')`. First ender wins
     the label. This moves the run out of the crash detectors' scan set
     (monitor and GC gate on `running`/`starting`) before its PIDs start
     dying, so a deliberate `veld stop` — which kills, then spends seconds in
     teardown steps and `on_stop` hooks (`orchestrator.rs:1022-1069`) — can no
     longer be raced by the 5 s monitor and mislabeled `crashed`. The
     **replaced** path gets the same treatment: `cleanup_stale_run` today
     kills while the old run still reads `running` (`orchestrator.rs:
     1098-1116`); it now calls `begin_ending(…, replaced)` first.
   - `finalize_run(run_id)` — after PIDs are confirmed dead and teardown ran:
     guarded `UPDATE … SET status=<terminal from end_reason>, ended_at=?
     WHERE run_id=? AND status='stopping'`.
   - Crash detection (PIDs already dead) collapses both phases into one
     guarded step whose guard is **exactly** `status IN ('starting','running')`
     — never widened to `stopping`. That constraint is what makes the protocol
     race-free: `begin_ending` commits while PIDs are still alive, so by the
     time any detector sees dead PIDs, the guard finds `stopping` and no-ops.
     Outcomes stop being a race between the 5 s monitor, the 600 s GC, and
     the CLI.
3. `remove_run` — today a name-keyed `DELETE` (`db/state.rs:285-300`) that
   under v3 would wipe an environment's **entire history** in one call — is
   retired outright. Nothing in the new model hard-deletes by name; the only
   row deletion left is the GC retention prune, keyed by `run_id` (§3.5).

**Invariant: at most one live run per environment** — enforced by the partial
unique index above, not by check-then-act application code. Two concurrent
`veld start --name dev` (or a start racing the monitor's recovery restart,
`monitor.rs:380`) can both pass an application-level check; the second insert
now fails atomically and surfaces "environment dev is already starting".

**No terminal label over live PIDs — but no permanent leaks either.** A run
should only reach a terminal status once its spawned PIDs are confirmed dead.
This matters most for the start-failure path: today a start that spawned
processes and then failed is deliberately left `Starting` so the next start's
orphan reap finds the leaked PIDs (`orchestrator.rs:595-608`, the #149 rule).
If we naively finalized it to `failed`, `is_reapable_orphan` would never look
at it again (`orchestrator.rs:108-117`) and a self-healing leak would become a
permanent one. Three rules make this hold without deadlocking the start path:

- The failure path calls `begin_ending(…, failed)`, kills its checkpointed
  PIDs, and finalizes only what is confirmed dead.
- **Stale-`stopping` reaper — grace-gated on both branches.** Dead PIDs under
  `stopping` is the *normal* state of a healthy `veld stop` (PIDs are killed
  first, then `on_stop` hooks and teardown steps run for seconds to minutes,
  `orchestrator.rs:1031-1066`) — indistinguishable in DB state from a
  SIGKILLed ender. So the reaper only acts on `stopping` runs older than a
  grace period (generous, e.g. 10 min — an ender that old is dead or hung):
  dead PIDs → finalize with the stored `end_reason`; live PIDs → re-kill,
  then finalize. Finalizing early would be worse than mislabeling: a terminal
  status releases the one-live-run slot, letting a new `veld start` interleave
  its setup with the old run's still-executing teardown hooks. Corollary: a
  second `veld start` during a slow teardown is blocked by the index for the
  teardown's duration and must surface "environment dev is stopping", not a
  raw unique-constraint error. Today `stopping` cannot persist, so nobody
  covers any of this.
- **Escape hatch + backstop:** the one-live-run index means a new
  `veld start` cannot insert its run until the old one leaves the live set —
  and an unkillable old PID (uninterruptible sleep) must not block a start
  that today always proceeds (`orchestrator.rs:1116` ignores kill failures).
  After a bounded kill wait, the replaced path finalizes anyway with
  `end_detail.message = "kill unconfirmed"`, and the GC gains a **terminal-run
  straggler sweep**: any terminal run whose recorded PIDs are still alive gets
  re-killed on each pass. Leak-freedom stops depending on the label.

**Preserved invariant** (per `orchestrator.rs:88-107` and #149): the
environment row and the new run row (status `starting`, nodes `pending`) are
persisted **before the first stage executes** — startup stays observable.

### 3.3 One finalize path, explicit end reasons

| `end_reason` | Set by | Today's behavior it replaces |
|---|---|---|
| `stopped` | `veld stop`, UI stop (the stale-`stopping` reaper finalizes with whatever reason `begin_ending` stored) | row deleted (`orchestrator.rs:1069`) |
| `failed` | start aborts mid-stage (after kill-and-confirm, §3.2); `--oneshot` terminal node exits **non-zero** | row deleted if nothing spawned, else left `Starting` forever; oneshot exit code discarded |
| `crashed` | any detector seeing a live run's PIDs dead: the 5 s monitor **and** the GC/start-time orphan sweeps | monitor marked it `Stopped` (indistinguishable from clean stop); sweeps deleted or kept inconsistently |
| `replaced` | `veld start` over a live same-name run | old row hard-deleted |
| `completed` | `--oneshot` terminal node exits **zero** | exit code discarded |

Notes:

- There is deliberately **no `reaped` reason**. The monitor (5 s) and the GC
  orphan sweep (600 s) detect the *same physical event* — dead PIDs under a
  live run — and which one fires first is timing (the monitor skips its backlog
  after macOS sleep, `monitor.rs:29`). Giving them different labels would make
  the recorded outcome nondeterministic. Both say `crashed`; `end_detail`
  carries which node died.
- `end_detail` is the machine-readable half of the outcome and lives at run
  level because the failing thing is not always a node: a setup step (`failed
  (setup: db-migrate, exit 1)`) has no node row. Shape:
  `{ failed_step?, failed_node?, exit_code?, message? }`. Command/oneshot exit
  codes continue to live in `node.outputs["exit_code"]` per node — no new
  `nodes` column; crashed servers have no exit code to record (§2 non-goals).
- The unused `RunStatus::Recovering` is removed in the same pass (recovery is
  per-node; grep shows no assignment site). Persisted terminal statuses become
  exactly `stopped | failed | crashed`.

### 3.4 Log scoping

Every write path stamps the new `run_id` column: `LogTarget::append` and the
`veld _log` wrapper get a `--run-id` arg (`process.rs:96-108`,
`veld/src/main.rs:612-660`), `LogWriter::for_run/for_node` carry it, and
client-log ingestion (`feedback_server.rs:249-401`) resolves it from the run it
already looks up. The `(project_root, run_name)` columns stay — they remain the
environment-scope filter and keep legacy rows readable.

Read paths gain an instance filter: default = latest run's `run_id`;
`run_id IS NULL` legacy rows appear only under `--all-runs` (§4.2). Follow-mode
watermarks are untouched — `id` stays the global monotonic cursor.

Late writers are tolerated by design: a detached `veld _log` wrapper carries
its `run_id` for its whole lifetime and may insert rows after its run was
pruned. Those rows are unreachable garbage until the 168 h age prune — same
posture as today's never-fatal `_log` (`main.rs:622-627`), no coordination
added.

### 3.5 Retention

- **Run history cap:** keep the last **10** ended runs per environment. Pruning
  runs in the existing 600 s daemon GC pass — **not** in `finalize_run`. Stop
  and crash-detection are latency-sensitive paths; cap enforcement is not
  urgent (an 11th row for ten minutes is harmless), so finalize stays a single
  guarded UPDATE.
- Pruning a run cascades `nodes`/`node_stats` by FK; `log_lines` has **no FK**
  (deliberately — logs outliving state rows is the current design,
  `db/state.rs:280-284`) so the GC issues an explicit
  `DELETE FROM log_lines WHERE run_id = ?`.
- **Age:** the existing 168 h log prune (`veld-daemon/src/gc.rs:18`) stays and
  now also prunes ended runs older than 168 h. The 72 h *entry* prune
  (`MAX_ENTRY_AGE_HOURS`) is deleted — it exists to sweep lingering stopped
  rows, which are now a feature, and it's the reason logs today become
  unreachable 96 h before they're deleted.
- Environments with zero runs after pruning are dropped (mirrors today's
  "projects row deleted when last run goes", `db/state.rs:293-297`).
- Cap is a constant first; a config knob (`history: { keep: N }`) only if
  someone actually asks.

## 4. CLI shape

Principle: **`--name` keeps meaning the environment.** Runs are addressed by a
new, orthogonal selector. (This does *not* mean zero behavior change — the
honest list of breaks is §7.)

### 4.1 `veld runs` — becomes the history view

Today it lists active run entries (`commands/runs.rs`) — a near-duplicate of
`veld list`. It becomes the run-instance listing, which is what the name always
wanted to mean:

```
$ veld runs --name dev
RUN       STARTED            ENDED     DURATION  OUTCOME
a3f8c12   today 09:12        —         2h 4m     running
9b01d77   today 07:55        09:11     1h 16m    replaced
4e2a9f0   yesterday 18:02    18:02     0s        failed (setup: db-migrate, exit 1)
77c0b1e   yesterday 09:14    17:58     8h 44m    stopped
1d9e884   2 days ago 09:30   14:11     4h 41m    crashed (api:local pid died)
```

Without `--name`: all environments' runs, grouped. `--json` emits, per run:
`{ name, run_id, status, end_reason, end_detail, created_at, ended_at,
nodes: [{ node, variant, status }] }` — `name` keeps meaning the environment
name as today. Short-id prefix matching like git (`a3f8c12` → full UUID).

### 4.2 `veld logs` — run selector

- Default scope narrows from "everything under this name" to **the latest
  run**. This fixes the generation-interleaving bug and matches what everyone
  already believes the command does — but it *is* a behavior change: after a
  restart, `veld logs --lines 500` no longer reaches into the previous
  generation. `--all-runs` restores the old interleaved behavior (and is the
  only way to see legacy `run_id IS NULL` rows).
- `--run <id-prefix>` — a specific historical run.
- `--previous` / `-p` — the run before the latest. Note the semantics: a
  crashed run *is* the latest, so the default already covers "it just died";
  `--previous` answers "what did the run before this restart look like".
- Everything composes with the existing `--source/--search/--since/--json`.
- `--follow` semantics: on an already-ended run, print history and exit 0 with
  a stderr note. On a live run that **ends mid-follow**, detect the terminal
  transition and exit 0 the same way — today's follow loop polls forever
  (`logs.rs:289-336`) and would hang an agent waiting on a crashed run.
  stdout stays pure log payload in both cases.

### 4.3 Everything else

- `veld start/stop/restart/status/urls/action/share` — same surface; they
  address the environment and operate on its current (or, for stopped
  environments, last) run. `veld status` prints the current run's short id and,
  when the environment is stopped, the last run's outcome line.
- **Stale URLs must not masquerade as live.** Stop tears down Caddy/DNS routes,
  so a stopped environment's last-run node URLs are dead. Today this can't
  mislead anyone (the row is gone); with history it can — an agent would curl a
  404 believing the env is up. `veld status` on a non-live run suppresses the
  URL column (outcome line instead), and `veld urls` on a stopped environment
  errors with "environment dev is stopped (last run ended …)" rather than
  printing dead URLs. `--json` mirrors this: `urls` empty + `live: false`.
- Name resolution (`resolve_run_name`, `commands/mod.rs:51-116`) needs two
  deliberate fixes, not just a rename:
  1. Its "active" predicate is `status != Stopped` (mod.rs:62-67). With
     `crashed`/`failed` now persisting, that predicate would count dead
     environments as active and poison the "pick the sole active run"
     auto-selection. Active becomes: has a live run (`end_reason IS NULL`).
  2. `veld restart` and `veld stop` currently exclude stopped runs from the
     sole-run fallback (`include_stopped=false`). With environments persisting,
     both keep the existing **two-tier** resolution (mod.rs:62-99) but flip the
     fallback on: prefer the sole *live* environment; only when zero are live,
     fall back to a sole environment. The tiers must not be collapsed —
     "regardless of state" would make a crashed `dev` + running `staging`
     answer "Multiple environments found" to a bare `veld stop`, breaking
     "stop the one running thing." (Corollary: "dev crashed overnight →
     `veld restart`" auto-resolves only when dev is the sole environment; with
     staging also live it correctly requires `--name dev`.)
- No new top-level nouns. No `veld envs`, no `veld history` — `veld list`
  (environments) and `veld runs` (instances) cover the two concepts.
- `veld list` shows environments including stopped ones (that *is* the
  discoverability ask), with the last outcome. Environments are deduplicated by
  name, so this is bounded — a project shows one row per name ever used within
  retention, not one per crash.

## 5. Management UI + API

- `/api/environments` (`management.rs:152-215`): each entry becomes an
  environment carrying `current_run` (shape as today) plus
  `history: [{ run_id, created_at, ended_at, end_reason, end_detail, status }]`
  (last 10). Stopped environments **now appear** (status `stopped`, last
  outcome shown) — today they vanish from the dashboard entirely.
- `/api/logs/{name}` gains `?run_id=` (default: latest), same clamps and CSRF
  posture as today.
- UI (env card, `management-ui.html:283-341`): the Logs tab gets a run picker —
  `current`, then ended runs as `outcome · started · duration` rows with a
  colored outcome dot (`stopped` gray, `crashed`/`failed` red, `replaced` dim).
  Services tab always shows the current run; for a stopped environment it shows
  the last run's final node states instead of an empty card. Restart stays one
  click away — that's the actual "it crashed at 3am" workflow: open card, see
  red dot, read logs, restart.
- No new page, no routing. History lives inside the existing card, consistent
  with the one-page UI.

## 6. Increments

1. **Core split** — migration v3 (three-table rebuild, §3.1), the `save_run`
   rewrite + guarded `finalize_run` (§3.2 — this is the load-bearing change),
   `EnvironmentState`/`RunState` split, end reasons + `end_detail`, log
   `run_id` stamping, retention in GC (cap 10 + 168 h, drop the 72 h entry
   prune), the two `resolve_run_name` fixes, `veld runs` history +
   `veld logs --run/--previous/--all-runs` + follow-exit-on-end, oneshot
   pass/fail end reasons, daemon restart after migration. Ships as one PR —
   the schema, the write path, and the finalize unification aren't separable
   without shipping an inconsistent intermediate state.
   **During increment 1, `/api/environments` stays filtered to live runs** so
   the untouched UI never renders dead cards it has no controls for.
2. **UI history** — `/api/environments` history shape + stopped-environment
   entries, run picker in the Logs tab, stopped cards with restart, outcome
   dots.
3. **(Sketch, separate RFC-let) Config forensics** — snapshot the *resolved
   graph* (node keys, variants, execution order, command strings — never env
   values) into the run row at start, so "what changed between the run that
   worked and the run that didn't" is answerable. The run-identity substrate
   this RFC builds is its prerequisite.

## 7. Compatibility — the honest list

Consumed-output changes (scripts/agents parsing today's output):

| Surface | Change | Mitigation |
|---|---|---|
| `veld runs --json` | **Breaking.** Entries for ended runs appear; `nodes` changes from `[string]` to objects; new fields (`run_id`, `end_reason`, `end_detail`, `ended_at`). `name` keeps its meaning. | Filterable by `status`; called out in release notes. Not a superset — say so. |
| `veld status --json` | `stopped_at` is renamed internally to `ended_at`. | JSON emits **both** keys for now (`stopped_at` as deprecated alias); removal is a later call. `end_reason`/`end_detail`/`run_id` are additive. |
| `veld logs` default | Narrows to latest run (§4.2). | `--all-runs` restores; release notes. |
| `veld list` / registry JSON | Stopped environments appear; `.projects[].runs` becomes environment-shaped. | Bounded (deduped by name); release notes. |
| `veld logs -f` | Now exits 0 when the followed run ends instead of polling forever. | This is a fix; agents relying on the hang are broken today. |
| `veld urls` / status URLs | On a stopped environment: `urls` errors instead of "No runs found", status hides dead URLs (§4.3). | New state that previously couldn't occur; release notes. |
| `status`/`runs` status strings | `crashed` appears as a new persisted value (and `failed` as persisted, not display-only). | Additive; consumers with status allowlists must add it. |

Untouched: `veld start/stop/restart` invocation shapes, `--name` semantics,
follow watermarks, feedback (stays keyed by environment name), share teardown
(already keyed by `run_id` — the split makes its key *more* correct, since a
finalized run's shares tear down against a still-existing row).

## 8. Open questions (maintainer calls)

1. **`veld logs` default scope.** Recommended: latest run (§4.2). The
   alternative — keep interleaved-all as default — preserves stricter backward
   compatibility but preserves the bug too. Position: change it; `--all-runs`
   is the escape hatch, and the current default is only ever "correct" by
   accident.
2. **History cap.** 10 runs/env, constant. Config knob deferred.
3. **`veld runs` repurpose.** It changes meaning from "active run entries" to
   "run history", and its `--json` shape breaks (§7). Position: acceptable —
   its current output is redundant with `veld list`, and the new meaning is the
   honest one. Alternative if that's too hot: a new `veld runs history`
   subcommand and freeze the top-level output; my view is that's a worse
   long-term surface for a pre-1.0 tool.

## 9. Documentation checklist impact

User-visible surface changes (README CLI table, `skills/veld/SKILL.md` +
`reference/config.md`; `docs/configuration.md` and the JSON schema only if the
history knob ever ships). Website: nothing to sell until increment 2 ships
("crash forensics / run history" is a real capability worth a features-grid
cell then, synced to `llms-full.txt`); increment 1 alone is CLI-facing → README
+ skills only.
