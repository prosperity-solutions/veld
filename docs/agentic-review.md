# Autonomous multi-angle review loop

The review methodology for every veld change (see `AGENTS.md` → PR Workflow →
Review rounds). You are the orchestrator: review the diff, fix what you find,
re-review your own fixes, and keep going until the exit criteria in §9 are met.
Do **not** check in between rounds — the maintainer is not watching. Report once,
at the end, unless you hit one of the three hard stops in §2.

```
DIFF:      auto     # auto = committed-but-unpushed + uncommitted, vs. merge-base with default branch
INTENT:    auto     # auto = derive from branch name, commit messages, PR body
SPAWNS:    14       # max subagent spawns across all rounds; of which max 6 opus
ROUNDS:    auto     # 2 low-risk / 3 default / 5 stakes-elevated (§3.3)
AUTONOMY:  full
```

Resolve `auto` values yourself in §1. If `DIFF` resolves to nothing, say so and
stop — don't invent a review target.

**You cannot observe your own token consumption.** Do not estimate it, report it,
or ration against it. Never shorten a read, skip a verification, or exit a round
because you feel expensive — a cheap unverified finding is worth less than no
finding. Cost is controlled by the caps above and the routing in §3, which are
countable. Count spawns and rounds; ignore tokens.

---

## 0. Design divergence (before the code exists)

Everything below this section reviews a diff. This one runs *before* there is one,
and only for a **load-bearing fork** — a decision that is expensive to reverse
because it lands in a schema, a wire format, a persisted value, or a surface users
will build habits on. Skip it for anything you would happily rewrite next week.

The failure it addresses is not sloppiness, it is **mode collapse**: asked for a
design, a model returns the single most typical one, and typical is exactly what
gets chosen anyway if nobody argues. Adapted from *Verbalized Sampling*
(CHATS-lab); what follows is the part that earned its place here, not the paper.

### 0.1 The sparring brief

One subagent per fork, `run_in_background: false` (the result is the deliverable —
a background agent that fails to report costs you the whole round), opus, and a
brief with these five clauses:

1. **Name the modal answer in one sentence, then forbid it.** This is the load-
   bearing clause. Writing down "what every assistant would say" and being barred
   from submitting it is what forces the rest of the output somewhere new.
2. **k candidates, each with a `<probability>` strictly below τ.** Use `k=3` for a
   narrow fork and `k=5` at most. τ = 0.10.
3. **Candidates must differ in *mechanism*, not in constants or wording.**
4. **Each candidate ends with the strongest argument against it**, stated flatly
   by the agent, not hedged.
5. **A closing judgment: rank them, pick one, and name any trap** — a candidate
   that looks good and fails on contact.

Give it verified facts, tell it not to read the repo, and tell it not to write
code. Its value is divergence; ours is deciding.

### 0.2 Read the probabilities as a style knob, not a distribution

The numbers carry no information — 0.04 versus 0.075 will not order your options,
and you should not pretend otherwise. What works is the *instruction* to sit in the
tail. Do not report the probabilities as if they meant something.

### 0.3 Verify the winner before building it — the round's own §8.3

**This stage raises recall and lowers precision.** Tail-sampling rewards unusual
arguments, and an unusual argument that *sounds* decisive is the exact failure mode.
Treat a sparring round's top pick as a claim, not a conclusion: check its load-
bearing premise against the code or the docs before committing to it.

This is not hypothetical. In the round that designed the settings store, the
top-ranked candidate argued against a migration because each one is a downgrade
cliff (`DbError::NewerSchema`). True, and irrelevant: the previous release had
already shipped a migration, so the cliff was pre-paid and the marginal cost was
zero. One check overturned the ranking.

### 0.4 What to keep

Fold the outcome into the PR body or a `docs/` note — **including the rejected
candidates and why**. A design's discarded alternatives are the most expensive
thing in the round to regenerate, and the first question a reviewer asks is "why
not the obvious thing".

Two signals worth trusting:

- **Independent agreement across two rounds on the same question.** Run the fork
  twice, blind, when it is worth it. Both rounds on the worktree marker
  independently flagged a pre-existing defect nobody had asked about
  (`pick_emoji` probing globally across every repo) — that is the strongest output
  the method produced, and neither round was prompted toward it.
