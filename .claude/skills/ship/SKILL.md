---
name: ship
description: >
  Carry a change to the veld repo from empty diff to merged PR the way this
  project expects — autonomous implementation, adversarial review rounds, draft
  PR, wait for green CI, and (when authorized) bypass-merge. Opens with a short
  kickoff questionnaire that sets review depth and merge policy for the rest of
  the run. Use when the maintainer says "ship this", "build and merge X",
  "implement and open a PR", "take this to merge", or hands over a feature/fix to
  carry all the way to main. Not for one-off edits with no PR.
---

# ship — carry a veld change to merge

You are the engineer of record for this change. Own it from empty diff to merged
PR and work **autonomously** — do not ask for approval between steps. The only
reasons to stop and ask are in **When to involve the human** below.

Read [AGENTS.md](../../../AGENTS.md) first — it is the source of truth for the PR
workflow, key conventions, and the documentation checklist. This skill is the
operational wrapper around it, not a replacement. Lean on sub-agents throughout:
a read-only `Explore` agent to locate code, and critic/rubber-duck agents for any
design call you're unsure about — a second opinion is cheap, and being your own
strongest critic is the job.

## Step 0 — Kickoff questionnaire (ask once, up front)

Before writing code, run a short interview so the rest of the run is unattended.
State the feature/scope in your own words if it isn't already clear, then use
`AskUserQuestion` for the settings below (skip any the maintainer already stated
in their request):

1. **Review depth**
   - *Full loop (recommended)* — repeated 5-angle adversarial review per
     `docs/agentic-review.md`; fix, re-review on the post-fix diff, loop until a
     round is clean.
   - *Trivial (3-angle, round cap 2)* — counterfactual + what-isn't-here +
     self-consistency, for a small / mechanical change. Still loops (Step 4), just
     with the lower cap the review doc's tuning note allows for low-risk diffs.
   - *None* — skip review. AGENTS.md makes the multi-angle review **mandatory for
     every change**, so this is the maintainer explicitly overriding that step;
     confirm they mean it and note the risk in the PR body.
2. **Merge policy** (AGENTS.md's default posture is **ask-first**; bypass is the
   exception and requires the maintainer's explicit upfront authorization, which
   this questionnaire captures)
   - *Bypass-merge on green* — merge with admin bypass the moment CI is green.
   - *Open PR, stop for human* — push the draft PR, report, do not merge.
   - *Human PR review* — push, request review, wait for approval, then merge.
3. **Docs & tests** (only if ambiguous) — confirm whether the change adds
   user-visible surface (triggers the AGENTS.md docs checklist) or is purely
   internal.

Record the answers and follow them for the rest of the run. Do not re-ask.

## Step 1 — Understand before touching code

- Prefer a read-only investigator (`Explore` sub-agent) for "where is X / what
  calls Y" so main context holds decisions, not file dumps.
- State the root cause / design in one paragraph before editing. If you can't,
  keep investigating.
- Think from the two angles this repo cares about:
  - **DX** — what does a human running the CLI see and feel?
  - **Coding-agent ergonomics** — how does an agent driving the CLI consume
    this? Favour `--json`, stable output, and state that is observable early.

## Step 2 — Implement

- Match surrounding code: naming, comment density, error handling, idioms.
- Honour the AGENTS.md key conventions (daemon `PATH`, brand on every HTML
  surface, `{var}` vs `${var}`, `command` vs `start_server` semantics).
- Build, then `rustup update stable` (CI uses floating stable — drift blocks it),
  `cargo clippy --workspace --all-targets`, `cargo fmt --all`, and run the tests
  as you go.

## Step 3 — Docs audit

Walk the [documentation checklist](../../../AGENTS.md#documentation-checklist).
If the change adds config fields, CLI flags, subcommands, or user-visible
behaviour, update **all** listed files. Purely-internal changes are exempt — say
so explicitly rather than skipping silently.

Explicitly ask **"does the marketing website need to change?"** For any
user-visible capability, decide whether it belongs on `website/index.html`
(features grid, CLI reference, sharing, the `for the nerds` architecture
diagram) and update it — plus `website/llms-full.txt` — if so. The site should
stay a current picture of what veld does, not drift behind the CLI. State your
call either way. When the change is website-facing, prefer serving it locally
(`veld start website:local`) and collaborating through `veld feedback` before
shipping.

## Step 4 — Review rounds

Run the review at the depth chosen in Step 0, following
[docs/agentic-review.md](../../../docs/agentic-review.md):

- Spawn the review angles as **parallel background sub-agents**, `model: opus`,
  one angle each. Give every angle the exact diff target
  (`git diff origin/main...HEAD`), the intent in 1-3 sentences, and where to read
  the real dependency source.
- Verify every critical/major yourself before acting. Fix all 🔴/🟠 (and cheap
  🟡). Re-run the angles on the post-fix diff — fixes introduce their own
  defects. Loop until a round is clean or the round cap hits.
- Report a deduped, severity-sorted summary — not the raw agent output.

## Step 5 — PR

- Branch if on `main` (never commit to main directly; if `main` is checked out
  in another worktree, branch from `origin/main`). Commit with a Conventional
  Commits message.
- Push and open a **draft** PR with a clear body: what changed, why, root cause,
  test evidence, reviewer-scope notes, and any known follow-ups.

## Step 6 — CI and merge

- **Wait for CI to actually go green.** Never assume checks are missing because
  they haven't started — poll until they report. A red or pending check is not a
  pass. When a check fails, read the failing job's log and fix the real cause;
  don't retry blind (a rerun re-runs the same commit).
- Then apply the Step 0 merge policy:
  - *Bypass-merge on green* → `gh pr ready` then `gh pr merge --squash --admin`,
    confirm merged, report the merge commit.
  - *Open PR, stop* → report the PR link and stop.
  - *Human PR review* → request review, wait for approval, then merge.

## When to involve the human

Stay autonomous. Only stop to ask when one of these holds:

- **Huge PR** — roughly ±10k lines changed. Surface the scope and a plan before
  going further.
- **Vision call** — a decision only the maintainer can make about the
  product / CLI / UI direction, where either option is technically fine but the
  choice sets a precedent.
- **Merge policy says so** — the maintainer chose "stop for human" or "human PR
  review" in Step 0.
- **Blocked** — a genuinely irreversible or destructive action with no safe
  default, or missing access/credentials you can't obtain.

Everything else — naming, refactors, test choices, fixing your own review
findings — you decide.
