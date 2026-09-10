# Node actions

A node can declare **actions** — shell commands that the CLI and dashboard
expose generically. Veld injects the node's live outputs so the rotating clone
port and password never have to be copied by hand.

```jsonc
// in veld.json, under a node:
"database": {
  "variants": { "dblab": { /* … */ } },
  "actions": [
    {
      "name": "psql",
      "label": "psql",
      "description": "Open a psql shell to the DB clone",
      "requires_outputs": ["DB_HOST", "DB_PORT", "DB_NAME", "DB_USER", "DB_PASS"],
      "shell": "PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER $DB_NAME"
    }
  ]
}
```

Actions are **node-scoped**: a command sees only the outputs of the node it's
attached to. Inside `command` you can reference:

- `$KEY` — the node's live outputs, injected as environment variables and expanded by the shell at runtime
- `${output.KEY}` — the same outputs, interpolated by Veld into the command string before it runs
- `${param.KEY}` — the action's static `parameters`
- `${veld.run}`, `${veld.node}`, `${veld.project}`, `${veld.root}`, `${veld.port}`, `${veld.url}`
- The node's declared `env` (as `$KEY`, below the outputs in precedence) and the veld-owned `VELD_*` variables (`VELD_RUN`, `VELD_ROOT`, `VELD_NODE`, `VELD_VARIANT`, `VELD_PROJECT`, and the port/url/host family) — so an action can read the `CONTAINER_NAME` its node was started with

> **Secrets — `$KEY` is better than `${output.KEY}`, but it is not automatically
> safe.** `${output.DB_PASS}` is interpolated by Veld into the command string, so
> the value is in `ps` for certain — that is a `secret-in-command` **error**.
> `$DB_PASS` is expanded by the *shell* instead, and where the expansion ends up
> decides whether anything leaks:
>
> | Form | Leaks? |
> |---|---|
> | `echo $DB_PASS` (shell builtin, no `execve`) | no |
> | `PGPASSWORD=$DB_PASS psql -U u db` (environment assignment) | no |
> | `psql "postgres://u:$DB_PASS@host/db"` | **yes** — the shell `execve`s `psql` with the expanded value in *its* argv |
> | `open -a Postico "postgresql://$DB_USER:$DB_PASS@$DB_HOST/$DB_NAME"` | **yes**, same reason |
>
> The shell's own `ps` entry shows the literal `$DB_PASS`; the program it then
> runs shows the value. Veld cannot tell the cases apart, so `$KEY` naming a
> secret is a `secret-shell-expansion` **warning**, not an error. Prefer handing
> the program the variable *name* and letting it read the environment
> (`PGPASSWORD=`, `--password-file`, `-e NAME` for a container). For a GUI client,
> drop the password and let it prompt:
> `open -a Postico "postgresql://$DB_USER@$DB_HOST:$DB_PORT/$DB_NAME"`.

Run actions from the CLI:

```sh
veld actions                   # list configured actions
veld action psql               # run it against the only active run
veld action psql --name dev    # target a specific run
veld action psql --node database  # disambiguate when several nodes define it
veld action psql --print       # print the resolved command instead of running it
veld action psql --json        # resolved command as JSON (does not run)
```

`requires_outputs` gates availability: the action only runs (and only appears as
a dashboard button) when the node is running and exposes all listed outputs.

The management dashboard (`veld ui`) shows a button for each available action on
the node's row. Clicking it runs the action server-side via the CLI, so any
credentials never reach the browser.

---

`veld skills` lists every topic. This document describes the veld binary that printed it — run `veld -V` if you need the version.
