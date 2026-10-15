# Reading a run: outputs, logs and resources

After starting an environment, read node outputs (database URLs, ports, credentials, etc.):

```sh
veld orchestrator status --outputs --name my-feature        # human-readable
veld orchestrator status --outputs --json --name my-feature  # machine-readable
```

`veld orchestrator status` also reports per-node resource usage (CPU % and memory, summed
over each node's whole process tree) — a `CPU`/`MEM` column in the table, and a
top-level `stats` map (`"node:variant"` → `{ cpu_percent, memory_bytes,
process_count, cpu_seconds, memory: { ... }, sampled_at }`) in `--json`. Values
are sampled by the daemon every ~5s, so they're absent (`–` / omitted) until the
first sample lands, and go absent again shortly after a node dies or the daemon
stops. The management UI shows the same figures live with a sparkline.

**Sampling covers the start phase, and who samples depends on the step type.**
Long-lived nodes (`start_server`) are sampled by the daemon from the moment they
spawn — including the whole boot-up window before the node is healthy, which is
where a dev server does most of its allocating. `command` steps (builds,
installs, codegen) are sampled every ~2s by the `veld orchestrator start` process that runs
them, because a `command` step's process is spawned, awaited and reaped inside
that command and its PID never exists anywhere else. `veld orchestrator restart` samples the
same way, since it re-runs the same steps. Three consequences worth knowing when
reading the data:

- A `command` node has **no live reading once it finishes** — it stops being
  sampled the moment it exits. Its curve is still in `veld orchestrator stats --history` and
  in the dashboard chart for the retention window; that history is the only place
  to read a build's peak.
- A step shorter than one sampling interval is represented by the single sample
  taken when it spawned, and a peak between two ticks isn't seen. This is a
  sampler, not kernel accounting. **That first sample's CPU is 0% by
  construction** — CPU is derived from the delta between two refreshes, and there
  has only been one — so for a sub-interval step, memory is the usable figure and
  the 0% is an artefact, not a measurement.
- A `docker build` reports almost nothing: the work happens in
  `dockerd`/`buildkitd`, which are not descendants of the step's process.

### Detailed resources: `veld orchestrator stats`

`veld orchestrator status`'s `MEM` column is the tree's **footprint**, not RSS. This matters
when reading the numbers: `memory_bytes` in `--json` is RSS summed over the
tree, which counts every page shared *inside* the tree once per process — a
five-process `npm run dev` reports far more than it occupies. Use
`memory.footprint` (proportional set size on Linux, `phys_footprint` on macOS),
which is the only memory figure that sums correctly over a tree.

`veld orchestrator stats` is the detailed view:

```sh
veld orchestrator stats --json --name my-feature                      # breakdown per node
veld orchestrator stats --processes --json --name my-feature           # + one row per subprocess
veld orchestrator stats --history --window 1h --json --name my-feature # + bucketed history
veld orchestrator stats --history --cpu --window 1h --name my-feature   # CPU instead of memory
## Is this node leaking, and which child? A leak is a TREND, so --history is
## what answers it — a single reading only tells you the value is large.
veld orchestrator stats --node web --memory private_dirty --processes --history --window 1h
```

`--json` gives, per node: `cpu_percent`, `cpu_seconds` (cumulative),
`process_count`, `resident`, and a `memory` object with `footprint`,
`virtual_bytes`, and the page classes `private_clean`/`private_dirty`/
`shared_clean`/`shared_dirty`/`swap`/`wired`. A page class is `null` where the
platform can't measure it — **`null` means "not measurable here", never zero**,
so don't sum or chart it as 0. Linux reports the full split
(`/proc/<pid>/smaps_rollup`); macOS reports totals plus `wired` only. The
top-level `available_metrics` tells you which are usable without probing each
node. `--processes` adds a `processes` array (`pid`, `parent_pid`, `depth`,
`name`, `cmd`, `cpu_percent`, `cpu_seconds`, `memory_bytes`, `memory`) in
pre-order — indent by `depth`, since the parent may be absent (the sampler
records at most 64 processes per node, keeping the heaviest). `--history` adds
`history` buckets averaged server-side; a bucket with no samples is **omitted,
not zero-filled**, so consecutive entries are not necessarily adjacent in time.
Each bucket carries `cpu_percent` and `cpu_peak` as well as the memory fields, so
one request answers both dimensions — `--cpu` only changes which one the terminal
sparkline draws. Use `cpu_peak`/`footprint_peak` when `samples > 1`: a mean over a
wide bucket hides the spike a 5s sample caught.

Which memory number answers which question:

| question | metric |
|---|---|
| what does this node cost the machine? | `footprint` |
| is it leaking? | `private_dirty` **with `--history`** — one reading shows size, only a rising trend shows a leak |
| why does `top` say 4 GB? | `virtual` / `resident` |
| is it thrashing? | `swap` climbing while `resident` is flat |
| which subprocess is it? | `--processes` |
| is it burning CPU, and in bursts? | `cpu_percent` vs `cpu_peak` (`--cpu` to graph it) |

Retention: node totals 24h, per-process rows 2h — the API reports both
(`retention_secs`, `process_retention_secs`) so a client never has to hardcode
them. A by-process view over a window longer than the per-process horizon is
legitimately empty for the older part of the range.

Two escape hatches. **There are two samplers, but both run on the daemon's side** —
the daemon samples `long_running` nodes itself, and the engine worker the daemon
spawns samples `command` steps — so the daemon's service environment is the one
place to set a switch. (Older veld sampled `command` steps in the CLI, which
needed the variable in your shell too; that no longer matters.)

> A plain `export VELD_STATS_CMDLINE=off` in your shell does **not** reach the
> daemon, and so reaches no node or worker. It runs as a launchd LaunchAgent
> (macOS) or a `systemd --user` unit (Linux); neither inherits an interactive
> shell's environment, and the engine workers the daemon spawns inherit the
> daemon's. Set it where the daemon can see it, then restart the daemon:
>
> ```sh
> # macOS: set it, then restart the agent so it picks it up
> launchctl setenv VELD_STATS_CMDLINE off
> launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon"
>
> # Linux
> systemctl --user set-environment VELD_STATS_CMDLINE=off
> systemctl --user restart veld-daemon
> ```
>
> Verify with `veld orchestrator stats --processes --json`: with argv capture off, every
> process's `cmd` is `null` while `name` still reports.

| variable | effect |
|---|---|
| `VELD_STATS_MEMORY_DETAIL=off` | Fall back to RSS-only sampling. For a process with a pathological number of memory mappings, where reading `smaps_rollup` is not cheap. `footprint` then equals RSS and every page class reports `null`. |
| `VELD_STATS_CMDLINE=off` | Stop recording each process's argv. The process *name* is still recorded. veld's own rules forbid secrets on a command line because the process table is world-readable — but on macOS argv is restricted to the owning uid, so recording it does move that data into the database and the daemon's localhost API. On by default (a command line is often the only way to tell two `node` children apart); this turns it off. |

`veld orchestrator status --json` additionally carries `live` (whether the environment
occupies the live run slot), `end_reason`/`end_detail` (populated once the run
has ended), and `ended_at` (also emitted as the deprecated alias `stopped_at`,
for scripts written against the old shape).

**What a run was started from** lives at
`graph_snapshot.started_from = {preset, selections}` in `veld orchestrator status --json` and
`veld orchestrator runs show <id> --json`. `preset` is the config name (absent for an
explicit-selection start), and `selections` is the sorted `node:variant` set that
name expanded to *at start time*. The expansion is stored beside the name because
presets are re-read from disk on every use, so the name alone can be stale.

**Which surface answers "is the live run still what this preset means?"**: the
human `veld orchestrator status` and `veld orchestrator runs show` do — they re-expand the preset and print
`Started from: preset \`x\` (redefined since start)`, `(no longer defined)`, or
`(cannot be expanded — see \`veld orchestrator lint\`)`. The `--json` shapes carry the *record*
(`started_from`) but not that verdict, and `veld orchestrator presets --json` gives raw
`selections` with `@preset` refs unexpanded — so an agent that wants the
comparison should read the human line, or diff two runs with
`veld orchestrator runs diff <old> <new> --json`, which reports `origin_changed`. Do not
compare `started_from.selections` against `veld orchestrator presets --json` output directly:
they are different shapes and will disagree for every preset written without
explicit variants. `started_from` is absent on runs started by a veld older than
this feature.

To debug liveness probe failures and recovery decisions:
```sh
veld orchestrator logs --source internal --name my-feature     # shows probe stderr, recovery attempts
veld orchestrator logs --source internal -f --name my-feature  # follow mode
```

Log sources, and where each kind of output lands:

| `--source` | Contains |
|---|---|
| `server` | Node output — both `long_running` processes and `command` steps (a `docker build`'s progress is here, under that node). Read one node with `--node <name>` |
| `setup` | Project-level `setup`/`teardown` step output, labelled `setup:<step name>` |
| `internal` | Liveness probe **transitions** and recovery decisions — see below |
| `all` (default) | All of them, interleaved by timestamp |

**Two `internal`-stream lines that mean something specific.**

- `[log] dropped N line(s) from <node>:<variant>` or `[log] rate limit: …` — Veld
  did not write that node's output to the database and **lost N lines**. It is on the `internal`
  stream rather than the node's own precisely so it cannot be forged by the
  process being watched: if you see it on `server`, the program printed it. Treat
  a gap in a node's log as explained only when this line is present. Two wordings:
  *the database was not keeping up* is a full write queue; *rate limit* means the
  node printed more than **200 lines/s** after a **2,000-line burst**, which is
  the ingest budget of a detached run (a plain `veld orchestrator start` — with or
  without `--attach` — one started from the IDE, or a `--oneshot` run's dependencies) — it exists so one crash-looping
  process cannot write millions of rows an hour. A rate-limit notice is followed
  by the **last 50 dropped lines** with their original timestamps, so the error a
  crash loop ends on is still there. Only a `--oneshot` run's own terminal node is not rate limited.
  `veld orchestrator start --attach` follows a detached run, so it is budgeted.
  Both notices carry no node, and `--node` hides the `internal` stream, so read
  them without `--node`.
- `database reports as damaged — skipping log retention and page reclaim` from
  `veld self gc`, and the matching daemon warning. While the database reads as damaged
  Veld deliberately stops pruning logs, reclaiming pages and emptying the worktree
  trash, so `veld self gc` reporting `0` pruned is not a bug — run `veld self doctor` and
  `veld self backup restore`. The database can grow in the meantime.

**A quiet `internal` stream means healthy, not broken.** The liveness prober logs
*changes*: a probe that starts failing, a node that recovers, a probe that cannot
run, a recovery attempt. A node whose probe simply keeps passing writes one
"probe passing" line an hour and nothing else. It used to write two lines per
node per poll — on one real machine that was 22.7% of every log row in the
database, all of them the same sentence — so do not read a long gap between
`internal` lines as the prober having stopped. If you want to know a node is
being probed right now, read its status (`veld orchestrator status --json`), not its log
volume.

**Timestamps.** Lines are stored in UTC and printed in the machine's **local** time
zone, so a human reading `veld orchestrator logs` sees the clock on their wall. `--utc` prints the
stored value verbatim, `--local` forces local, and either overrides the `logs.timeZone`
setting for that command. **`--json` always emits UTC RFC 3339** regardless of the
flags or the setting — parse `timestamp` and convert on your side rather than reaching
for `--local`, which does nothing to JSON output.

Step output is recorded verbatim and never redacted, so a node or step that
echoes a secret from its environment puts it in that run's log. A `command`
step's stdin is `/dev/null` — one that prompts fails on EOF instead of hanging.

**Outputs can change after a recovery restart.** When a liveness probe triggers recovery (e.g., SSH tunnel drops and the DB clone restarts), the restarted node may produce new outputs (different port, new password, new connection string). Always re-read outputs with `veld orchestrator status --outputs` after a restart rather than caching them. If you observe connection failures to a previously-working service, check whether a recovery happened and refresh your outputs.

---

`veld agent skills` lists every topic. This document describes the veld binary that printed it — run `veld -V` if you need the version.
