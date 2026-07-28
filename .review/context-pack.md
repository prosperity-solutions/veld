# Review context pack — veld config v3 (issue #173)

Every subagent gets this verbatim. Do not re-derive it.

## Target

- **Diff target:** `git diff $(git merge-base origin/main HEAD)...HEAD` on branch
  `config-system-v3-issue-173`. 53 files, +11159 / −1595, 14 commits.
  (The configured `auto` = "unpushed + uncommitted" resolves empty because the
  branch is already pushed; the branch-vs-merge-base diff is the change.)
- **Repo path:** `/Users/peterkuhmann/git/_worktrees/config-system-v3-issue-173`
- **PR:** #174. **Issue:** #173 — read it with
  `gh issue view 173 --repo prosperity-solutions/veld`. It is the specification;
  a deviation from it is a finding unless the diff documents why.

## Intent

Rework the `veld.json` authoring surface so a large monorepo config stays readable
and editable by several teams, without introducing inheritance, templates, or a new
config language. Guiding principle the change claims to uphold: **deduplicate
values, never structure** — which keys a node has stays written in that node, and
`rg <ENV_VAR_NAME>` must still find the line that sets it. Gated behind
`schemaVersion: "3"`; documents declaring `"1"` or `"2"` must keep loading with
today's semantics.

Implements the issue's 12 ordered steps: RC1 characterization tests, a
parse/validate loader split, `veld.*` namespace closure, JSONC, `argv`/`shell`
replacing `command`, node-level defaults, named ports, value sources with a
`secret` flag, `vars`, `include` file splitting, reserved `hooks`/`ui`, gap
primitives, and `--migrate` + schema + docs.

## In scope

- `crates/veld-core/src/`: `config.rs` (+2600), `include.rs` (new, 640),
  `migrate.rs` (new, 500), `values.rs` (new, 480), `jsonc.rs` (new, 380),
  `orchestrator.rs`, `process.rs`, `graph.rs`, `state.rs`, `port.rs`, `health.rs`,
  `lib.rs`
- `crates/veld/src/`: `main.rs`, `commands/{config,lint,nodes,action,runs,start,mod}.rs`
- `crates/veld-daemon/src/`: `monitor.rs`, `stats.rs`, `share/api.rs`,
  `desktop.rs`, `management.rs`
- `crates/veld-share/src/endpoint.rs`
- `schema/v3/veld.schema.json` + `schema/v3/examples/*.json`
- `tests/validate-schema.sh`, `.github/workflows/ci.yml`
- Docs: `README.md`, `AGENTS.md`, `docs/configuration.md`,
  `docs/migrating-to-v3.md` (new), `skills/veld/SKILL.md`,
  `skills/veld/reference/config.md`, `website/index.html`, `website/llms-full.txt`

## Out of scope

`Cargo.lock`, `THIRD-PARTY-LICENSES.md`, anything under `node_modules/`,
`target/`, `crates/veld-daemon/{frontend,ui}/` build output. Pre-existing bugs the
diff merely exposed → report as deferred, do not fix.

## Change shapes (§3.1)

`new-feature` (dominant) + `schema-migration` (config schema, the
`GraphSnapshot.command` serialized format, the v3 `command` gate) +
`config-infra` (CI, schema harness) + `docs-prompts` (7 doc surfaces) +
`mechanical-refactor` elements (every spawn site converted from `&str` to
`CommandSpec`).

**Stakes-elevated (§3.3).** Touched paths handle secrets (`values.rs` resolves
credential sources; `secret: true` gates what may reach a command line), file
modes for credentials, share **consent** (`variant_share` decides what may be
exposed), and a serialized-format migration. Angles 3 and 7 run at opus; no
cheap-tier substitution anywhere.

## Pre-pass results (verbatim — DO NOT RE-REPORT)

```
cargo fmt --all --check          → clean
cargo clippy --workspace --all-targets → No issues found (0 warnings, 0 errors)
cargo test --workspace           → 498 passed, 3 ignored (12 suites)
tests/validate-schema.sh         → 10 passed, 0 failed
```

**The typechecker, linter and tests already ran; their output is above. Do not
re-report it. Findings that duplicate tool output count against you.**

Baseline on `origin/main` was 411 tests, so the diff adds ~87.

## Dependency surface — verify against these, not from memory

| Library | Version | Installed source |
|---|---|---|
| `serde_json` | 1.0.149 | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde_json-1.0.149/` |
| `nix` | 0.29.0 (features `signal`, `process`, `net`, `fs`) | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/nix-0.29.0/` |
| `tokio` | 1.50.0 (features `full`; **`test-util` NOT enabled**) | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.50.0/` |
| `sysinfo` | 0.33.1 | `~/.cargo/registry/src/index.crates.io-*/sysinfo-0.33.1/` |
| `rusqlite` | 0.37.0 | `~/.cargo/registry/src/index.crates.io-*/rusqlite-0.37.0/` |
| `sha2` | 0.10.9 | `~/.cargo/registry/src/index.crates.io-*/sha2-0.10.9/` |

Correctness claims that depend on these: `serde_json` is silently last-wins on
duplicate keys and its `Error::line()`/`column()` are byte-offset-derived (the
whole JSONC design rests on this); `serde` forbids `deny_unknown_fields` with
`flatten`; `nix` for `getpgid`/`waitpid`/`raise`/`kill`; `tokio`'s signal registry
drops an event with no registered listener.

## Known deliberate deviations from the issue (already disclosed — not findings unless the reasoning is wrong)

1. **F9.7 not implemented** (`${nodes.x.field}` deriving the variant from
   `depends_on`). The issue marks it optional and says to skip if it complicates
   `graph.rs`.
2. **`start-server-needs-readiness` is version-gated** — error in v3, warning in
   v1/v2. The issue specifies "error"; applying that to v1/v2 would break the
   acceptance criterion that such configs run unchanged.
3. **`sensitive_outputs` NOT folded into `outputs` with `secret: true`** — would
   require `Outputs` values to become `ConfigValue`. `sensitive_outputs` still
   works; `--migrate` leaves it alone.
4. **`veld.local.json` NOT implemented** — F2's per-developer override layer,
   including the `local overrides: N values` status line. Substantive gap.

## Prior review findings already fixed (confirm or refute; do not re-report as new)

A five-angle review ran on the first four commits only. Fixed since:

- Duplicate-key rejection moved out of the loader into `validate` via
  `VeldConfig::deferred_findings`, because a load error is reachable from
  `veld stop` and F0.1 forbids that.
- `BUILTIN_VARS` + the `unknown-builtin-var` rule added, so the `veld.*` closure
  fails at `veld start`/`veld lint` rather than silently skipping an `on_stop`
  hook at teardown; `docs/configuration.md` corrected.
- `output_does_not_shadow_builtin` reprobed on `run` instead of `port` (on `main`,
  `set_builtin("port", …)` ran *after* the outputs loop, so `port` was never
  shadowable and the test passed against unmodified code).
- Duplicate-key errors report at the key, not the object's closing brace.
- `validate`'s sort made total; `#[must_use]` on `validate`.
- `port.rs` tests serialized against a real pre-existing flake.

**Still open from that review (in scope to re-find with better evidence, but known):**
`veld action` builds a fourth `veld.*` builtin set not routed through
`BuiltinScope`; `${veld.run_id}` is absent in setup/teardown though documented;
`detached_logs_reach_run_log` substitutes `tee` for the real `veld _log` so the
SQLite write and timestamping are untested; RC1 asserts `>= 2` process counts
rather than exact ones.

## Ledger

`.review/ledger.md`