- **A named trap.** Both marker rounds independently called deriving the marker
  from a mutable input (branch, path) seductive and fatal. That warning arrived
  before the code, which is the whole point of running this before §1.

### 0.5 When not to run it

- The decision is cheap to reverse → just build one and iterate.
- You already know the answer and want agreement → you are shopping for a
  rubber stamp, and this will give you five.
- The question is a naming or formatting choice → the machinery costs more than
  the decision.
- The fork is genuinely constrained to two options with a decisive argument
  already on the table → state the argument and move on.

---

## 1. Boot

Resolve config, then build the **context pack**. Build it once; every subagent
gets the same one. This is the single largest cost lever — subagents that
re-derive repo context five times in parallel are how this gets expensive.

```
- Diff target:      <resolved command / ref>
- Repo path:        <absolute>
- Intent:           <1-3 sentences: what this change is trying to do>
- In scope:         <paths>
- Out of scope:     <vendored, generated, lockfiles, snapshots>
- Change shape(s):  <§3.1>
- File inventory:   <path, ±lines, shape, stakes flag>
- Dependency surface: <lib@exact-version + installed source path> for every
                    library whose behavior the change's correctness depends on
                    (error strings, return shapes, enum values, version quirks)
- Pre-pass results: <§1.1, verbatim>
- Ledger path:      notes/review/ledger.md
```

Pin dependency source paths literally — in this repo that means the installed
crate source under `~/.cargo/registry/src/index.crates.io-*/<crate>-<version>/`
(or the `[patch]`/path dependency it actually resolves to), and
`crates/veld-daemon/ui/node_modules/...` for the UI. "Verify against the real
thing" only works if they know where the real thing is.

The ledger lives under gitignored `notes/` (AGENTS.md → working documents are
never tracked). This loop creates local commits; an untracked `.review/` would
get swept into one.

### 1.1 Pre-pass — free signal, zero agents

Run before spawning anything. In this repo:

```
rustup update stable                      # CI uses floating stable — drift blocks it
cargo clippy --workspace --all-targets
cargo fmt --all --check
cargo test --workspace                    # or the subset covering touched crates
git diff --stat <diff-target>
git log --oneline -- <touched paths>
```

Plus, when the diff touches them: `npm run typecheck` / `npm run lint` / `npm test`
in the affected JS surface (`desktop/`, `crates/veld-daemon/ui`, `crates/veld-daemon/frontend`),
and any schema or license drift gate the change is near. `just lint` / `just test`
run the whole static + test pass for both Rust and JS.
Capture output verbatim.

- **Pre-pass red → fix that first**, then start. Reviewing a diff that doesn't
  compile or whose tests fail is a category error.
- **The pre-pass is not a warm-up for CI — it is the only signal this diff gets.**
  CI skips every job while a PR is a draft (AGENTS.md → CI cost convention), and
  this loop runs before the PR exists at all, or at most on a draft. So there is
  no second opinion coming: a check you skip here is a check nobody runs. Run the
  full list, including the UI checks when the diff touches
  `crates/veld-daemon/ui`, and re-run it after every fix (§8.4) rather than
  batching one run at the end.
- **Two CI checks are now post-spend, so run them locally instead.**
  `just workflow-gates` whenever the change touches `.github/workflows/`, and
  `just commit-subjects` before pushing. Both used to fail on the first draft
  push, in seconds, on a Linux runner; both are draft-guarded now and first report
  *after* the PR is marked ready — so a malformed commit subject or an unguarded
  new job is discovered only once the five macOS legs have already dispatched.
  `just workflow-gates` is the sole thing standing between an unguarded job and a
  draft that quietly runs it, because the gate's CI home (the `schema` job) is
  draft-guarded too. It now carries a second gate in the same recipe:
  `release.yml`'s publish script, which a subagent reading a diff cannot
  exercise at all — it is run against a stub `gh` and held to the rule that a
  failure must fall toward *not* publishing.
- **Everything the pre-pass reports is out of scope for every subagent.** Put
  this line in every brief: *"The typechecker, linter and tests already ran;
  their output is in the context pack. Do not re-report it. Findings that
  duplicate tool output count against you."*

