---
name: ship
description: >
  Carry a change to the veld repo from empty diff to merged PR the way this
  project expects — autonomous implementation, adversarial review rounds, draft
  PR, mark ready only once the local review is done (CI does not run on drafts),
  wait for green CI, and (when authorized) bypass-merge. Opens with a short
  kickoff questionnaire that sets review depth, merge policy, and hands-on test
  checkpoints for the rest of the run. Use when the maintainer says "ship this", "build and merge X",
  "implement and open a PR", "take this to merge", or hands over a feature/fix to
  carry all the way to main. Not for one-off edits with no PR.
metadata:
  # Contributor-only dev tool. `npx skills` scans `.claude/skills/` alongside
  # `skills/`, so without this flag `npx skills add prosperity-solutions/veld`
  # would install /ship into unrelated projects. `internal: true` makes the
  # skills CLI skip it in both its clone and GitHub-tree discovery paths.
  internal: true
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
   - *Super light (`SPAWNS: 1`, `ROUNDS: 1`)* — for a tiny, obviously-scoped fix
     (a handful of lines, one clear behavior change, no new surface). One sonnet
     agent reads the diff once for correctness — no staged angles, no separate
     verify pass — and reports findings directly. This is a further step down
     from Light's own trivia-clause floor (§11's angles 4+5, one round); reach
     for it only when a second opinion would be reading the same three lines
     twice. Skips the loop's ledger and multi-round structure entirely — there
     is no round two to carry state into.
   - *None* — skip review. AGENTS.md makes the multi-angle review **mandatory for
     every change**, so this is the maintainer explicitly overriding that step;
     confirm they mean it and note the risk in the PR body.

   The stakes override (§3.3 — privileged helper, secrets/relay tokens, gateway
   auth, proxy headers, daemon API, SQLite migrations) is **not** downgradable by
   this answer. If *Light* or *Super light* is chosen and the diff turns out to
   touch one of those paths, run the standard loop and say so in the final report.
2. **Merge policy** (AGENTS.md's default posture is **ask-first**; bypass is the
   exception and requires the maintainer's explicit upfront authorization, which
   this questionnaire captures)
   - *Bypass-merge on green* — mark ready, merge with admin bypass the moment CI
     is green.
   - *Open PR, stop for human* — push, mark ready so CI actually runs, report the
     link and the CI result, do not merge.
   - *Human PR review* — push, mark ready, request review, wait for approval,
     then merge.

   All three end with the PR marked ready, because **CI does not run on drafts**
   (AGENTS.md → CI cost convention). "Stop for human" does not mean "leave it a
   draft" — a draft PR handed over with no checks is a PR the maintainer has to
   flip themselves to learn anything. What differs between the three is who
   merges, not whether CI runs.
3. **Hands-on test checkpoints** — orthogonal to the merge policy: does the
   maintainer want to drive the change themselves before it moves on? This is the
   house style for anything with a UI or a new CLI surface, because a review
   subagent cannot see that a graph renders wrong.
   - *One checkpoint, before review (recommended — the house default)* —
     implement, hand over so the maintainer drives the feature, then run the
     review loop and everything after it unattended. This is the proposed answer
     for a user-visible change: the checkpoint that catches what a subagent
     cannot see is the one on the *feature*, and it is worth most while the
     design is still cheap to change. Put it first in the question's options.
   - *Two checkpoints* — the above plus a second hand-over after the review
     fixes, for a regression pass. See **Checkpointed autonomy** below. Reach for
     it when the review is likely to touch rendering, wire shapes, or CLI output
     rather than its own fixes.
   - *One checkpoint, after review* — implement and review unattended, then hand
     over once before the PR.
   - *None* — fully unattended from kickoff to the merge policy's endpoint.
4. **Docs & tests** (only if ambiguous) — confirm whether the change adds
   user-visible surface (triggers the AGENTS.md docs checklist) or is purely
   internal.

Record the answers and follow them for the rest of the run. Do not re-ask.

### Checkpointed autonomy

The default working mode for a user-visible change in this repo. Autonomy is not
all-or-nothing: the maintainer tests the running software, and everything after
that point is unattended.

The house default is **one checkpoint, before the review** — the shape to propose
first:

```
implement  →  ⏸ HAND OVER (maintainer drives it)  →  review loop  →  PR  →
green CI  →  merge policy
```

