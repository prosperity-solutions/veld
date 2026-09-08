# Migrating a config to `schemaVersion: "3"`

**You have to.** This veld reads `schemaVersion: "3"` only — a `"1"` or `"2"`
config fails to load with a message pointing here.

Supporting two readings was tried and abandoned. Every rule needed a severity that
depended on the document's version, every new field was silently live in an old
document that had never opted into it, and the result was two config languages
sharing one parser — more confusing than the migration it was meant to avoid.

This page is the complete rule set. There are three mechanical rewrites and one
semantic change, and the whole thing is usually a handful of lines.

## The fastest way: hand this page to your coding agent

```
Migrate my veld.json to schemaVersion "3", following the rules in
https://github.com/prosperity-solutions/veld/blob/main/docs/migrating-to-v3.md
Then run `veld lint` and fix everything it reports.
```

`veld lint` is the check that makes this safe to delegate: it reads the migrated
config with the real parser and reports every remaining problem with a file, line,
and rule name. Verify the result, don't trust the rewrite.

### Why veld does not do this for you

veld shipped a `veld config --migrate` and removed it. Preserving your comments
ruled out a serde round-trip, so it rewrote bytes — and a byte-level rewriter
cannot see structure, so it could not honour the rule that `hooks` and `ide` are
opaque blobs veld promises not to interpret. It rewrote a `command` key inside
them anyway. Making it structural would have deleted the comments it existed to
protect.

The other half: choosing `argv` or `shell` for a given string is a judgment, and
the tool's tokenizer resolved every doubt by picking `shell` — permanently baking
in a `sh -c` that was not needed. Something that can read the command and reason
about it does better.

What veld keeps is the half a program is good at: **detection**. Loading a config
with any legacy form fails with every offending position named, exactly, with no
heuristics involved.

---

## The change that is easy to miss

Three of the four changes below are mechanical find-and-replace. This one is a
*semantic* change — the shape stays legal, the meaning moves:

**Node outputs are no longer reachable as `${veld.<OUTPUT>}` inside `on_stop`.**

```jsonc
// Before — worked, and the docs recommended it
"on_stop": "docker rm -f db-${veld.exit_code}"

// After
"on_stop": { "shell": "docker rm -f db-${output.exit_code}" }
```

`veld.*` used to absorb every node output on the teardown path — and only there.
So a node with an output named `run`, `branch`, or `port` shadowed the builtin
during teardown but nowhere else, and the same string resolved to two different
values depending on which path expanded it. `veld.*` is now a closed set.

Outputs are reachable as:

- `${output.KEY}` — this node's own outputs
- `${nodes.<node>.KEY}` — any node's