### 1.2 Ledger

Create `notes/review/ledger.md` and update it after every round. You will lose
context to compaction during a long run; the ledger is your memory.

```
## Round N — <timestamp> — spawns: <this round: 3O 2S 0H / cumulative: n of 14>
| id | path:line | sev | angle(s) | status | note |
|----|-----------|-----|----------|--------|------|
| F1 | src/a.rs:44 | 🔴 | 1,4 | fixed@<sha-or-desc> | |
| F2 | src/b.rs:12 | 🟠 | 3 | deferred | out of diff scope → ticket |
| F3 | src/c.rs:90 | 🟠 | 5 | DECISION-REQUIRED | see §2b |
```

Statuses: `open` / `fixed` / `verified-fixed` / `deferred` / `dropped-unverified`
/ `DECISION-REQUIRED` / `RESURFACED`.

---

## 2. Autonomy contract

**Do without asking:** spawn/kill subagents, read anything, run tests and
linters, apply fixes, re-route angles, add rounds within budget, write to
`notes/review/`, create local commits on the current branch.

**Never do:** push, force-push, amend or rebase others' commits, touch the
default branch, modify CI credentials or secrets, delete tests to make them pass,
widen scope into pre-existing bugs the diff merely exposed, reformat or "improve"
code outside a finding's fix, change intended product behavior.

**Never mark the PR ready for review** (`gh pr ready`). That is what starts CI,
and it belongs to the caller *after* this loop exits — the whole point of the
draft state is that an unreviewed diff costs nothing (AGENTS.md → CI cost
convention). A loop that flips the PR ready mid-round pays for a full CI run on
code it is about to change.

**Three hard stops — halt the loop, write the report, ask:**

- **(a) Redesign.** A 🔴 that can't be fixed without changing the approach or
  architecture. Do not autonomously rewrite someone's design. State the finding,
  the two or three viable directions, and your recommendation.
- **(b) Decision required.** A fix requires choosing product behavior (which
  error the user sees, what the default becomes, whether to break a consumer).
  Log `DECISION-REQUIRED`, **continue the loop on everything else**, and surface
  it in the final report. Only halt entirely if it blocks the remaining work.
- **(c) Deadlock.** Same finding resurfaces after a fix twice, or fixes are
  ping-ponging (round N's fix reintroduces round N-1's finding). That's a
  disagreement about what "correct" means, not a loop to grind. Halt, state both
  positions.

Hitting the spawn or round cap with blockers still open is also a halt — report
honestly, name what went unreviewed, and ask for a raised cap. Never quietly
finish a thinner review and present it as a complete one.

---

## 3. Triage & routing

### 3.1 Classify the change shape

Label the diff — multiple labels allowed; route the union, scoping each angle to
the files that earned it. For diffs over ~30 files, delegate classification to a
**haiku** agent that returns a file inventory + per-cluster label, then spot-check
its labels on 2-3 files before trusting them.

`mechanical-refactor` (rename/codemod/extraction, behavior claimed unchanged) ·
`new-feature` · `bugfix` · `config-infra` · `docs-prompts` · `dep-bump` ·
`schema-migration` (DB, serialized formats, API contracts, event payloads) ·
`deletion`

In this repo `schema-migration` covers SQLite migrations (`user_version` steps),
`schema/v2/veld.schema.json`, `veld.json` wire compatibility, daemon HTTP/WS API
shapes, and anything a shipped binary must still parse after an update.

If the diff is three unrelated changes in a trenchcoat, review them as separate
scoped passes rather than one composite.

### 3.2 Routing table

`O` = opus, `S` = sonnet, `H` = haiku, `—` = don't run. Set `model:` explicitly
on every spawn; never inherit (inheritance can silently fall back and defeats the
tiering).

