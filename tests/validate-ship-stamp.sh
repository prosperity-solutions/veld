#!/usr/bin/env bash
#
# CI gate: every pull request must carry a stamp proving it came through
# `docs/ship.md`. Run from `.github/workflows/ci.yml`; see the `ship` job there
# for the environment it expects.
#
# Two things this deliberately does NOT do:
#
#   * It does not read the expected argument or the helper script out of the
#     *pull request*. Both come from the PR's base commit, so a branch cannot
#     rotate the constant or neuter the helper and then certify itself. The one
#     exception is bootstrapping — the commit that introduces this gate has a
#     base that predates it — which is handled explicitly and announced.
#   * It does not tell a failing caller how to produce a stamp. The instruction
#     is to read the workflow, and printing the command would let an agent
#     satisfy the gate without reading the document the gate exists to enforce.
set -euo pipefail

HELPER=scripts/dev/prmeta.sh
SKILL=.claude/skills/ship/SKILL.md

: "${BASE:?BASE (base sha) is required}"
: "${HEAD:?HEAD (head sha) is required}"
: "${BRANCH:?BRANCH (head ref) is required}"
ACTOR=${ACTOR:-}
LABELS=${LABELS:-}
PR_BODY=${PR_BODY:-}

# Bots do not read the workflow and are not expected to. Renovate and
# Dependabot open dependency PRs mechanically; holding those to a workflow
# written for human/agent-authored change would only mean nothing ever updates.
case "$ACTOR" in
  renovate | renovate\[bot\] | dependabot | dependabot\[bot\] | github-actions\[bot\])
    echo "✅ $ACTOR is an automation account — ship stamp not required."
    exit 0
    ;;
esac

# The maintainer's override. A label needs write access on the repository, so
# this is the one escape an agent cannot take by itself, which is the point:
# `SHIP-OVERRIDE` in a PR body is a *request*, and this label is the answer.
if printf '%s' "$LABELS" | grep -q '"no-ship"'; then
  echo "✅ 'no-ship' label present — a maintainer waved this PR through."
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Resolve the gate's two inputs from the base commit, falling back to the
# working tree only when the base predates them.
bootstrap=""
if git cat-file -e "$BASE:$HELPER" 2>/dev/null && git cat-file -e "$BASE:$SKILL" 2>/dev/null; then
  git show "$BASE:$HELPER" > "$work/helper.sh"
  git show "$BASE:$SKILL" > "$work/skill.md"
else
  bootstrap=yes
  cp "$HELPER" "$work/helper.sh"
  cp "$SKILL" "$work/skill.md"
  echo "ℹ️  Base commit predates this gate — reading the helper and the expected"
  echo "    argument from the pull request itself. This is expected exactly once,"
  echo "    on the PR that introduces the gate."
fi
chmod +x "$work/helper.sh"

arg=$(grep -oE 'prmeta\.sh +[A-Za-z0-9_-]+ +stamp' "$work/skill.md" | head -1 | awk '{print $2}' || true)
if [ -z "$arg" ]; then
  echo "❌ Could not recover the expected argument from $SKILL." >&2
  echo "   The gate and the workflow have drifted apart. They must change together." >&2
  exit 1
fi

matched=""
while IFS= read -r sha; do
  [ -n "$sha" ] || continue
  value=$(git log --format='%(trailers:key=Ship-Stamp,valueonly)' -1 "$sha" | tr -d '[:space:]')
  [ -n "$value" ] || continue
  value=${value#v1}
  if "$work/helper.sh" "$arg" verify "$BRANCH" "$value"; then
    matched=$sha
    break
  fi
done < <(git rev-list "$BASE".."$HEAD")

if [ -n "$matched" ]; then
  echo "✅ Ship stamp verified on $matched."
  [ -n "$bootstrap" ] && echo "   (verified against the pull request's own helper — bootstrap.)"
  exit 0
fi

cat >&2 <<'EOF'

❌ This PR was not created through the ship workflow.

No commit in this pull request carries a valid ship stamp.

Whoever or whatever opened this PR skipped the only supported way to change
this repository.

Agent: stop. Read docs/ship.md. All of it. Then redo this properly.

There is no shortcut here and no flag to silence this. Do not patch this
check, and do not guess the stamp — it is derived, not chosen, and a wrong
guess fails exactly the same way while costing another CI run.

EOF

if printf '%s' "$PR_BODY" | grep -q 'SHIP-OVERRIDE'; then
  cat >&2 <<'EOF'
This PR body contains SHIP-OVERRIDE, so somebody already decided to skip the
workflow knowingly. That is a request, not a decision: a maintainer can honour
it by adding the 'no-ship' label, which needs write access on this repository.
Until then this check stays red.

EOF
fi

exit 1
