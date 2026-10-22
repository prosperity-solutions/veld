# Veld basics

Veld orchestrates local dev environments. It starts services declared in
`veld.json`, wires up their dependencies, and gives each one an HTTPS URL like
`https://frontend.my-feature.myproject.localhost`.

You are reading documentation carried by the `veld` binary itself, so it
describes the veld on this machine and no other. `veld agent skills` lists the rest.

## Look things up, do not memorise them

Nothing about *this project* is in these documents — no node names, no presets,
no ports, no config. That is deliberate: it would be stale, and it would cost
you context before you knew whether you needed it. Ask when you have the
question:

```sh
veld orchestrator presets              # what this project can start, and when to use each
veld orchestrator nodes                # every node and variant, with the file and line it is defined in
veld orchestrator status               # what is running right now
veld orchestrator urls                 # the URLs of a live run
veld orchestrator config --files       # which files define which nodes (why is my node missing?)
veld <part> <verb> --help              # the flags, always current
```

Every one of those takes `--json`. `veld orchestrator presets --json` in particular carries
each preset's `when_to_use` prose, which is the project author telling you which
one to pick.

**Do not read `veld.json` to find out what you can start.** It can be tens of
thousands of characters, it can be split across `include` globs, and half of
what it says is resolved at run time anyway. `veld orchestrator presets` and `veld orchestrator nodes` are
the resolved answer and cost a hundredth as much.

## The command surface

Every command is `veld <part> <verb>`. Bare `veld` prints an overview of what
is up and who needs you, `veld --help` lists the parts, and `veld --help-all`
lists every command in one go. A retired name (the old top-level start) prints
where it moved to (``that is `veld orchestrator start` now``) and exits 2. The shape worth knowing:

| Part | Commands |
|---|---|
| `orchestrator` (alias `orc`) | `start`, `stop`, `restart`, `status`, `urls`, `logs`, `runs list`/`show`/`diff`, `stats`, `presets`, `nodes`, `graph`, `config`, `vars list`/`set`/`unset`, `lint`, `init`, `actions`, `action` |
| `ide` | `open`, `open-url`, `dashboard`, `worktree name`, `worktree new`, `worktree groups`, `pane restart` |
| `agent` | `skills` (this document), `restart` |
| `network` | `share`, `join`, `list`, `unshare`, `leave`, `approve`, `deny` |
| `self` | `setup`, `doctor`, `update`, `desktop`, `settings list`/`get`/`set`/`unset`/`describe`, `backup list`/`now`/`restore`, `projects list`, `gc`, `version`, `uninstall` |

## Five things agents get wrong

**1. A preset is `--preset`, never a positional.** `veld orchestrator start` positionals are
node selections in `node:variant` form. A preset name goes behind the flag:

```sh
veld orchestrator start --preset dev-headless --name dev   # a preset
veld orchestrator start api:local web:local --name dev     # explicit selections
veld orchestrator start api --name dev                     # a node, using its default_variant
```

`veld orchestrator start dev-headless` does not fall back to the preset — it fails as an
unknown node. (It will tell you so and point at `--preset`, but the round trip
is wasted.)

**2. `veld orchestrator start` detaches.** It returns as soon as the environment is up; it
does not stream. The run belongs to veld's daemon, not to your terminal: closing
the terminal or killing `veld` does not stop it. Pass `--attach` (`-a`) to follow
its logs afterwards — it blocks until Ctrl+C, which stops the run, so do not use
it in an agent shell. There is no `--detach`/`-d` — that is already what happens.
Ctrl+C (or SIGINT) while `veld orchestrator start`/`restart` is still bringing things up asks
the daemon to cancel and clean up what it started, and exits `130`.

**2b. The daemon starts on demand.** `start`, `stop`, `restart`, `status`,
`logs` and `action` talk to the daemon's engine; if it is not running `veld`
starts it and waits (so a first command can be slower). Exit `75` means an
update is in progress — retry shortly (`veld agent skills gotchas`). Nodes get the
daemon's environment plus the login-shell `PATH`, **not** your terminal's
environment — see `veld agent skills gotchas`.

**3. Machine-readable output is on stdout; everything else is on stderr.**
`--json` payloads and a `--oneshot` node's own output go to stdout. Progress,
status lines, warnings and errors go to stderr. So `veld orchestrator status --json | jq`
works, and a stray warning will never corrupt it.

**4. A veld config is JSONC.** `veld.json` (and `veld.jsonc` — both names are
legal, never hardcode one) may contain `//` comments and trailing commas. Strip
them before handing the file to a strict JSON parser, or better, use
`veld orchestrator config --json`, which hands you the parsed result.

**5. Veld never rewrites a user's config.** `veld orchestrator init` writes one when none
exists; nothing else edits one. If a change needs a config edit, make the edit
yourself and check it with `veld orchestrator lint`.

## Environments, runs, names

An **environment** is the durable named slot — `--name dev` — and it is what
`start`, `stop` and `status` address. A **run** is one execution of it, with an
id, a start and end time, and an outcome that survives the run ending. When a
project has several people or worktrees on it, `--name` is what keeps them
apart, so pass it rather than relying on a default.

`veld agent skills runs` has the rest, including `--oneshot` for tests and CI.

## When something does not work

`veld self doctor` first — it checks the daemon, ports, DNS and certificates and says
what is wrong. Then `veld orchestrator logs --node <node>`, then `veld agent skills troubleshooting`.

---

`veld agent skills` lists every topic. This document describes the veld binary that printed it — run `veld -V` if you need the version.