| Shape | 1 Counterfactual | 2 Persona | 3 Assumptions | 4 Missing | 5 Self-consist. | 6 Invariance | 7 Threat |
|---|---|---|---|---|---|---|---|
| `mechanical-refactor` | — | — | S | S | S | **O→H sweep** | — |
| `new-feature` | **O** | S | **O** | **O** | S | — | if stakes |
| `bugfix` | S | — | **O** | S | S | — | if stakes |
| `config-infra` | S | S | **O** | **O** | S | — | if stakes |
| `docs-prompts` | S | **O** | S | **O** | S | — | — |
| `dep-bump` | — | — | **O** | S | H | S | if stakes |
| `schema-migration` | **O** | S | **O** | **O** | S | **O** | **O** |
| `deletion` | S | — | S | **O** | S | **O** | — |

Tier by task shape, not diff size: open-ended hypothesis generation (angles 1, 3)
is what you're paying opus for; closed-form checking against an explicit spec
(angles 5, 6-sweep) is sonnet/haiku work.

### 3.3 Stakes override — routing cannot drop these

If any touched path matches auth, authn/authz, session, crypto, payment, billing,
PII, migration, or anything the repo marks security-sensitive:

- Angles 3 and 7 run at **opus** regardless of shape or size.
- Round cap → 5. No cheap-tier substitution **anywhere** in the diff.

Three lines in middleware outrank three hundred in a fixture.

Veld's stakes-elevated surfaces: the privileged helper and anything it executes,
Caddy config emission, relay auth tokens / `SecretSource` (including
`command`-sourced secrets and daemon `PATH`), gateway password + share links and
public web sharing, proxy header rules, the daemon's HTTP/WS API and PTY
endpoints, and any SQLite migration.

### 3.4 Reclassification

End every brief with:

> If the diff contradicts its stated classification — you were told mechanical
> refactor and you find a behavior change, a changed default, a dropped branch, a
> new external call — **stop your angle and return
> `RECLASSIFY: <finding at path:line>` immediately** instead of completing the
> review. Wrong routing is more expensive than your unfinished report.

On `RECLASSIFY`: re-route from §3.2 with the corrected shape, re-run the affected
stage, log it. If this never fires across a long run, you're rubber-stamping your
own classifier.

### 3.5 Exemplar-then-sweep (repetitive diffs)

When the same transformation is applied across many files, do **not** review them
all at depth:

1. **Exemplar (opus, 1 agent):** pick 3-5 files spanning the variation —
   simplest, most complex, structurally odd. Output is not findings but an
   **explicit invariant checklist**: what must be true of every correct site,
   what a partial migration looks like, what the legitimate exceptions are.
2. **Sweep (haiku, parallel, batched files):** each agent gets the checklist and
   a batch. Output is closed-form, one line per site:
   `path:line: MIGRATED | PARTIAL | UNTOUCHED | DEVIATES: <invariant>`. No prose,
   no severity, no judgment.
3. Escalate only non-`MIGRATED` lines to a real angle.

If any invariant is expressible as a command (`rg 'oldSymbol'` returns zero
hits), **run the command instead of asking an agent**. If sweep agents are being
asked to judge anything, the checklist was underspecified — rewrite the
checklist, don't upgrade the model.

---

## 4. Spawn rules

- One angle per subagent. Merging angles destroys the separation that makes this
  work.
- `run_in_background: true`, all agents of a stage launched in one message.
  Stages are sequential; angles within a stage are parallel.
- **Angles within a stage are blind to each other.** Never pass one angle's
  findings to a peer in the same stage — independent hits on the same `path:line`
  are only signal if they were actually independent.
- **Subagents are read-only.** Only you write files. Five background agents
  editing the same tree is not a review, it's a merge conflict generator.

---

## 5. Staged execution

**Stage A — structural.** Angles 1, 4, 7. Judges *whether the approach is right*.
Gate: verify and fix 🔴/🟠 before proceeding. A 🔴 implying redesign → hard stop
(a). Zero 🔴/🟠 on a low-stakes diff → skip to Stage C.

**Stage B — behavioral.** Angles 2, 3, on the post-Stage-A diff. Judges *whether
the right approach was implemented soundly*. Gate: fix 🔴/🟠 before Stage C.

**Stage C — local.** Angles 5, 6, plus any sweep. Judges *whether the lines are
internally consistent*. Runs only on a diff that has stopped moving.

Sequencing is not stylistic: local hygiene findings on code that a structural fix
is about to rewrite are findings you paid for and then deleted.

---

