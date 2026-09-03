#!/usr/bin/env bash
#
# Reminds an agent that this repo has a required workflow, at two moments: when a
# prompt arrives, and on push.
#
# It never blocks, and it deliberately does not touch Claude Code's permission
# decisions. An earlier version answered the `PreToolUse` hook with
# `permissionDecision: "allow"` on the theory that it "allows the call and
# attaches a note" — it does not. It bypasses the user's Edit/Write
# confirmation, and because the hook fired only when the marker was *absent* it
# would have auto-approved edits for exactly the people who had not opted into
# the workflow. `UserPromptSubmit`'s `additionalContext` is the only mode that
# actually puts text in front of the model, so it is the only one used here.
#
# Local enforcement is advisory on purpose. A colleague who has just cloned the
# repo has not yet accepted Claude Code's workspace-trust prompt, so these hooks
# may not run for them at all, and a gate that only sometimes fires must not be
# the gate anything depends on. The one that does is CI's `ship` job
# (tests/validate-ship-stamp.sh).
set -euo pipefail

mode=${1:?usage: ship-nag.sh prompt|push}

root=${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}
marker=$root/.veld-ship.json
# `symbolic-ref`, not `rev-parse --abbrev-ref`: the latter returns the literal
# string `HEAD` when detached, which made the marker's branch field meaningless.
branch=$(git -C "$root" symbolic-ref --quiet --short HEAD 2>/dev/null || true)

# A marker counts only if it names the current branch. Without that check a
# leftover from a finished run silences the reminder forever in a long-lived
# checkout — and the marker's own presence suppresses the only thing that would
# rewrite it, so the staleness never self-corrects. Step 5 also reads this file
# to render the PR's settings table, where a stale one would report a previous
# run's answers as this one's.
marker_ok=""
if [ -n "$branch" ] && [ -f "$marker" ]; then
  recorded=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("branch",""))' \
    "$marker" 2>/dev/null || true)
  [ -n "$recorded" ] && [ "$recorded" = "$branch" ] && marker_ok=yes
fi

read -r -d '' NOTE <<'EOF' || true
This repo has a required workflow for changes. If this turn is a question about
the repository, ignore this. If it changes anything — add, fix, delete,
refactor, docs — invoke the `ship` skill before your first edit (or read
docs/ship.md in full if skills are unavailable). A pull request whose head
commit has no valid stamp turns CI's `ship` check red, and it stays red.
EOF

case "$mode" in
  prompt)
    [ -n "$marker_ok" ] && exit 0
    python3 - "$NOTE" <<'PY'
import json, sys
print(json.dumps({"hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": sys.argv[1]}}))
PY
    ;;
  push)
    if [ -z "$marker_ok" ]; then
      printf '\n⚠  No ship-workflow marker for this branch in this worktree.\n\n%s\n\n' \
        "$NOTE" >&2
      printf 'Pushing anyway. CI has the final say.\n\n' >&2
      exit 0
    fi
    # The workflow is running, so the useful warning is a different one: is the
    # stamp where CI will actually look? A trailer with prose on the line above
    # it parses as no trailer at all, and without this check that is discovered
    # only after `gh pr ready` — costing the five macOS legs the draft
    # convention exists to avoid.
    values=$(git -C "$root" log --format='%(trailers:key=Ship-Stamp,valueonly)' -1 HEAD 2>/dev/null || true)
    ok=""
    while IFS= read -r v; do
      v=${v//[[:space:]]/}
      case "$v" in
        v1*) "$root/scripts/dev/prmeta.sh" verify "$branch" "${v#v1}" && ok=yes && break ;;
      esac
    done <<< "$values"
    if [ -z "$ok" ]; then
      printf '\n⚠  The head commit carries no valid ship stamp for `%s`.\n\n' "$branch" >&2
      printf 'CI reads the stamp from a `Ship-Stamp:` git trailer on the HEAD commit\n' >&2
      printf 'only. A trailer needs a blank line before it and nothing after it, and\n' >&2
      printf 'the value derives from the branch name — so a rename invalidates it.\n' >&2
      printf 'See docs/ship.md Step 5. Pushing anyway.\n\n' >&2
    fi
    ;;
  *) echo "unknown mode: $mode" >&2; exit 1 ;;
esac
