# Runs and environments

An **environment** is the durable named slot (`--name dev`) — what `veld start`,
`veld stop`, and `veld status` address. A **run** is one execution instance of
an environment: it has an id, a start/end time, and an outcome. Stopping,
crashing, or replacing a run doesn't erase it — it persists as history (last 10
runs per environment, 7 days) along with its logs.

Post-mortem workflow — "why did last night's run die?":

```sh
veld runs --name dev                # list past runs for dev, newest first
veld logs --run a3f8c12             # logs for that specific run (id prefix, like git)
veld runs show a3f8c12              # full detail: node results + the graph snapshot it started with
veld runs diff a3f8c12              # config diff vs its predecessor ("what changed since it worked?")
```

Every run stores a **graph snapshot** at start: raw (pre-interpolation)
command strings, cwd, env variable *names* (never values — they can be
secrets), URL templates, and a hash of veld.json. `veld runs diff <old> <new>`
(or one id, against its predecessor) reports node added/removed and per-field
changes — the fastest answer to "did the config change between the run that
worked and the run that didn't?"

`veld runs --json` gives the machine-readable outcome: `end_reason` is one of
`stopped | failed | crashed | replaced | completed`, and `end_detail` carries
the specifics (`failed_step`, `failed_node`, `exit_code`, `message`) — a
crashed run tells you which node's process died, a failed setup step tells you
which step and its exit code. `crashed` (process died unexpectedly) is now
distinguishable from `stopped` (clean `veld stop`). A `--oneshot` run records
`completed` (exit 0) or `failed` (non-zero).


## One-off runs (`--oneshot`) — e2e tests, CI

`veld start <node> --oneshot` runs a `command` node as the run's **terminal
node**: it starts the node's dependencies, runs the node to completion
(streaming its output), then tears the whole environment down in reverse order
and exits with the node's exit code. The local/CI analog of
`docker compose run --rm --abort-on-container-exit`.

```sh
## Bring up e2e's deps (web, api, db), run the suite, tear down, exit w/ its code.
veld start e2e --oneshot
veld start e2e --oneshot --all-logs   # also interleave dependency logs (stderr)
```

- **stdout = only the terminal node's stdout.** Veld's chrome (summary,
  progress NDJSON, teardown lines) and dependency logs all go to **stderr**, so
  an agent/CI capturing stdout gets just the program output. Dep logs are
  recorded (`veld logs --node <dep>`); `--all-logs` interleaves them live.
- Ports are dynamic, so pass dep URLs into the runner via `${nodes.<node>.url}`
  in the command or its `env` (e.g. `"env": { "BASE_URL": "${nodes.web.url}" }`).
- The node **must be `command` type** (a `long_running` never exits) **and must
  terminate** — a server mistyped as `command` hangs the run. Exactly **one**
  selection is required (no multi-node preset); its deps start automatically.
- A non-zero exit (failing tests) becomes veld's own exit code — chain it:
  `veld start e2e --oneshot && deploy`. Ctrl+C aborts and exits `130`. The run
  is recorded with `end_reason: completed` (exit 0) or `failed` (non-zero,
  `end_detail.exit_code` set) — visible via `veld runs --name e2e`.
- Teardown (`on_stop` hooks, project `teardown`) always runs — on completion
  and on Ctrl+C — and runs to completion once started, except that a single
  hook or step is killed after 30s so a wedged one cannot stop the run ending.
  Deps aren't health-monitored while the node runs.

---

`veld skills` lists every topic. This document describes the veld binary that printed it — run `veld -V` if you need the version.