The two-checkpoint variant adds a second stop for a regression pass, and is the
answer when the review is likely to touch rendering, wire shapes, or CLI output:

```
implement  →  ⏸ HAND OVER  →  review loop  →  ⏸ HAND OVER AGAIN  →  PR  →
green CI  →  merge policy
```

Rules that make it work:

- **A checkpoint is a full stop, not a status ping.** Build first, confirm it
  runs, then report what to exercise and what you changed since they last looked.
  Do not start the next step "while they check".
- **Name the exercise.** Concrete commands and concrete screens, not "please
  test". The maintainer should not have to reconstruct your feature's surface.
- **A second checkpoint, when chosen, exists because review fixes are code too.**
  A review round that touches rendering, wire shapes, or CLI output can regress
  what was hand-verified at checkpoint one. Say explicitly which review fixes are
  behaviour-visible and therefore worth re-driving. When the maintainer chose one
  checkpoint before the review, say the same thing in the **final report**
  instead — they get one shot at spotting a regression, so it has to be named
  rather than left in the diff.
- **Resume without re-asking.** Their "looks good" / "continue" is the signal to
  run the rest of the chosen policy to completion, including a bypass merge if
  that's what they picked. Feedback instead of approval means fix it and
  re-present the same checkpoint — a checkpoint can repeat.
- **Never launch the desktop app yourself** — build it and hand over (see
  `desktop/ARCHITECTURE.md`).

## Step 1 — Understand before touching code

- **Stale-check first.** This repo's worktrees drift when `main` is not updated,
  and a stale branch lacks the latest DB migrations (so its schema tests fail
  and its PRs conflict late). Run `just stale-check` before starting work, again
  before the review loop, and again before opening the PR. If it reports the
  branch is behind, `git fetch origin && git rebase origin/main` (or fast-forward
  if unpushed) before proceeding — do not build on a stale base.
- Prefer a read-only investigator (`Explore` sub-agent) for "where is X / what
  calls Y" so main context holds decisions, not file dumps.
- **Classify the feature as core or customization** using
  [`docs/extensions-vision.md`](../../../docs/extensions-vision.md) — the
  universal-primitive / data-contract tests. State the verdict in one sentence.
  A **core** feature proceeds normally. A **customization** feature (anything
  that needs a provider API, a provider-specific schema, or provider-specific
  auth) is *not built here*; instead it is added to the extension backlog in
  that doc in Step 3. Do not silently drop a customization feature request.
- State the root cause / design in one paragraph before editing. If you can't,
  keep investigating.
- Think from the two angles this repo cares about:
  - **DX** — what does a human running the CLI see and feel?
  - **Coding-agent ergonomics** — how does an agent driving the CLI consume
    this? Favour `--json`, stable output, and state that is observable early.

## Step 1.5 — Spar the load-bearing forks (only when there are any)

If Step 1 surfaced a decision that is **expensive to reverse** — it lands in a
schema, a migration, a wire format, a persisted value, or a surface users build
habits on — run the design-divergence stage in
[docs/agentic-review.md §0](../../../docs/agentic-review.md) before writing code.
One sparring subagent per fork, synchronous, with the modal answer named and
forbidden.

Skip it otherwise, and say you skipped it. Most changes have no such fork, and
running this on a naming choice burns tokens to be told five ways to name a field.

Three rules that make it worth the spend rather than theatre:

- **Verify the winner's load-bearing premise before you build on it** (§0.3). The
  stage raises recall and lowers precision — a confident, unusual, wrong argument
  is its characteristic failure.
- **You decide, not the agent.** Overriding a sparring round's top pick with a
  stated reason is a good outcome, not a wasted round.
- **Keep the rejected candidates** in the PR body. "Why not the obvious thing" is
  the first question a reviewer asks, and the answer is expensive to regenerate.

## Step 2 — Implement

- Match surrounding code: naming, comment density, error handling, idioms.
- Honour the AGENTS.md key conventions (daemon `PATH`, brand on every HTML
  surface, `{var}` vs `${var}`, `command` vs `start_server` semantics).
- Build, then `rustup update stable` (CI uses floating stable — drift blocks it),
  `cargo clippy --workspace --all-targets`, `cargo fmt --all`, and run the tests
  as you go. For a JS/TS change, run the Biome `lint` + `typecheck`/`test` in the
  affected surface too (`desktop/`, `crates/veld-daemon/ui`, `crates/veld-daemon/frontend`)
  — `just lint` and `just test` cover all of it in one command each.
