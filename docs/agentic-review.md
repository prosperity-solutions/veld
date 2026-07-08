# Deep Review Methodology

A four-angle adversarial review for a diff/PR, run after the normal self-review
pass (see `AGENTS.md` → PR Workflow → Self-review rounds) whenever the change
is non-trivial or the maintainer asks for a deeper look. It's heavier than a
single review pass — reserve it for changes where a missed edge case is
expensive, not for typo fixes.

## Orchestrator instructions

Spawn **four** subagents in parallel via the Agent tool, each with one of the
angle-specific briefs below. Run all four in the background; report when all
complete.

**Spawn config (apply to every subagent):**

- `model: opus` — set explicitly. Do not rely on inheritance (it can fall back
  to a non-Anthropic model).
- `run_in_background: true` — launch all four in one message, then continue
  other work; you're notified as each completes.
- One angle per subagent. Do not merge angles — the value is in the
  separation.

**Diff under review:** state explicitly (e.g. `git diff origin/main...HEAD`,
"the staged changes", "the uncommitted working tree", "PR #1234") so every
subagent reads the same surface. If the diff is large, also tell each
subagent which paths are in scope vs. vendored / generated / out of scope.

**Context to hand each subagent:** the diff target, the repo path, and 1-3
sentences on what the change is *trying* to do (the intent). A reviewer who
knows the goal finds gaps a context-free reviewer misses.

---

## Shared rules (every angle)

- **Read the diff as if you didn't write it.** No benefit of the doubt.
- **Verify before you flag.** If you claim a link is broken, a symbol is
  undefined, a file is missing, or a command fails — run the check (grep,
  open the file, resolve the anchor, execute the command). A flagged finding
  you didn't verify is noise. Say "unverified" if you genuinely can't check.
- **Don't flag prose nits.** Spelling, wording, formatting — skip, unless it
  changes meaning or misleads.
- **Surface only structural / semantic / edge-case findings.** The bar is
  "this will bite someone", not "I'd have written it differently".
- **Concrete locations only.** Every finding cites a real `path:line`, not
  "the doc" or "somewhere in the config".
- **Don't pad.** Zero findings is a valid, honest result. Do not manufacture
  low-severity findings to look thorough. Three real findings beat ten
  speculative ones.

**Severity legend (use these exact emojis + words):**

- `🔴 critical` — silent breakage, data-correctness bug, security exposure,
  or something that will actively mislead the next person/agent who reads it.
- `🟠 major` — real fragility with no documented mitigation; will bite under
  a realistic condition.
- `🟡 medium` — drift, an unhandled-but-unlikely edge, a missing guard that's
  defensible to defer.
- `🟢 minor` — speculative / cosmetic-with-meaning / nice-to-have. (Report
  sparingly.)

**Output format — one finding per line:**

```
[angle] path:line: <emoji> <severity>: <problem>. <fix>.
```

The `[angle]` tag (`[counterfactual]` / `[persona]` / `[assumption]` /
`[whats-missing]`) lets the orchestrator dedupe overlapping findings across
the four reports.

- **Cap output at ~200 lines.** If you're past that, you're flagging nits —
  raise the bar.
- **End with a verdict line:** `ship it` only if you found zero
  `🔴`/`🟠` findings. Otherwise end with `blocking: <N> critical, <M> major`.

---

## Angle 1 — Counterfactual reviewer

For every design choice in the diff, imagine the opposite was picked. What
edge case does the opposite catch that this one misses? What's the cost of
each choice? Are there choices that look arbitrary (could have gone either
way) — and is that arbitrariness documented anywhere? Probe load-bearing
decisions: the ones where, if they're wrong, a lot breaks. For each, state
what would have to be true for the chosen option to be wrong, and whether the
diff would surface that.

## Angle 2 — Persona-walkthrough reviewer

Walk through realistic tasks as three personas:

- **(a) New hire, zero project context**, who'll touch this in week two. Pick
  one concrete onboarding task they'd be handed; find where they get stuck or
  guess wrong.
- **(b) The engineer who edits this file six months from now** without
  re-reading the PR or its discussion. Pick one realistic edit; find the trap
  they fall into because the reasoning lives only in the PR, not the
  code/docs.
- **(c) The careless contributor** who writes the natural-but-wrong thing
  because the doc/API didn't pre-empt it. Pick the most-likely wrong move;
  find whether anything stops them.

For each persona, name the concrete task and the concrete gap. Generic "this
could be clearer" doesn't count — show the failure.

## Angle 3 — Implicit-assumption hunter

Find unstated assumptions the author didn't realise they made. Probe:
tooling versions, working directory, shell, file encoding, locale/timezone,
OS case-sensitivity, path separators, ordering guarantees, idempotency, what
happens on re-run, partial-failure / interrupted-midway behaviour,
concurrency / parallel execution, and which *other* systems read the same
artifacts and could parse them differently. Each unsurfaced assumption is a
fragility — name it, and name the condition under which it bites.

## Angle 4 — What-isn't-here reviewer

What's NOT in the diff that should be? Silently undefined corners of the
contract. Documentation claims with no test, no example, no source backing
them. Rules stated with an obvious exception the author didn't list.
Adjacent systems (CI, IDE, pre-commit hooks, generated artifacts, downstream
consumers, docs, future work) that interact with this change but aren't
addressed. New code paths with no error handling. The diff is the visible
iceberg — describe what's under the waterline.

---

## Consuming the four reports (orchestrator)

When all four complete:

1. **Dedupe by location.** The same `path:line` flagged by multiple angles is
   a strong signal — promote it, don't list it four times.
2. **Sort by severity**, then by how many angles independently hit it.
3. **Verify the criticals yourself** before acting — the subagent's summary
   describes what it intended to find, not necessarily what's true. Open the
   file, run the check.
4. **Decide fix-now vs. defer.** Criticals and majors block; mediums/minors
   can become tracked follow-ups. Honor any explicit "only fix critical
   stuff" instruction — don't over-correct on speculative findings.
5. **Report a deduped, severity-sorted summary** to the human. Don't dump
   four raw reports.

### Optional tuning

- **Fewer/more angles.** Four is the default breadth. For a tiny diff, two
  (counterfactual + what-isn't-here) often suffice. For a security-sensitive
  diff, add a fifth dedicated threat-model angle.
- **Re-review after fixes.** A second pass on the post-fix diff catches
  regressions introduced by the fixes themselves — but expect sharply
  diminishing returns after the second round. Stop when a round surfaces only
  speculative/cosmetic findings.