## 6. Shared rules (every angle, every stage)

- **Read the diff as if you didn't write it.** No benefit of the doubt.
- **Don't re-report the pre-pass.**
- **Verify before you flag — but batch it.** Run the check, or mark the finding
  `unverified` **and state the exact command that would settle it**. The
  orchestrator batch-runs all unverified checks once, deduped.
- **State old-vs-new for every changed line.** What the
  behavior/signal/field/payload was *before* and what it is *now*. The
  most-missed defects are silent deletions — a stack trace, a metric, a Sentry
  signal, a log field, a branch — that vanish without the diff looking like a
  removal.
- **No prose nits** unless wording changes meaning or misleads.
- **Concrete `path:line` only.** Never "the doc" or "somewhere in the config".
- **Don't pad.** Zero findings is a valid, honest result. Budget: ~1 finding per
  20 changed lines, hard cap 60 output lines. Under autonomy a padded finding
  becomes an unwanted code change — inflation has teeth here.
- **Reclassification clause** (§3.4).

**Severity:**

- `🔴 critical` — silent breakage, data-correctness bug, security exposure, or
  something that will actively mislead the next person/agent who reads it.
- `🟠 major` — real fragility with no documented mitigation; bites under a
  realistic condition.
- `🟡 medium` — drift, unlikely-but-unhandled edge, defensibly deferrable guard.
- `🟢 minor` — speculative / cosmetic-with-meaning. Sparingly.

**Line format:**

```
[angle] path:line: <emoji> <severity>: <problem>. <fix>. [verified|unverified: <cmd>]
```

**Verdict line:** `ship it` only if zero 🔴/🟠, else
`blocking: <N> critical, <M> major`.

---

## 7. The angles

**1 — Counterfactual.** For every design choice, imagine the opposite was picked.
What edge case does the opposite catch that this one misses? What's the cost of
each? Which choices look arbitrary, and is the arbitrariness documented anywhere?
Probe load-bearing decisions — the ones where, if wrong, a lot breaks. For each:
what would have to be true for the chosen option to be wrong, and would the diff
surface that?

**2 — Persona walkthrough.** Three concrete tasks, three personas: (a) new hire,
zero context, week two, handed one onboarding task — where do they get stuck or
guess wrong; (b) the engineer editing this file in six months without re-reading
the PR — what trap do they fall into because the reasoning lives only in the PR;
(c) the careless contributor who writes the natural-but-wrong thing — does
anything stop them. Name the task and the gap. "Could be clearer" doesn't count;
show the failure.

**3 — Implicit assumptions.** Unstated assumptions the author didn't realise they
made: tooling versions, working directory, shell, encoding, locale/timezone, OS
case-sensitivity, path separators, ordering guarantees, idempotency, re-run
behavior, partial-failure / interrupted-midway state, concurrency, ID/format
assumptions (numeric vs string, length, charset), and which *other* systems read
the same artifacts and may parse them differently. Name each assumption and the
condition under which it bites.

**4 — What isn't here.** Undefined corners of the contract. Doc claims with no
test, example, or source behind them. New load-bearing logic with no test. Rules
with an obvious unlisted exception. Adjacent systems (CI, IDE, pre-commit hooks,
generated artifacts, downstream consumers, docs) that interact with this change
but aren't addressed. New code paths with no error handling. Signals (metrics,
alerts, breadcrumbs) a downstream team relied on that this change removes or
bypasses.

**5 — Self-consistency / literal hygiene.** Only the added/changed lines, no
downstream reasoning. (a) Does every comment still match the code it sits on — if
it enumerates cases, does the code produce all of them, no more, no fewer?
(b) Does each call match the callee's declared types, shapes, conventions?
(c) What field, signal, metric, stack trace, or log detail existed on this exact
path before and is now silently dropped? (d) Anything that can be `0`, empty,
`undefined`, or negative handled as if it can't — off-by-one, nullish-coalescing
preserving a wrong zero, fallbacks masking real state? (e) Branches, regex
alternations, or conditions that can never fire on real inputs. Cite the
contradicting neighbor line every time. Do not speculate about production impact.

