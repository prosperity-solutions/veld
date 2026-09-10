# Veld basics

Veld orchestrates local dev environments. It starts services declared in
`veld.json`, wires up their dependencies, and gives each one an HTTPS URL like
`https://frontend.my-feature.myproject.localhost`.

You are reading documentation carried by the `veld` binary itself, so it
describes the veld on this machine and no other. `veld skills` lists the rest.

## Look things up, do not memorise them

Nothing about *this project* is in these documents — no node names, no presets,
no ports, no config. That is deliberate: it would be stale, and it would cost
you context before you knew whether you needed it. Ask when you have the
question:

```sh
veld presets              # what this project can start, and when to use each
veld nodes                # every node and variant, with the file and line it is defined in
veld status               # what is running right now
veld urls                 # the URLs of a live run
veld config --files       # which files define which nodes (why is my node missing?)
veld <subcommand> --help  # the flags, always current
```

Every one of those takes `--json`. `veld presets --json` in particular carries
each preset's `when_to_use` prose, which is the project author telling you which
one to pick.

**Do not read `veld.json` to find out what you can start.** It can be tens of
thousands of characters, it can be split across `include` globs, and half of
what it says is resolved at run time anyway. `veld presets` and `veld nodes` are
the resolved answer and cost a hundredth as much.

## The command surface

`veld --help` is authoritative and short. The shape worth knowing before you
read it:

| Doing | Commands |
|---|---|
| Run things | `start`, `stop`, `restart` |
| See what happened | `status`, `urls`, `logs`, `runs`, `stats` |
| Understand the project | `presets`, `nodes`, `graph`, `config`, `lint` |
| Act on a node | `actions`, `action` |
| Share | `share`, `join`, `shares`, `unshare` |
| Work with a human | `feedback` |
| Machine setup | `setup`, `doctor`, `update`, `desktop`, `settings` |

## Five things agents get wrong

**1. A preset is `--preset`, never a positional.** `veld start` positionals are
node selections in `node:variant` form. A preset name goes behind the flag:

```sh
veld start --preset dev-headless --name dev   # a preset
veld start api:local web:local --name dev     # explicit selections
veld start api --name dev                     # a node, using its default_variant
```

`veld start dev-headless` does not fall back to the preset — it fails as an
unknown node. (It will tell you so and point at `--preset`, but the round trip
is wasted.)

**2. `veld start` detaches.** It returns as soon as the environment is up; it
does not stream. Pass `--attach` (`-a`) from a TTY to stay in the foreground.
There is no `--detach`/`-d` — that is already what happens.

**3. Machine-readable output is on stdout; everything else is on stderr.**
`--json` payloads and a `--oneshot` node's own output go to stdout. Progress,
status lines, warnings and errors go to stderr. So `veld status --json | jq`
works, and a stray warning will never corrupt it.

**4. A veld config is JSONC.** `veld.json` (and `veld.jsonc` — both names are
legal, never hardcode one) may contain `//` comments and trailing commas. Strip
them before handing the file to a strict JSON parser, or better, use
`veld config --json`, which hands you the parsed result.

**5. Veld never rewrites a user's config.** `veld init` writes one when none
exists; nothing else edits one. If a change needs a config edit, make the edit
yourself and check it with `veld lint`.

## Environments, runs, names

An **environment** is the durable named slot — `--name dev` — and it is what
`start`, `stop` and `status` address. A **run** is one execution of it, with an
id, a start and end time, and an outcome that survives the run ending. When a
project has several people or worktrees on it, `--name` is what keeps them
apart, so pass it rather than relying on a default.

`veld skills runs` has the rest, including `--oneshot` for tests and CI.

## When something does not work

`veld doctor` first — it checks the daemon, ports, DNS and certificates and says
what is wrong. Then `veld logs --node <node>`, then `veld skills troubleshooting`.

---

`veld skills` lists every topic. This document describes the veld binary that printed it — run `veld -V` if you need the version.
