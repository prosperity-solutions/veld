# Migrating a config to `schemaVersion: "3"`

**You do not have to.** `schemaVersion: "1"` and `"2"` keep loading with today's
semantics, indefinitely. There is no flag day. Migrate a project when you want
what v3 adds, not because a deadline is coming.

Start with the tool:

```sh
veld config --migrate          # dry run: shows a diff, writes nothing
veld config --migrate --write  # apply it
veld lint                      # check the result before starting anything
```

The dry run is the default on purpose. Turning a shell string into an argv is a
heuristic — `sh -c "a | b"` and `["a", "|", "b"]` are different programs — so
anything with shell syntax in it is deliberately left alone and listed for you to
look at.

---

## The one breaking change that affects v1 and v2 too

Everything else in v3 is opt-in. This is not.

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
so this is caught **before a run starts** rather than at teardown — which
matters, because a teardown hook that fails to interpolate does not run, and
whatever it was going to clean up gets left behind.

Run `veld lint` on your existing config now, before migrating anything. If it is
quiet, this does not affect you.

---

## What `--migrate` changes

### `command` becomes `argv` or `shell`

v3 has one vocabulary for every place veld runs something. Two keys, exactly one
of them:

| Key | Meaning |
|---|---|
| `"argv": ["pnpm", "dev"]` | An array, spawned directly. No shell, no word splitting, no globbing. |
| `"shell": "pnpm dev \| tee out.log"` | A string, run via `sh -c`. You own the quoting. |

`--migrate` converts to `argv` when the string is safely tokenizable and leaves
it as `shell` otherwise:

```jsonc
"command": "node server.js --port ${veld.port}"
// → "argv": ["node", "server.js", "--port", "${veld.port}"]

"command": "tail -f app.log | grep ERROR"
// → "shell": "tail -f app.log | grep ERROR"   (a pipeline needs a shell)
```

A string stays `shell` if it contains any of `| & ; < > ( ) \` " ' * ? [ ] { } ~
# \` , a newline, a `VAR=value` prefix, or a bare `$VAR`. Note that
`${veld.port}` is *veld's* interpolation, not the shell's, so it survives into an
argv element.

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

Same rule everywhere: probes, `actions[]`, `setup`/`teardown` steps.

### `schemaVersion` becomes `"3"`

Which is also the switch that makes `command` a load error. A v3 document
containing `command` fails to load, and the message names every offending
position and points back at `veld config --migrate`.

### What `--migrate` does *not* touch

- **Comments and formatting.** The rewrite is textual and surgical rather than a
  serde round-trip, precisely so your comments survive.
- **`sensitive_outputs`.** Still supported, unchanged, not deprecated.
- **Anything already in v3 form.** Re-running is a no-op.

---

## What you can adopt afterwards, at your own pace

None of this is required. Each item exists to remove a specific workaround.

### Comments and trailing commas

Legal in every config file, no rename needed. One caveat: your **editor** does
not know that. A `veld.json` with a `$schema` is validated by the editor's strict
JSON parser, so map it to the `jsonc` language to stop spurious errors:

```jsonc
// .vscode/settings.json
{ "files.associations": { "veld.json": "jsonc", "*.node.json": "jsonc" } }
```

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

`veld config --files` prints the glob → file → node chain, which is the fastest
way to find out why a node seems missing.

### Node-level defaults

Declare a field once for every variant of a node. Any variant may override it.

```jsonc
"api": {
  "type": "start_server",
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

### Named ports

```jsonc
"ports": { "http": "auto", "debug": "auto" }
// ${veld.ports.debug}, and VELD_PORT_DEBUG in the environment
```

`${veld.port}` still means the primary — the one named `http`, or the sole entry.
A node with no `ports` map behaves exactly as before. This exists so
debug-adapter variants and multi-port containers stop needing hand-picked literal
ports, which silently break parallel worktrees.

### Value sources and `secret`

```jsonc
"env": {
  "GITHUB_TOKEN": { "env": "GITHUB_TOKEN", "secret": true },
  "SIGNING_KEY":  { "file": ".secrets/signing.key", "secret": true },
  "DATABASE_URL": { "argv": ["secret-tool", "read", "path/to/secret"], "secret": true }
}
```

veld never takes custody of a secret: it carries a pointer and a flag, resolves
it at run start, and passes it to the process's environment or a file.
`secret: true` is what lets veld *refuse* the unsafe uses — a secret in an `argv`
element or a `shell` string is an error, because both appear in the process table.

A missing `env` source is an error at start naming the node and the variable,
rather than an empty value your app trips over later.

### `files:`

For a program that can only read a credential from disk:

```jsonc
"files": { ".secrets/client.pem": { "env": "CLIENT_CERT", "secret": true, "mode": "0400" } }
```

Created with its mode (default `0600`), so it is never briefly world-readable.

### Preset composition

```jsonc
"presets": { "core": ["api:dev", "web:dev"], "ci": ["@core", "e2e:dev"] }
```

---

## Rollback

Set `schemaVersion` back to `"2"` and revert the `argv`/`shell` conversions —
or just `git checkout` the file, which is why `--migrate` is a dry run by
default. v1 and v2 loading is unchanged, so there is nothing else to undo.

One thing worth knowing before you upgrade veld itself, independent of migrating a
config: an unrecognised top-level key — most often the pre-JSONC `"//": "…"`
comment idiom — used to be ignored silently and is now reported by `veld lint` and
refused by `veld start`. It is deliberately **not** a load error, so `veld stop`
keeps working against an already-running environment. Run `veld lint` once after
upgrading; if it is quiet, nothing here affects you.
