# Deep Review Methodology

A multi-angle adversarial review for a diff/PR, run after the normal self-review
pass (see `AGENTS.md` → PR Workflow → Review rounds) whenever the change is
non-trivial or the maintainer asks for a deeper look. It's heavier than a single
review pass — reserve it for changes where a missed edge case is expensive, not
for typo fixes.

## Orchestrator instructions

Spawn **five** subagents in parallel via the Agent tool, each with one of the
angle-specific briefs below. Run all five in the background; report when all
complete, then drive the **review → fix → re-review loop** (see "Iterate until
confident" at the end) until only minor findings remain.

**Spawn config (apply to every subagent):**

- `model: opus` — set explicitly. Do not rely on inheritance (it can fall back
  to a non-Anthropic model).
- `run_in_background: true` — launch all in one message, then continue other
  work; you're notified as each completes.
- One angle per subagent. Do not merge angles — the value is in the separation.

**Diff under review:** state it explicitly (e.g. `git diff origin/main...HEAD`,
"the staged changes", "the uncommitted working tree", "PR #1234") in every brief
so every subagent reads the same surface. If the diff is large, also tell each
subagent which paths are in scope vs. vendored / generated / out of scope.

**Context to hand each subagent:** the diff target, the repo path, and 1-3
sentences on what the change is *trying* to do (the intent). A reviewer who
knows the goal finds gaps a context-free reviewer misses.

**Pin the dependency surface.** If the change's correctness depends on a
library's behavior (error strings, return shapes, enum values, version-specific
semantics), tell each subagent the exact installed version and where to read its
source (e.g. an installed crate path under `~/.cargo/registry/src/...`, or a
`node_modules/.pnpm/...` path). "Verify against the real thing" only works if
they know where the real thing is.

---

## Shared rules (every angle)

- **Read the diff as if you didn't write it.** No benefit of the doubt.
- **Verify before you flag.** If you claim a link is broken, a symbol is
  undefined, a file is missing, a string doesn't match, or a command fails — run
  the check (grep, open the file, resolve the anchor, read the installed library
  source, execute the command). A flagged finding you didn't verify is noise.
  Say "unverified" if you genuinely can't check.
- **State old-vs-new for every changed line.** For each meaningful change, name
  what the behavior/signal/field/payload was *before* and what it is *now*. The
  most-missed defects are silent **deletions** — a stack trace, a metric, a
  Sentry signal, a log field, a branch — that vanish without the diff looking
  like a removal. A review that only judges the new state in isolation misses
  these.
- **Don't flag prose nits.** Spelling, wording, formatting — skip, unless it
  changes meaning or misleads.
- **Surface only structural / semantic / edge-case / self-consistency
  findings.** The bar is "this will bite someone", not "I'd have written it
  differently".
- **Concrete locations only.** Every finding cites a real `path:line`, not
  "the doc" or "somewhere in the config".
- **Don't pad.** Zero findings is a valid, honest result. Do not manufacture
  low-severity findings to look thorough. Three real findings beat ten
  speculative ones.

**Severity legend (use these exact emojis + words):**

- `🔴 critical` — silent breakage, data-correctness bug, security exposure, or
  something that will actively mislead the next person/agent who reads it.
- `🟠 major` — real fragility with no documented mitigation; will bite under a
  realistic condition.
- `🟡 medium` — drift, an unhandled-but-unlikely edge, a missing guard that's
  defensible to defer.
- `🟢 minor` — speculative / cosmetic-with-meaning / nice-to-have. (Report
  sparingly.)

**Output format — one finding per line:**

```
[angle] path:line: <emoji> <severity>: <problem>. <fix>.
```

The `[angle]` tag (`[counterfactual]` / `[persona]` / `[assumption]` /
`[whats-missing]` / `[self-consistency]`) lets the orchestrator dedupe
overlapping findings across the reports.

- **Cap output at ~200 lines.** If you're past that, you're flagging nits —
  raise the bar.
- **End with a verdict line:** `ship it` only if you found zero `🔴`/`🟠`
  findings. Otherwise end with `blocking: <N> critical, <M> major`.

---

## Angle 1 — Counterfactual reviewer

For every design choice in the diff, imagine the opposite was picked. What edge
case does the opposite catch that this one misses? What's the cost of each
choice? Are there choices that look arbitrary (could have gone either way) — and
is that arbitrariness documented anywhere? Probe load-bearing decisions: the
ones where, if they're wrong, a lot breaks. For each, state what would have to
be true for the chosen option to be wrong, and whether the diff would surface
that.

## Angle 2 — Persona-walkthrough reviewer

Walk through realistic tasks as three personas:

- **(a) New hire, zero project context**, who'll touch this in week two. Pick one
  concrete onboarding task they'd be handed; find where they get stuck or guess
  wrong.
- **(b) The engineer who edits this file six months from now** without
  re-reading the PR or its discussion. Pick one realistic edit; find the trap
  they fall into because the reasoning lives only in the PR, not the code/docs.
- **(c) The careless contributor** who writes the natural-but-wrong thing
  because the doc/API didn't pre-empt it. Pick the most-likely wrong move; find
  whether anything stops them.

For each persona, name the concrete task and the concrete gap. Generic "this
could be clearer" doesn't count — show the failure.

## Angle 3 — Implicit-assumption hunter

Find unstated assumptions the author didn't realise they made. Probe: tooling
versions, working directory, shell, file encoding, locale/timezone, OS
case-sensitivity, path separators, ordering guarantees, idempotency, what
happens on re-run, partial-failure / interrupted-midway behaviour, concurrency /
parallel execution, ID/format assumptions (numeric vs string, length, charset),
and which *other* systems read the same artifacts and could parse them
differently. Each unsurfaced assumption is a fragility — name it, and name the
condition under which it bites.

