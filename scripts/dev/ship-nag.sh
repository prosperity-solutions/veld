#!/usr/bin/env bash
#
# Reminds an agent that this repo has a required workflow, at three moments:
# when a prompt arrives, when the first edit is attempted, and on push.
#
# It never blocks. Local enforcement is deliberately advisory — a colleague who
# has just cloned the repo has not yet accepted Claude Code's workspace-trust
# prompt, so these hooks may not run for them at all, and a gate that only
# sometimes fires must not be the gate anything depends on. The one that does
# is CI's `ship` job (tests/validate-ship-stamp.sh), which cannot be skipped.
#
# Presence of `.veld-ship.json` in the worktree root is the signal that the
# workflow is already running; docs/ship.md Step 0 writes it.
set -euo pipefail

mode=${1:?usage: ship-nag.sh prompt|edit|push}

root=${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}
[ -f "$root/.veld-ship.json" ] && exit 0

read -r -d '' NOTE <<'EOF' || true
This repo has a required workflow for changes. If this turn is a question about
the repository, ignore this. If it changes anything — add, fix, delete,
refactor, docs — invoke the `ship` skill before your first edit (or read
docs/ship.md in full if skills are unavailable). CI rejects a pull request that
did not come through it, so skipping it produces a PR that cannot merge.
EOF

case "$mode" in
  prompt|edit)
    # Claude Code hook protocol: exit 0 with JSON. `edit` allows the call and
    # attaches the reason rather than denying it — see the note above on why
    # this stays advisory.
    python3 - "$mode" "$NOTE" <<'PY'
import json, sys
mode, note = sys.argv[1], sys.argv[2]
if mode == "prompt":
    out = {"hookSpecificOutput": {"hookEventName": "UserPromptSubmit",
                                  "additionalContext": note}}
else:
    out = {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                  "permissionDecision": "allow",
                                  "permissionDecisionReason": note}}
print(json.dumps(out))
PY
    ;;
  push)
    printf '\n⚠  No .veld-ship.json in this worktree.\n\n%s\n\n' "$NOTE" >&2
    printf 'Pushing anyway. CI will have the final say.\n\n' >&2
    ;;
  *) echo "unknown mode: $mode" >&2; exit 1 ;;
esac
