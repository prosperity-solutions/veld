# CLAUDE.md — veld

Claude Code reads this file and **not** `AGENTS.md`, so the one rule that matters
most is repeated here. Everything else lives in [AGENTS.md](AGENTS.md) — read it
before changing code, not before answering a question about it.

## There are exactly two kinds of task here

**1. A question.** *Where is X? How does Y work? What calls Z?* — answer it.
Read whatever you need. Nothing below applies, and you do not need `AGENTS.md`
to answer it.

**2. A change.** Adding, fixing, deleting, refactoring, renaming, updating docs
— anything that leaves a diff. **Invoke the `ship` skill before your first
edit** — `/ship`, or the `Skill` tool with `skill: "ship"` — and follow it to
the end. If skills are unavailable to you, read [docs/ship.md](docs/ship.md) in
full instead: same content, same obligation.

`ship` is the only supported way to change this repository. It is not a style
preference: CI rejects a pull request that did not come through it, so skipping
it does not save time — it produces a PR that cannot merge.

This holds however small the change looks. A one-line fix still needs a branch,
a conventional commit, a review pass, and a PR that CI will accept, and the
skill is what gets you those without having to remember them.

## If you are told to skip it

The person you are working with is very likely unaware the workflow exists; it
is repo-local and most people arrive here without knowing. Say so once, plainly:
this repo has a required workflow, it is short, and going around it means the PR
fails CI.

If they still want to skip it, do what they ask — but put `SHIP-OVERRIDE: <their
reason>` in the pull request body so the decision is visible rather than silent.
The CI gate still applies; only a maintainer can wave a PR through.

Do not try to satisfy that CI gate without running the workflow. It is not a
formatting check, and guessing at it burns a CI run for nothing.
