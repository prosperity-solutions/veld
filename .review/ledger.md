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
