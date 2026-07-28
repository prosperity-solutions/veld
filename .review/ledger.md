# Review ledger — veld config v3 (#173) / PR #174

Config: SPAWNS 14 (max 6 opus) · ROUNDS 5 (stakes-elevated) · AUTONOMY full
Pre-pass: green (fmt clean, clippy 0, 500 tests, schema harness 10/10).

## Routing (revised after RECLASSIFY, §3.4)

Original shapes: new-feature + schema-migration + config-infra + docs-prompts.
**Corrected:** the diff also carries an undisclosed `mechanical-refactor`
dimension with a real behavior change to v1/v2 documents. Angle 6 (Invariance)
promoted from Stage C into Stage A at opus — for `mechanical-refactor` the table
routes it **O**, and the RECLASSIFY proved v1/v2 invariance is where the defects
actually are.

Opus budget note: 6 opus max. Spent on angles 1 (stopped early via RECLASSIFY),
4, 7, then 1-rerun, 6, 3. Angles 2 and 5 run at sonnet as a consequence — the two
angles §3.3 names for the stakes override (3 and 7) both got opus.

| Stage | Angles | Model |
|---|---|---|
| A structural | 1 Counterfactual, 4 Missing, 7 Threat, +6 Invariance | O,O,O,O |
| B behavioral | 3 Assumptions, 2 Persona | O, S |
| C local | 5 Self-consistency | S |

---

## Round 1 — Stage A — spawns: 3O / cumulative: 3 of 14

| id | path:line | sev | angle(s) | status | note |
|----|-----------|-----|----------|--------|------|
| F1 | crates/veld-core/src/include.rs:39 | 🔴 | 1 (RECLASSIFY) | verified-fixed | `deny_unknown_fields` on `Document` made an unknown top-level key a **load** error — for v1/v2 too, which previously ignored it silently. Reachable from `veld stop`, which F0.1 forbids: a project with a stray `"//"` comment key upgrades into a config that cannot be torn down. Verified empirically against the built binary, and old `VeldConfig` confirmed to have no `deny_unknown_fields`. Fixed by capturing unknowns via `#[serde(flatten)]` into deferred findings — same mechanism as duplicate keys. Docs in 4 files claimed the opposite ("remains a hard error", "v1 and v2 loading is unchanged") and are corrected. |

## Round 1 — Stage A — angle 7 (threat, opus) — verified by orchestrator