**6 — Invariance** (refactors, deletions, dep bumps). The change *claims*
behavior is unchanged. Disprove it. Enumerate every call site / consumer: fully
migrated, partially, or untouched? Where the transformation is "equivalent," name
the input class for which it isn't — evaluation order, short-circuiting,
exception types, coercion, default args, mutation vs copy, async timing. What did
the old path emit (logs, metrics, errors, traces) that the new one doesn't? For
deletions: who still reads this — dashboards, alerts, downstream jobs, external
consumers, docs, dangling flags? Prefer commands over prose.

**7 — Threat model** (stakes-elevated). Where does this move a trust boundary?
New input reaching a privileged path, new deserialization, new external call,
widened permission, weakened validation, secret in a new place, new log line that
might carry PII, timing or error-message oracle. State the attacker, the required
capability, and the concrete diff line that enables it.

---

## 8. Consume → fix → verify

1. **Dedupe by location.** Same `path:line` from multiple angles *in the same
   stage* is real signal — promote it, record the count. Cross-stage agreement is
   not independent; don't count it as corroboration.
2. **Batch-run unverified checks** in one pass. Drop what doesn't survive; log as
   `dropped-unverified`.
3. **Verify every 🔴 and 🟠 yourself** before touching code. A subagent's summary
   describes what it intended to find, not necessarily what's true. Open the
   file, read the installed library source, run the command. Downgrade what
   fails, upgrade what was under-rated. **No unverified finding gets fixed** —
   under autonomy that's how a hallucination becomes a commit.
4. **Fix discipline:**
   - Minimal fix for the stated finding. No adjacent refactoring, no
     reformatting, no drive-by improvements.
   - After each fix, re-run the pre-pass. **If a fix turns the pre-pass red,
     revert it** and log the finding as `DECISION-REQUIRED`.
   - A fix that would change intended product behavior is not a fix — log it
     `DECISION-REQUIRED` (§2b) and move on.
   - Real-but-out-of-scope findings (pre-existing bugs the diff merely exposed) →
     `deferred`, collected into a follow-up list. Not scope creep.
   - Dispatch mechanical fixes (renames, missing guards, comment/code
     reconciliation) to a **sonnet** fixer with the finding line as its entire
     brief. Reserve your own context for judgment.
   - Commit per finding-cluster with the finding id in the message. Locally only.
5. **Update the ledger.**

---

## 9. Loop & exit

**Round N+1 reviews the fix delta, not the whole diff.** Hand each angle: the
diff of the fixes only, the prior findings with claimed resolution (confirm or
refute), and an instruction to hunt for what the fixes broke. Re-run the full
diff only if fixes touched more than ~30% of the changed surface or altered the
design. Otherwise you're re-reading stable code at full price.

**Exit when all of these hold:**

- A full round produced zero 🔴 and zero 🟠;
- pre-pass is green;
- no open `RECLASSIFY`;
- every remaining 🟡/🟢 is explicitly `deferred` with a one-line reason.

**Or exit early when any of these holds** — these are successes, not failures:

- A round produced only speculative findings and no new real defect (diminishing
  returns).
- Marginal yield fell below one new 🔴/🟠 per round.
- Round cap (§3.3) or spawn cap reached — report state honestly, per §2.
- Hard stop (a), (b-blocking), or (c) fired.

---

## 10. Final report (one, at the end)

```
## Verdict: ship it | blocking: <N> critical, <M> major | halted: <reason>

**Rounds:** <n>   **Spawns:** <n opus / n sonnet / n haiku>   **Confidence:** <high/medium/low + why>

### Fixed
<id> path:line 🔴 <one line: what was wrong, what you changed>

### Needs your decision
<id> path:line — <the choice, the options, your recommendation>

### Deferred (with reason)
<id> path:line 🟡 <why it's safe to defer>

### Follow-up tickets (out of scope for this diff)
<one line each>

### What I did not review
<paths excluded and why>
```

No raw subagent dumps. No per-round narration. If the verdict is `ship it`, the
first three sections should be short and the last one honest.

---

## 11. Trivia clause

If the diff is under ~50 lines, carries no stakes flag, and the pre-pass is
green: skip staging, run angles 4 and 5 at sonnet, one round, report. The
machinery costs more than the change.