## Angle 4 — What-isn't-here reviewer

What's NOT in the diff that should be? Silently undefined corners of the
contract. Documentation claims with no test, no example, no source backing them.
New load-bearing logic with no test. Rules stated with an obvious exception the
author didn't list. Adjacent systems (CI, IDE, pre-commit hooks, generated
artifacts, downstream consumers, docs, future work) that interact with this
change but aren't addressed. New code paths with no error handling. Signals
(metrics, alerts, breadcrumbs) that a downstream team relied on and this change
removes or bypasses. The diff is the visible iceberg — describe what's under the
waterline.

## Angle 5 — Diff self-consistency / literal-hygiene reviewer

Read **only the added/changed lines**, with no downstream reasoning — the
narrow, local, literal pass. This is the lens the "will it bite in production"
angles structurally under-weight. For each changed line, check:

- **(a) Comment ↔ code agreement.** Does every comment/docstring on the change
  still match the code it sits on? If a comment enumerates values, ranges, or
  cases ("first / retry / exhausted"), does the code actually produce all of
  them — no more, no fewer?
- **(b) API-convention conformance.** Does each call match the callee's declared
  parameter types, expected shapes, and conventions? Wrong-but-tolerated
  argument types, values that violate a documented contract, etc.
- **(c) Before/after payload delta.** What field, signal, metric, stack trace,
  or log detail existed on this exact path before and is now silently dropped or
  changed? (This overlaps the shared "old-vs-new" rule — here, apply it
  line-locally and exhaustively.)
- **(d) Degenerate / boundary values.** Any value that can be `0`, empty,
  `undefined`, or negative that the changed line handles as if it can't?
  Off-by-one, nullish-coalescing that preserves a wrong zero, fallbacks that
  mask a real state.
- **(e) Dead or unreachable sub-expressions.** Branches, regex alternations, or
  conditions in the new code that can never fire given the real inputs.

Cite the contradicting neighbor line for every finding. This angle does NOT
speculate about production impact — it reports local contradictions and lets the
orchestrator weigh severity.

---

## Consuming the reports (orchestrator)

When all five complete:

1. **Dedupe by location.** The same `path:line` flagged by multiple angles is a
   strong signal — promote it, don't list it five times. Record how many angles
   independently hit it.
2. **Sort by severity**, then by independent-angle count.
3. **Verify the criticals and majors yourself** before acting — the subagent's
   summary describes what it intended to find, not necessarily what's true. Open
   the file, read the installed library source, run the check. Downgrade or drop
   anything that doesn't survive your own verification; upgrade anything a
   subagent under-rated.
4. **Decide fix-now vs. defer.** Criticals and majors block; mediums/minors can
   become tracked follow-ups. Honor any explicit "only fix critical stuff"
   instruction — don't over-correct on speculative findings. Findings that are
   real but out of the diff's scope (a pre-existing bug the change merely made
   visible) become their own ticket, not scope creep into this change.
5. **Report a deduped, severity-sorted summary** to the human. Don't dump the
   raw reports.

---

## Iterate until confident (review → fix → re-review loop)

A single review pass is a snapshot, and fixes introduce their own defects. Loop:

1. **Round 1:** run all five angles, consume as above, produce the deduped
   finding list.
2. **Fix:** apply fixes for every `🔴` and `🟠` (and any `🟡` cheap enough to
   fold in). Where a fix is a design choice with real downstream consequence,
   surface it to the human before applying rather than guessing.
3. **Re-review:** re-run the angles **on the post-fix diff**. This catches
   regressions the fixes introduced — a fix for one finding routinely creates
   another. Give each angle the previous round's findings as context so it can
   confirm they're resolved and hunt for what the fix broke.
4. **Repeat** until the **stop condition** is met.

**Stop condition — stop when ANY of these holds:**

- A full round surfaces **zero `🔴` and zero `🟠`** findings, and the remaining
  `🟡`/`🟢` are ones you've consciously chosen to defer or accept. This is the
  target: "only minor stuff left, confidence high."
- A round adds **only speculative / cosmetic findings** and no new real defect —
  diminishing returns; stop even if some `🟡` remain.
- You hit a **round cap** (default: 3 rounds; raise to 4-5 only for
  security-sensitive or data-correctness-critical diffs). Expect sharply
  diminishing returns after round 2.

**Escalate instead of looping forever.** If a `🔴`/`🟠` keeps resurfacing across
two rounds because the fix and the review disagree on what "correct" is, that's
not a loop to grind — it's a design question. Stop and surface the disagreement
to the human with both positions stated. Same if fixes are ping-ponging (each
round's fix reintroduces a prior round's finding).

**Report per round.** After each round tell the human: findings this round
(deduped, severity-sorted), what you fixed, what you deferred and why, and the
current confidence level ("2 majors → 0 majors, 1 medium deferred to ticket
#X, high confidence"). The human can halt the loop at any round.

---

## Optional tuning

- **Fewer/more angles.** Five is the default breadth. For a tiny diff, three
  (counterfactual + what-isn't-here + self-consistency) often suffice. For a
  security-sensitive diff, add a sixth dedicated threat-model angle.
- **Round cap** scales with stakes: 2 for a low-risk change, 3 default, 4-5 for
  money/PII/auth paths.
- **Skip the loop** only when explicitly told "one pass, findings only, no
  fixes" — then run round 1, report, and stop.