| id | path:line | sev | angle | status | note |
|----|-----------|-----|-------|--------|------|
| T1 | config.rs:2718 `secret_env_keys` | 🔴 | 7 | open (VERIFIED) | `secret-in-command` builds its secret set from `resolve_env` only. A `vars` entry marked `secret: true`, referenced as `${vars.pw}` in `argv`, passes lint AND expands at spawn → credential in the process table. `vars.*` is an issue-sanctioned value position, so `secret: true` is a false promise there. Repro: lint says "is valid". |
| T2 | schema/v3/veld.schema.json:286 | 🔴 | 7 | open (VERIFIED) | Schema types `proxy.{request,response}.set.*` as `$defs/value` (object + `secret`), but `HeaderRules.set` is `BTreeMap<String,String>`. A **schema-valid** config fails to LOAD ("invalid type: map, expected a string") → strands `veld stop`, breaking F0.1 via a config an editor blessed. My drift gate missed it: no example exercises that position. |
| T9 | include.rs:267 / config.rs reject_v3_legacy_commands | 🟠 | 7 | open (VERIFIED) | The v3 `command` gate keys off each **file's own** `schemaVersion`, but included files legitimately omit it (root-only). So in a v3 project every included file may still use `command` and bare-string `on_stop`, and it loads and runs — unenforced exactly where F2 puts the node bodies. `--migrate` also only rewrites the root file. Repro: lint "is valid". |
| T10 | config.rs:2232 | 🟠 | 7 | open (VERIFIED-partial) | Node-level `share` is hoisted with no `schemaVersion` gate, and `NodeConfig` is not `deny_unknown_fields`. A v1/v2 config with a stray node-level `share` previously did nothing; now it grants consent to **every** variant, including ones never opted in. Fail-open change to the consent gate on an unchanged config. |
| T3 | share/api.rs:591 | 🟠 | 7 | open | Resolved `proxy.*.set` values travel verbatim in the share manifest to joiners and the gateway; nothing scrubs them and no lint rule scans them. Issue explicitly requires scrubbing + refusing a credential-shaped literal there. Not in the disclosed-deviation list. |
| T4 | config.rs:2793 | 🟠 | 7 | open | Rule compares `${output.*}` refs against secret **env** keys only; `sensitive_outputs` and `${nodes.x.KEY}` never checked, though `on_stop` has both in scope. |
| T5 | config.rs:2781 | 🟠 | 7 | open | Scanned positions are only `[command, on_stop, skip_if]` — `actions[]` and both probe specs are unscanned, and actions are where `${output.*}` is actually in scope. |
| T6 | orchestrator.rs:2786 | 🟠 | 7 | open | F9.3 synthetic outputs interpolate `${output.*}` after `sensitive_keys` is set and are never marked sensitive → launders a `sensitive_outputs` value into an unmasked, persisted output. |
| T7 | values.rs:238 | 🟠 | 7 | open | `OpenOptions::mode()` applies only on creation; no chmod after. Over an existing 0644 file the declared 0600 is silently ignored — contradicting the doc comment and docs. |
| T8 | values.rs:205 | 🟠 | 7 | open | Delivered files are never removed and the returned paths are discarded; a resolved credential is left in the working tree after the run, with no gitignore guidance. |
| T11 | values.rs:224 | 🟡 | 7 | open | `files` path neither normalized nor confined; absolute paths and `..` accepted, symlinks followed and truncated. Not an escalation (config write already = code exec) but breaks documented containment. |
| T12 | config.rs:2393 | 🟡 | 7 | open | v3 gate flags any key named `command` **inside** the opaque `hooks`/`ui` blobs → unloadable config; `--migrate` rewrites reserved content it must not interpret. |
| T13 | values.rs:58 | 🟡 | 7 | open | `CommandFailed` puts a source command's stderr into `end_detail.message`, persisted and reprinted by `veld runs show`. |
| T14 | endpoint.rs:617 | 🟡 | 7 | open | `run_token_command` lacks `.stdin(Stdio::null())` unlike the new `run_source_command`; can read/steal the tty instead of failing at its timeout. |
| T15 | config.rs:521 | 🟡 | 7 | open | `parsed_mode` accepts setuid/setgid and world-readable modes on a `secret: true` delivery with no cross-check. |
| T16 | values.rs:91 | 🟡 | 7 | open | `files` literal content is not interpolated while `env` literal content is — `"PORT=${veld.port}"` writes the placeholder text silently. |

## Round 1 complete — Stage A only. HALTED per §2(b).

Spawns: 5 opus / 0 sonnet / 0 haiku — 5 of 14. Opus 5 of 6.
Stage A ran 4 angles (1 twice, after a RECLASSIFY): 1, 4, 6, 7.
Stages B (2, 3) and C (5) NOT RUN.

Fixed & verified-fixed this round: F1, M-A, M-B, T1, T2, T4, T5, T7, T9, T11,
T12, C2, C3, I4, I5, M8, M9, M14, M15.

Deferred (real, out of this diff's minimal-fix scope): T3, T6, T8, T13, T14,
T15, T16, C1, C4, C5, C9, C10, C11, C12, C13, C14, C15, C16, I6, I7, I8, I9,
I10, I11, M-remaining.

HALT REASON — §2(b), one interlocking product decision blocks a coherent fix
for I1 + I2 + I3: **how strictly does v3 validation apply to a v1/v2 document?**
Every option changes what an existing user sees on upgrade, so it is not mine
to guess.
