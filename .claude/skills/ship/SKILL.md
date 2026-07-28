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

1. **Review depth** — sets the `SPAWNS` / `ROUNDS` caps of the autonomous review
   loop in [docs/agentic-review.md](../../../docs/agentic-review.md). Everything
   else about the loop (staging, model routing, ledger, exit criteria) is the
   doc's, not yours to negotiate.
   - *Standard loop (recommended)* — the doc as written: `SPAWNS: 14` (max 6
     opus), `ROUNDS: auto` (2 low-risk / 3 default / 5 stakes-elevated).
   - *Deep* — raise the caps (`SPAWNS: 24`, `ROUNDS: 5`) for a change the
     maintainer flags as expensive-if-wrong beyond what §3.3 auto-detects.
   - *Light (`SPAWNS: 6`, `ROUNDS: 2`)* — for a small / mechanical change. The
     doc's own trivia clause (§11) still applies underneath: a sub-50-line,
     no-stakes diff with a green pre-pass gets angles 4+5 at sonnet, one round.
   - *None* — skip review. AGENTS.md makes the multi-angle review **mandatory for
     every change**, so this is the maintainer explicitly overriding that step;
     confirm they mean it and note the risk in the PR body.

   The stakes override (§3.3 — privileged helper, secrets/relay tokens, gateway
   auth, proxy headers, daemon API, SQLite migrations) is **not** downgradable by
   this answer. If *Light* is chosen and the diff turns out to touch one of those
   paths, run the standard loop and say so in the final report.
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

## Step 4 — Review loop

Run the **autonomous multi-angle review loop** in
[docs/agentic-review.md](../../../docs/agentic-review.md) at the depth chosen in
Step 0. That doc is the operative spec — follow it end to end rather than
improvising a review. The parts most easily skipped, and therefore worth naming
here:

- **Pre-pass first** (§1.1) — `rustup update stable`, clippy, `cargo fmt --check`,
  tests, plus the UI checks when `crates/veld-daemon/ui` is touched. Red pre-pass
  → fix before spawning anything. Its output is out of scope for every subagent.
- **Build the context pack once** (§1) and hand the same one to every angle:
  diff target, intent, in/out of scope paths, change shape, and the pinned
  dependency source paths (`~/.cargo/registry/src/...`).
- **Stages A → B → C** (§5), angles blind to each other within a stage,
  `run_in_background: true`, `model:` set explicitly per the §3.2 routing table —
  not opus for everything.
- **Verify every 🔴/🟠 yourself before fixing** (§8.3). No unverified finding gets
  fixed — under autonomy that's how a hallucination becomes a commit.
- **Ledger** at `notes/review/ledger.md` (gitignored), updated every round —
  it's your memory across compaction. Keep it out of your commits.
- **Report once, at the end**, in the §10 format. No per-round narration, no raw
  subagent dumps.

The loop's hard stops (§2) are the *review's* escalation points and they compose
with this skill's **When to involve the human** list — a redesign-class 🔴 or a
deadlock halts the run and comes back to the maintainer; a `DECISION-REQUIRED`
finding does not halt the loop but must appear in the final report and, if it
gates the PR, in the PR body.

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
