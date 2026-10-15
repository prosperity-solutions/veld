---
name: veld
description: >
  Orchestrate local dev environments with veld. Use this skill when the user wants to
  start, stop, or restart services; check run status or logs; configure veld.json
  (nodes, services, dependencies, presets, health checks, ports, URL templates); or
  debug environment issues like port conflicts or health-check failures. Also use when
  they want to customize the Veld IDE for a project — add a status
  badge (pull request state, CI, a deploy tag), a button that opens the worktree in
  an editor, a menu of project actions, a terminal pane that runs a coding agent, a
  picker that offers that agent's earlier sessions to resume, or tell the team
  something changed. Covers any `veld` CLI command.
triggers:
  - veld
  - veld.json
  - customize the veld ide
  - ide.extensions
  - status badge in the top bar
  - show pr status in veld
  - open worktree in editor
  - ide.panes
  - add a claude or codex pane
  - resume an earlier agent session
  - list past session ids in a pane
  - start the environment
  - "*.localhost"
allowed-tools: Read, Edit, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "2.0.0"
---

# Veld

Veld orchestrates local dev environments. It starts services from `veld.json`,
wires dependencies, and gives each service an HTTPS URL like
`https://frontend.my-feature.myproject.localhost`.

## The documentation is in the CLI

This file is a pointer, on purpose. The veld binary carries its own agent
documentation, so what you read describes **the veld installed here** rather
than whichever version this file was written against:

```sh
veld agent skills            # the index: thirteen topics, one line each
veld agent skills basics     # start here — the command surface and the traps
veld agent skills <topic>    # the one you need, when you need it
```

Read `veld agent skills basics` before your first `veld` command. Fetch any other
topic only when the task reaches it; each one is a full reference and there is
no reason to pay for all thirteen.

**Do not load project state up front either.** Nothing about *this* project —
its presets, nodes, ports or config — belongs in your context until you have a
question it answers. Ask then:

```sh
veld orchestrator presets           # what this project can start, with each preset's `when_to_use`
veld orchestrator nodes             # every node and variant, and where it is defined
veld orchestrator status            # what is running now
veld <subcommand> --help
```

`veld orchestrator config` prints the whole resolved configuration and is routinely tens of
thousands of characters. Reach for `veld orchestrator presets`, `veld orchestrator nodes` or
`veld orchestrator config --files` instead; reach for `veld orchestrator config` only when you genuinely
need the document.

## If veld is not installed

Two failures mean two different things, and the fix differs:

- **`veld -V` says "command not found"** — veld is not installed. The installer
  is <https://veld.oss.life.li/get>; tell the user rather than installing
  software on their machine unasked.
- **`veld agent skills` says "unrecognized subcommand"** — veld is installed but
  predates its own documentation. `veld self update`, then try again.
- **You cannot run `veld` at all** — no shell tool, a sandbox with no execution,
  or a policy that does not grant `Bash(veld *)`. Then this file is all you have:
  say so, and read <https://veld.oss.life.li/llms-full.txt> for a description of
  veld that may not match the version installed here.

In every case, do not guess at `veld` commands in the meantime.