- **Leave the worktree clean before you stop.** Building and running tests
  drifts `Cargo.lock`, and experiment scaffolding lands untracked (`.idea/`,
  a scratch dir, a prototype) — but a worktree carrying uncommitted changes or
  untracked files cannot be trashed and deleted in the IDE: `git worktree
  remove` refuses on them. Before a checkpoint hand-off, before a draft, and
  before `gh pr ready`, run `git status --short` and revert incidental
  `Cargo.lock`/build drift and untracked scaffolding. Deliberate uncommitted
  work is fine while the PR is in flight — it is the *incidental* mess that
  blocks the user (and future agents) from cleaning up. This is also what makes
  the trash flow surface these files: the IDE now lists them at trash/delete
  time, so a clean tree is the happy path for everyone.
- If Step 0 chose a pre-review checkpoint — the default *one checkpoint, before
  review*, or the two-checkpoint variant: this is **checkpoint one**. Finish the
  whole feature (including the docs audit in Step 3), leave it building and
  runnable, then hand over per **Checkpointed autonomy** and wait.

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

If Step 1 classified the change as **customization** (or it includes a
customization-shaped request), add a row to the extension backlog in
`docs/extensions-vision.md` in this same PR — feature, data contract, UI
surface, status. The backlog is the record the future config-driven system is
built against; a feature request that lands in the customization realm must be
captured there, not dropped. Never delete a backlog row without the maintainer.

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

If Step 0 chose a post-review checkpoint, the review report is **checkpoint two**:
present it, call out which fixes changed observable behaviour, and wait before
opening the PR.

## Step 5 — PR

- Branch if on `main` (never commit to main directly; if `main` is checked out
  in another worktree, branch from `origin/main`). Commit with a Conventional
  Commits message.
- Push and open a **draft** PR with a clear body: what changed, why, root cause,
  test evidence, reviewer-scope notes, and any known follow-ups.
- **CI does not run on a draft** (AGENTS.md → CI cost convention). Every job in
  `ci.yml` and `release.yml` skips while `draft == true`, so pushing here buys you
  a workflow run in which nothing executes. Don't poll it, don't push extra
  commits to "kick" it, and don't read the absence of checks as a problem — this
  is the design. Five macOS legs plus a four-target release matrix are what a
  draft run occupies.
- **Those skipped jobs report as passing checks.** The draft PR you just opened
  will show a green tick. It means nothing ran. See step 6.3 before you ever
  report a CI result.
- Note what this does and does not save *for this flow*: /ship pushes once, at
  this step, with the review already done — so the guard's payoff here is the
  `opened` run plus any push made before you mark ready (a checkpoint iteration, a
  post-PR review fix). The per-intermediate-commit waste it was written for belongs
  to flows that push repeatedly while drafting. Don't invent draft pushes to
  "use" the saving, and don't treat a short draft window as a reason to skip
  step 6.1.

## Step 6 — Ready, CI, and merge

**Mark ready first, then wait.** In that order, always: nothing reports until the
PR is ready, so waiting on a draft's checks is an infinite wait, not patience.

1. **Gate yourself before spending.** Only run `gh pr ready` once the Step 4
   review loop has met its exit criteria (or legitimately exited early per
   §9/§11) **and** the pre-pass is green. A red pre-pass never gets marked ready
   — fix it locally first. If Step 0 chose *review: None*, say in the PR body that
   the only pre-merge signal is CI.
2. `gh pr ready` — this is the step that fires `ready_for_review` and starts CI.
3. **Wait for CI to actually go green — and do not trust a green summary.**
   Never assume checks are missing because they haven't started; poll until they
   report. A red or pending check is not a pass. **A *skipped* check is also not a
   pass, and it looks exactly like one:** a draft's jobs all skip, `gh pr checks`
   files those under the `skipping` bucket, exits 0, and prints "All checks were
   successful". So the exit code cannot tell you whether anything ran. Assert on
   the buckets — and scope the assertion to the **CI** workflow, because
   `release.yml`'s push-only jobs are skipped on every healthy PR:

   ```sh
   gh pr view --json isDraft,mergeable        # isDraft must be false
   gh pr checks --json name,bucket,workflow \
     --jq '[.[] | select(.workflow == "CI" and .bucket == "skipping")]'   # must be []
   ```

   Apply that assertion **only when no CI check is `pending`** — it is a terminal
   test. The ready run's check replaces the draft's for a given job name (names do
   not duplicate), but a name whose ready-run job has not started yet still shows
   the draft's `skipping` entry; measured on #209, four CI names read `skipping`
   while `check` was in progress because their jobs `needs:` it. Read the run
   itself if you want an unambiguous answer at any moment:

   ```sh
   gh run list --workflow CI --commit "$(git rev-parse HEAD)" --json databaseId
   gh run view <id> --json jobs
   ```

   This is the failure mode to fear under autonomy: forget step 2, poll, read
   "All checks were successful", and report a green CI on a diff nothing ran.
   If *no* checks appear at all a minute or two after marking ready, the PR is
   probably `CONFLICTING` — that stops `pull_request` events firing entirely.