`veld lint` and `veld start` reject any `${veld.*}` name that is not a builtin,
**and any real builtin written where it is not populated** — `${veld.url}` on a
`command` node, `${veld.node}` in a `setup` step, `${veld.port}` in a `vars`
value. Both are caught **before a run starts** rather than at teardown — which
matters, because a teardown hook that fails to interpolate does not run, and
whatever it was going to clean up gets left behind. See
[Availability](configuration.md#availability) for the full matrix.

`on_stop` now receives everything its node had, `${veld.url}` and
`${veld.url.*}` and `${veld.ports.*}` included; it previously got `${veld.port}`
and stopped there.

---

## The rewrites

### `command` becomes `argv` or `shell`

v3 has one vocabulary for every place veld runs something. Two keys, exactly one
of them:

| Key | Meaning |
|---|---|
| `"argv": ["pnpm", "dev"]` | An array, spawned directly. No shell, no word splitting, no globbing. |
| `"shell": "pnpm dev \| tee out.log"` | A string, run via `sh -c`. You own the quoting. |

Use `argv` when the string is a program plus arguments. Keep it as `shell` when it
needs a shell:

```jsonc
"command": "node server.js --port ${veld.port}"
// → "argv": ["node", "server.js", "--port", "${veld.port}"]

"command": "tail -f app.log | grep ERROR"
// → "shell": "tail -f app.log | grep ERROR"   (a pipeline needs a shell)
```

**`shell` is required** if the string contains any of `| & ; < > ( ) * ? [ ] { } ~
#`, a backtick, a quote, a newline, a `VAR=value` prefix, or a bare `$VAR` — every
one of those is something the shell does, and `argv` has no shell. When in doubt,
`shell` is the answer that cannot change behaviour.

Note that `${veld.port}` is *veld's* interpolation, not the shell's, so it survives
into an `argv` element unchanged. Do not treat it as shell syntax.

**Why prefer `argv`?** Interpolation runs per element, *after* the array is
fixed, so a value containing spaces, globs, quotes, or newlines can never change
the argument count:

```jsonc
"argv": ["psql", "${vars.db_url}"]   // always exactly two arguments
"shell": "psql ${vars.db_url}"       // a URL with a space becomes two words
```

**`shell` is not deprecated.** It is permanently supported, and that is what
makes this a safe breaking change: any node that misbehaves under `argv` can be
reverted to a string by its author, with no veld change and no config version
change.

### Bare-string `on_stop` and `skip_if` become objects

```jsonc
"on_stop": "docker rm -f api"
// → "on_stop": { "argv": ["docker", "rm", "-f", "api"] }
```

Same rule everywhere: probes, `actions[]`, `setup`/`teardown` steps, and the
command form of a value source (`{ "command": "op read …" }` → `{ "shell": "op
read …" }`).

### `schemaVersion` becomes `"3"`

Do this **last**. It is the switch that makes every legacy form a load error, so
flipping it first only means the config stops loading before you have finished
fixing it. Once flipped, a remaining `command` fails to load with every offending
position named — which is a useful final check in its own right.

### What to leave alone

- **`hooks` and `ide`.** Opaque to veld. A `command` key *inside* them is not
  veld's key and must not be touched — the loader deliberately does not look
  there. (This is the rule the removed converter broke.) Seven keys under `ide`
  are now interpreted — `quicklinks`, `permissions`, `externalOrigins`, `panes`,
  `extensions`, `news` and the `git` subscope (`ide.git.stalenessSensitivity`), see
  [configuration.md](configuration.md#ide-the-projects-own-ide-surfaces) — but they are additions
  rather than a migration: a config that has none of them is unaffected, and the
  rest of `ide` stays opaque, including the `command` exemption. Note that
  `ide.panes[]` and `ide.extensions[]` entries *do* use `argv`/`shell` and are read
  by veld, so unlike the rest of `ide` they follow the v3 command rules.
  A config still spelling the key `ui` gets an unknown-top-level-key error that
  names the rename — that is the whole migration.
- **`sensitive_outputs`.** Still supported, unchanged, not deprecated.
- **Comments, formatting, key order.** Nothing about v3 requires changing them.
- **Anything already in v3 form.**

---

## What you can adopt afterwards, at your own pace

None of this is required. Each item exists to remove a specific workaround.

### Comments and trailing commas

Legal in every config file, no rename needed. One caveat: your **editor** does
not know that. A `veld.json` with a `$schema` is validated by the editor's strict
JSON parser, so either rename the root file to `veld.jsonc` — veld reads both, and
editors pick JSONC mode from the extension — or map the `.json` names to the
`jsonc` language:

```jsonc
// .vscode/settings.json
{ "files.associations": { "veld.json": "jsonc", "*.node.json": "jsonc" } }
```

Rename rather than copy: in a directory holding both, `veld.json` wins and
`veld lint` errors with `ambiguous-root-config`, so `veld start` refuses until you
delete one. (`veld stop` keeps working — that is a lint finding, not a load
failure, so teardown hooks still run.) `veld init` still writes `veld.json`.

### Splitting the config across files

```jsonc
// veld.json — the root file
{
  "schemaVersion": "3",
  "name": "example-monorepo",
  "include": ["veld.d/*.jsonc", "services/*/veld.node.json"]
}
```

```jsonc
// services/api/veld.node.json — CODEOWNERS: /services/api/ @some-team
{ "nodes": { "api": { /* … */ }, "worker": { /* … */ } } }
```

Every file uses the same schema. A node is defined in **exactly one** file — the
same name in two files is an error naming both, so there is no precedence rule to
learn. Relative paths (`cwd`, `script`, output paths) stay relative to the
**project root**, not to the file that declares them.

Only `nodes`, `presets`, `vars`, `env`, `setup`, and `teardown` merge across files:
each entry has an owning file, so there is nothing to arbitrate. The project-level
singletons — `url_template`, `default_preset`, `features`, `proxy`, `sharing`,
`client_log_levels` — are read from the root file only, because a single value
would need a precedence rule. Declaring one in an included file is a `root-only-key` error rather than a
silent no-op.

`veld config --files` prints the glob → file → node chain, which is the fastest
way to find out why a node seems missing.

### Node-level defaults

Declare a field once for every variant of a node. Any variant may override it.

```jsonc
"api": {
  "type": "long_running",
  "probes": { "readiness": { "type": "http", "path": "/healthz" } },
  "env": { "API_URL": "${vars.remote_api}" },
  "variants": {
    "dev":   { "argv": ["node", "server.js"] },
    "debug": { "argv": ["node", "--inspect", "server.js"], "env": { "API_URL": null } }
  }
}
```

This deduplicates **values, never structure**: which keys a node has is still
written in that node, and `rg API_URL` still finds the line that sets it. There
is no inheritance, no mixins, no templates.

The merge rules differ per field, on purpose — see the merge table in
[configuration.md](configuration.md#node-level-defaults).

### `vars`

One definition point per value:

```jsonc
"vars": { "remote_api": "https://api.example.com" }
// used as "${vars.remote_api}" wherever it is needed
```

A var holds a *value*, not a config fragment, and may not reference another var.
`veld config --why <pointer>` shows where an effective value came from.

A var literal may use the **run-scoped** built-ins — `${veld.run}`, `run_id`,
`name`, `project`, `root`, `worktree`, `branch`, `username`. The per-node ones
(`port`, `url`, `url.*`, `ports.*`, `node`, `variant`) are a lint error in a var,
because a var is one value for the whole run; compose those at the use site. A var
backed by a `file` / `env` / `argv` / `shell` source is resolved only when the
plan reaches it, so a credential helper behind a var is not run by a
`veld start docs`.

### Machine-overridable vars

Values that are facts about the **machine** rather than the project — a locally
installed container runtime, a memory ceiling, a path to a local tool — no longer
need a hand-set env var or a gitignored file the project parses itself:

```jsonc
"vars": {
  "container_runtime": {
    "machine": {
      "default": "docker",
      "choices": ["docker", "podman"],
      "description": "Which local runtime runs this project's containers"
    }
  }
}
```

The declaration is committed; each developer answers it once with
`veld config set container_runtime podman`. The answer is stored per machine and
**shared across every worktree of the repo**, so it is not asked again in each
checkout — `--worktree` narrows it when one checkout genuinely differs.

Nothing changes for an existing config: this is opt-in per var, and a plain var is
still a plain var. Adding a machine var with **no** `default` is the one thing to
introduce deliberately, because every machine (and every CI runner) then has to
answer it before the project starts.

### Named ports

```jsonc
"ports": { "http": "auto", "debug": "auto" }
// ${veld.ports.debug}, and VELD_PORT_DEBUG in the environment
```

`${veld.port}` still means the primary — the one named `http`, or the sole entry.
A node with no `ports` map behaves exactly as before. This exists so
debug-adapter variants and multi-port containers stop needing hand-picked literal
ports, which silently break parallel worktrees.

Named ports have since grown a `protocol`, and `ports` has since grown a third
authoring — see
[Added since v3 shipped](#added-since-v3-shipped-nothing-here-is-a-migration).

### Value sources and `secret`

```jsonc
"env": {
  "GITHUB_TOKEN": { "env": "GITHUB_TOKEN", "secret": true },
  "SIGNING_KEY":  { "file": ".secrets/signing.key", "secret": true },
  "DATABASE_URL": { "argv": ["secret-tool", "read", "path/to/secret"], "secret": true }
}
```

veld never takes custody of a secret: it carries a pointer and a flag, resolves
it at run start (only if the plan reaches it), and passes it to the process's
environment or a file.
`secret: true` is what lets veld *refuse* the unsafe uses — a secret veld
*substitutes* into an `argv` element or a `shell` string (`${vars.x}`,
`${output.x}`, `${nodes.a.x}`) is an error, because the value lands in the process
table for certain.

A bare `$SECRET_NAME` is a **warning** (`secret-shell-expansion`), not an error:
the *shell* expands it, so it leaks only when the expansion becomes another
program's argument. `PGPASSWORD=$DB_PASS psql …` is safe;
`psql "postgres://u:$DB_PASS@host/db"` is not, because the shell then `execve`s
`psql` with the password in its argv. Handing a container the name only,
`["docker", "run", "-e", "SECRET_NAME", "img"]`, is always safe.

A missing `env` source is an error at start naming the node and the variable,
rather than an empty value your app trips over later.

### `files:`

For a program that can only read a credential from disk:

```jsonc
"files": { ".secrets/client.pem": { "env": "CLIENT_CERT", "secret": true, "mode": "0400" } }
```

Created with its mode (default `0600`), so it is never briefly world-readable.
veld does **not** delete the file when the run ends — a program may re-read it
across restarts, and removing a path you declared is a worse default than leaving
it. Git-ignore the path; veld warns at start if a `secret` file lands somewhere git
would commit.

### Preset composition

```jsonc
"presets": { "core": ["api:dev", "web:dev"], "ci": ["@core", "e2e:dev"] }
```

### Presets gained keys and metadata

**Read this even if you change nothing: the numbers in the `veld start` picker
move once, on upgrade.**

They used to be positions in an *alphabetically sorted* list. They are now keys,
assigned in the order presets are declared in the file. So for a config whose
presets are not written in alphabetical order, the number people type now means a
different preset:

```jsonc
"presets": { "zulu": [...], "alpha": [...], "docker": [...] }
```

| | `1` | `2` | `3` |
|---|---|---|---|
| before | `alpha` | `docker` | `zulu` |
| after | `zulu` | `alpha` | `docker` |

That is a one-time change, and it is the point: a position renumbers every time
someone adds a preset, which is the bug being fixed. But it does mean a runbook
or a habit that says "run `veld start`, press 2" needs re-checking once. After
upgrading:

```sh
veld presets              # the numbers as they now stand
veld presets --pin        # a block to paste that freezes them for good
```

Pin the numbers people actually type and they will not move again.

The rest of this section is optional. A preset may now be an object as well as an
array; **the array form stays valid and is not deprecated**, so no existing
`presets` block needs to change:

```jsonc
"presets": {
  "core": ["api:dev", "web:dev"],            // unchanged, still correct
  "designer-preview": {
    "key": 1,                                 // stable picker number
    "label": "Site preview (staging content)",
    "when_to_use": "Reviewing visuals against real CMS content. Slow to start.",
    "group": "For non-developers",
    "selections": ["web:prod", "api:staging"]
  }
},
"default_preset": "core"
```

Worth adopting if the numbering at `veld start` has ever shifted under someone,
or if a preset list has grown past what a newcomer can identify:

- `key` pins the number typed at the picker so it stops moving. Unpinned presets
  take the lowest unclaimed number in declaration order, so appending a preset —
  or pinning one at the number it already shows — changes nothing for anyone else.
  `veld presets --pin` prints the current numbering as a block to paste.
- `label` / `when_to_use` / `group` make the list pickable by people who did not
  write the config, and by coding agents reading `veld presets`.
- `default_preset` gives a bare `veld start` a defined answer, including in a
  non-interactive shell where it previously failed with "No selections provided".

`veld lint` reports a duplicate or zero `key`, a `default_preset` naming nothing,
and — once, at eight or more presets with no metadata at all — a notice suggesting
the object form.

---

## Added since v3 shipped (nothing here is a migration)

**You do not have to change anything.** Since v3 shipped, veld gained
`long_running` (a permanent alias of `start_server`), long-running nodes with no
ports at all, `protocol` and `host` on a named port, the `settle` readiness probe,
and per-port sharing consent. All of it is additive *within* `schemaVersion: "3"`:
there is no v4, and no config that loads today stops loading tomorrow.

### Telling your team something changed

`ide.news[]` is new: a short card your project shows its own team in the Veld IDE,
merged with the change it describes and shown once to each teammate who pulls it.
Five fields, all capped (a headline and one sentence), at most five live items, and
`veld lint` reports anything malformed rather than failing the load. The date is
required and gates the card, so a teammate who joined after it never sees it.
Purely
additive — a config without it is unaffected — and see
[configuration.md](configuration.md#idenews-telling-your-own-team-something-changed)
for the fields and [promotions.md](promotions.md) for how the channel behaves.

### How long a share link lives

`sharing` gained `peer_ttl_minutes` and `web_ttl_minutes` (5–480, clamped), the
project's answer to how long its share links last. Additive: a config without them
falls through to each machine's `sharing.peerTtlMinutes` / `sharing.webTtlMinutes`
setting, which carries the defaults — 240 peer, 120 web. Those are **longer than
the constants they replace** (120 / 60), so a default share link now lives twice
as long; the keep-awake caps are unchanged, and being the shorter pair they are
now what ends the hold. `veld share --ttl` still overrides both
for one share. See
[configuration.md](configuration.md#sharingpeer_ttl_minutes--sharingweb_ttl_minutes),
which also explains why this is the deadline that usually ends a share and how it
relates to the keep-awake ceiling above it.

### Terminal ergonomics

`ide.panes[]` gained `allow_terminal_renaming` (default `false`), which lets the
process inside a config-declared pane rename its own tab with the terminal title
it sets (OSC 0/2). A plain terminal — a login shell, not a pane — always adopts
its title; this is the opt-in for a pane whose own title is more useful than its
fixed `label`. Veld's terminals also ring the bell (BEL), and the OSC 9 "notify"
sequence raises a notification *and* rings the bell with it, naming the worktree
and the pane's own name with a click that focuses the pane — terminal
ergonomics, not a persisted inbox. All additive.

`ide.extensions[]` is new and additive: badges, buttons and menus a project
contributes to the IDE's top bar, each backed by an `argv`/`shell` command veld
runs in that worktree. Nothing existing changes — a config without the key behaves
exactly as before — and the entries follow the v3 command rules like `ide.panes[]`
does. See
[configuration.md](configuration.md#ideextensions-the-projects-own-badges-buttons-and-menus).
A `status` badge also gained `display` (`text`, the default, or `icon`, which
renders the glyph alone with the label kept as the accessible name) — additive
the same way, and overridable per value the same way `open_in` is.

It has its own page, because it is a menu rather than a migration:
**[docs/adopting-long-running-and-ports.md](adopting-long-running-and-ports.md)**.

Read at least its [behaviour
changes](adopting-long-running-and-ports.md#behaviour-changes-worth-a-veld-lint)
section — the only part that can move under a config you never
touch. `veld lint` reports every one of them.

---

## Rollback

`git checkout` the config. Do the migration on a branch and nothing here is
irreversible. Note that rolling the *config* back means rolling *veld* back too,
since this version reads only `schemaVersion: "3"`.

One thing worth knowing before you upgrade veld itself, independent of migrating a
config: an unrecognised top-level key — most often the pre-JSONC `"//": "…"`
comment idiom — used to be ignored silently and is now reported by `veld lint` and
refused by `veld start`. It is deliberately **not** a load error, so `veld stop`
keeps working against an already-running environment. Run `veld lint` once after
upgrading; if it is quiet, nothing here affects you.
