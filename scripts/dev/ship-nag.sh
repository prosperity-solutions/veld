#!/usr/bin/env bash
#
# Reminds an agent that this repo has a required workflow, at two moments: when
# a prompt arrives, and on push.
#
# It never blocks, and it deliberately does not touch Claude Code's permission
# decisions. An earlier version answered the `PreToolUse` hook with
# `permissionDecision: "allow"` on the theory that it "allows the call and
# attaches a note" — it does not. It bypasses the user's Edit/Write
# confirmation, and because the hook only fires when the marker is *absent* it
# would have auto-approved edits for exactly the people who had not opted into
# the workflow. `UserPromptSubmit`'s `additionalContext` is the only mode that
# actually puts text in front of the model, so that is the only one used here.
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

# A marker is only this run's if it names this branch. Without that check a
# leftover from a finished run silences the reminder forever in any long-lived
# checkout — and the marker's own presence is what suppresses the only thing
# that would rewrite it, so the staleness never self-corrects. Step 5 also reads
# this file back to render the PR's settings table, where a stale one would
# report a previous run's answers as "this run".
if [ -f "$marker" ]; then
  recorded=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("branch",""))' \
    "$marker" 2>/dev/null || true)
  current=$(git -C "$root" rev-parse --abbrev-ref HEAD 2>/dev/null || true)
  if [ -n "$recorded" ] && [ "$recorded" = "$current" ]; then
    exit 0
  fi
fi

read -r -d '' NOTE <<'EOF' || true
This repo has a required workflow for changes. If this turn is a question about
the repository, ignore this. If it changes anything — add, fix, delete,
refactor, docs — invoke the `ship` skill before your first edit (or read
docs/ship.md in full if skills are unavailable). A pull request without that
workflow's stamp fails CI's required `ship` check and cannot be merged.
EOF

case "$mode" in
  prompt)
    python3 - "$NOTE" <<'PY'
import json, sys
print(json.dumps({"hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": sys.argv[1]}}))
PY
    ;;
  push)
    printf '\n⚠  No ship-workflow marker for this branch in this worktree.\n\n%s\n\n' "$NOTE" >&2
    printf 'Pushing anyway. CI has the final say.\n\n' >&2
    ;;
  *) echo "unknown mode: $mode" >&2; exit 1 ;;
esac