4. When a check fails, read the failing job's log and fix the real cause; don't
   retry blind (a rerun re-runs the same commit). **A failure you did not cause is
   still yours to fix** — check the same job on `main` to find out which it is, and
   either way the answer is a fix, not a hand-back (see *Shipping means fixing what
   you hit on the way*). A pre-existing bug masked by flakiness is the common case:
   it looks like your regression and is not, and it will keep costing whoever wins
   the coin flip next. Pushing the fix fires
   `synchronize`, and since the PR is now ready, that re-runs CI normally.
5. Then apply the Step 0 merge policy:
   - *Bypass-merge on green* → `gh pr merge --squash --admin`, confirm merged,
     report the merge commit.
   - *Open PR, stop* → report the PR link and the CI result, then stop. Leave it
     ready, not draft.
   - *Human PR review* → request review, wait for approval, then merge.

## Shipping means fixing what you hit on the way

**"Ship it" is not "ship the happy path".** If something blocks the merge, the
default is that you fix it — including things you did not write and did not break.
A red CI job caused by a pre-existing bug is still between this change and `main`,
so it is still yours to deal with.

This is the failure to avoid: diagnose the obstacle perfectly, write it up
beautifully, and hand it back with the work unmerged. That is the *appearance* of
diligence with none of the value — the maintainer now has a report to read and a
job to do, which is exactly what they delegated. **A correct diagnosis is the
beginning of the fix, not a substitute for it.**

So, on hitting an obstacle:

1. **Find the real cause.** Not the symptom, and not a guess — for CI, compare the
   same job on `main` before concluding it is yours. A pre-existing failure masked
   by flakiness looks exactly like a regression you introduced.
2. **Fix it, in this PR, and say so.** Adjacent-but-necessary work belongs here;
   it is the price of the change landing. Name it in the PR body under its own
   heading so a reviewer sees it was deliberate rather than smuggled in.
3. **Prefer the proper fix to the one that makes the symptom go away.** If two
   users are fighting over one file, give them a file each; do not `chown` after
   the fact. If the honest fix is genuinely too large for this PR, do the small
   correct thing *and* record the real one as a named follow-up — never leave a
   workaround unlabelled.

**The narrow exception — a genuine blocker with no right answer.** Stop and ask
only when fixing it properly would mean a *product decision* you cannot make, an
irreversible or destructive act, credentials you cannot obtain, or a change so
large it is its own PR. Then bring: the root cause, the options with their costs,
and your recommendation — not just the problem.

And say which of those it is. "I could not merge because X" is only acceptable
when X is on that list; otherwise it reads as a fix you decided not to do.

## When to involve the human

Stay autonomous. Only stop to ask when one of these holds:

- **Huge PR** — roughly ±10k lines changed. Surface the scope and a plan before
  going further.
- **Vision call** — a decision only the maintainer can make about the
  product / CLI / UI direction, where either option is technically fine but the
  choice sets a precedent.
- **Merge policy says so** — the maintainer chose "stop for human" or "human PR
  review" in Step 0.
- **A test checkpoint is due** — Step 0 chose checkpointed autonomy. These are
  planned stops, not failures; hand over and wait.
- **Blocked** — a genuinely irreversible or destructive action with no safe
  default, or missing access/credentials you can't obtain. Note what does *not*
  count: an obstacle that is merely unwelcome, unrelated, or somebody else's fault.
  See **Shipping means fixing what you hit on the way** above.

Everything else — naming, refactors, test choices, fixing your own review
findings, **and repairing whatever else stands between this change and `main`** —
you decide.
