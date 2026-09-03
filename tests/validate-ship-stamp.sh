#!/usr/bin/env bash
#
# CI gate: every pull request must carry a stamp proving it came through
# `docs/ship.md`. Run from the `ship` job in `.github/workflows/ci.yml`; see
# that job for the environment it expects. `--selftest` runs the fixtures at the
# bottom and needs no environment.
#
# Three things this deliberately does NOT do:
#
#   * It does not read the helper out of the *pull request*. The helper comes
#     from the PR's base commit, so a branch cannot neuter it and then certify
#     itself. The one exception is the commit that introduces this gate, whose
#     base predates it — and "base predates the gate" is decided by whether the
#     `ship` job existed at base, NOT by whether the helper file is readable.
#     Those two are not the same question: the file is also unreadable at base
#     the moment somebody *moves* it on main, and treating that as a bootstrap
#     would silently hand every later PR a self-certifying fallback. Fail
#     closed there. `selftest_fail_closed_on_moved_helper` is that regression.
#   * It does not read the workflow document. An earlier version recovered the
#     helper's argument by regex-grepping prose in SKILL.md, which coupled the
#     gate to that file's exact line wrapping; `verify` needs no argument.
#   * It does not tell a failing caller how to produce a stamp. The instruction
#     is to read the workflow. Printing the command would let an agent satisfy
#     the gate without reading the document the gate exists to enforce.
set -euo pipefail

HELPER=scripts/dev/prmeta.sh
WORKFLOW=.github/workflows/ci.yml
GATE_JOB='^  ship:'

# Only the bracketed forms are real automation accounts. A bare `renovate` or
# `dependabot` is a username a person can register, and exempting it would hand
# anyone a bypass for the price of a signup.
BOTS='renovate[bot] dependabot[bot] github-actions[bot]'

gate() {
  : "${BASE:?BASE (base sha) is required}"
  : "${HEAD:?HEAD (head sha) is required}"
  : "${BRANCH:?BRANCH (head ref) is required}"
  local actor=${ACTOR:-} labels=${LABELS:-} body=${PR_BODY:-}

  local bot
  for bot in $BOTS; do
    if [ "$actor" = "$bot" ]; then
      echo "✅ $actor is an automation account — ship stamp not required."
      return 0
    fi
  done

  # The maintainer's override. A label needs write access on the repository, so
  # it is the one escape an agent cannot take by itself — which is the point:
  # `SHIP-OVERRIDE` in a PR body is a request, and this label is the answer.
  # Parsed, not grepped: `toJSON` output is JSON, and a substring match would
  # also fire on a label whose own name happened to contain the token.
  if [ -n "$labels" ] && python3 -c 'import json,sys
sys.exit(0 if "no-ship" in json.loads(sys.argv[1] or "[]") else 1)' "$labels"; then
    echo "✅ 'no-ship' label present — a maintainer waived this PR."
    return 0
  fi

  local work
  work=$(mktemp -d)
  # shellcheck disable=SC2064  # expand $work now, not at trap time
  trap "rm -rf '$work'" RETURN

  local bootstrap=""
  if git show "$BASE:$WORKFLOW" 2>/dev/null | grep -qE "$GATE_JOB"; then
    # The gate existed at base, so the helper must be there too. If it is not,
    # something moved or deleted it and the honest answer is to fail, loudly.
    if ! git cat-file -e "$BASE:$HELPER" 2>/dev/null; then
      echo "❌ The ship gate exists at the base commit but $HELPER does not." >&2
      echo "   The helper was moved or removed without updating this gate." >&2
      echo "   Failing closed: a missing helper must never mean 'everything passes'." >&2
      return 1
    fi
    git show "$BASE:$HELPER" > "$work/helper.sh"
  else
    bootstrap=yes
    cp "$HELPER" "$work/helper.sh"
    echo "ℹ️  The ship gate does not exist at the base commit, so this is the pull"
    echo "    request that introduces it. Verifying against its own helper."
  fi
  chmod +x "$work/helper.sh"

  local sha value matched=""
  while IFS= read -r sha; do
    [ -n "$sha" ] || continue
    value=$(git log --format='%(trailers:key=Ship-Stamp,valueonly)' -1 "$sha" | tr -d '[:space:]')
    [ -n "$value" ] || continue
    value=${value#v1}
    if "$work/helper.sh" verify "$BRANCH" "$value"; then
      matched=$sha
      break
    fi
  done < <(git rev-list "$BASE".."$HEAD")

  if [ -n "$matched" ]; then
    echo "✅ Ship stamp verified on $matched."
    [ -n "$bootstrap" ] && echo "   (bootstrap: verified against the pull request's own helper.)"
    return 0
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

  if printf '%s' "$body" | grep -q 'SHIP-OVERRIDE'; then
    cat >&2 <<'EOF'
This PR body contains SHIP-OVERRIDE, so somebody already chose to skip the
workflow knowingly. That is a request, not a decision: a maintainer can honour
it with the 'no-ship' label, which needs write access on this repository.
Until then this check stays red.

EOF
  fi

  return 1
}

# ── Selftest ──────────────────────────────────────────────────────────
#
# Same reasoning as the repo's other gates (`validate-workflow-gates.py`,
# `validate-release-publish.py`, `signing-slots.py`), which all run
# selftest-first for the reason spelled out in `ci.yml`: a gate that has rotted
# into "always passes" passes the real check trivially, and that reads as a
# clean bill of health. This gate's rot mode is `matched` always ending up set.
# So the fixtures assert both directions, and one of them — the moved helper —
# is a regression test for a fail-open bug this script actually shipped with.

REAL_HELPER=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/dev/prmeta.sh
STAMP_ARG=sextant-4417
FAILURES=0

# $1 dir · $2 gate-job-at-base (yes/no) · $3 helper-at-base (yes/no)
new_repo() {
  local d=$1 gate_at_base=$2 helper_at_base=$3
  mkdir -p "$d" && git -C "$d" init -q -b main
  git -C "$d" config user.email t@example.com
  git -C "$d" config user.name Test
  mkdir -p "$d/.github/workflows" "$d/scripts/dev"
  if [ "$gate_at_base" = yes ]; then
    printf 'jobs:\n  ship:\n    runs-on: ubuntu-latest\n' > "$d/$WORKFLOW"
  else
    printf 'jobs:\n  other:\n    runs-on: ubuntu-latest\n' > "$d/$WORKFLOW"
  fi
  [ "$helper_at_base" = yes ] && cp "$REAL_HELPER" "$d/$HELPER"
  git -C "$d" add -A && git -C "$d" commit -qm "base"
}

# $1 dir · $2 branch to stamp for ("" = no stamp)
add_commit() {
  local d=$1 stamp_branch=$2 msg
  cp "$REAL_HELPER" "$d/$HELPER"
  chmod +x "$d/$HELPER"
  date > "$d/change.txt"
  msg="feat: a change"
  if [ -n "$stamp_branch" ]; then
    msg="$msg

$("$REAL_HELPER" "$STAMP_ARG" stamp "$stamp_branch")"
  fi
  git -C "$d" add -A && git -C "$d" commit -qm "$msg"
}

expect() {
  local label=$1 want=$2 d=$3 branch=$4 got
  shift 4
  if (cd "$d" && BASE=$(git rev-parse HEAD~1) HEAD=$(git rev-parse HEAD) \
        BRANCH=$branch ACTOR=${1:-someone} LABELS=${2:-[]} PR_BODY=${3:-} \
        gate >/dev/null 2>&1); then
    got=accept
  else
    got=reject
  fi
  if [ "$got" = "$want" ]; then
    printf '  ok    %-46s %s\n' "$label" "$got"
  else
    printf '  FAIL  %-46s want %s, got %s\n' "$label" "$want" "$got"
    FAILURES=$((FAILURES + 1))
  fi
}

selftest() {
  local tmp
  tmp=$(mktemp -d)
  # Expand $tmp now, not at trap time: it is `local`, so it is out of scope by
  # the time an EXIT trap fires and `set -u` turns the cleanup into an error.
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" EXIT
  echo "ship-stamp selftest:"

  new_repo "$tmp/good" yes yes;  add_commit "$tmp/good" feature-x
  expect "valid stamp, gate and helper at base" accept "$tmp/good" feature-x

  new_repo "$tmp/wrong" yes yes; add_commit "$tmp/wrong" some-other-branch
  expect "stamp derived from a different branch" reject "$tmp/wrong" feature-x

  new_repo "$tmp/none" yes yes;  add_commit "$tmp/none" ""
  expect "no stamp at all" reject "$tmp/none" feature-x

  # The commit that introduces the gate: no `ship` job at base, so the helper
  # legitimately is not there either.
  new_repo "$tmp/boot" no no;    add_commit "$tmp/boot" feature-x
  expect "bootstrap — gate absent at base" accept "$tmp/boot" feature-x

  # The regression. Gate present at base, helper missing at base: somebody moved
  # it on main. Must NOT fall back to the PR's own copy.
  new_repo "$tmp/moved" yes no;  add_commit "$tmp/moved" feature-x
  expect "moved helper — must fail closed" reject "$tmp/moved" feature-x

  # A stamp is irrelevant for an exempt actor, and a wrong one must not save a
  # non-exempt one.
  new_repo "$tmp/bot" yes yes;   add_commit "$tmp/bot" ""
  expect "renovate[bot] exempt without a stamp" accept "$tmp/bot" feature-x 'renovate[bot]'
  expect "bare 'renovate' is NOT exempt"        reject "$tmp/bot" feature-x 'renovate'
  expect "no-ship label waives"                 accept "$tmp/bot" feature-x 'someone' '["no-ship"]'
  expect "label containing the token does not"  reject "$tmp/bot" feature-x 'someone' '["not-no-ship-really"]'
  expect "SHIP-OVERRIDE alone does not waive"   reject "$tmp/bot" feature-x 'someone' '[]' 'SHIP-OVERRIDE: no time'

  if [ "$FAILURES" -ne 0 ]; then
    echo "ship-stamp selftest: $FAILURES failure(s)" >&2
    exit 1
  fi
  echo "ship-stamp selftest: all fixtures behave"
}

case "${1:-}" in
  --selftest) selftest ;;
  *)          gate ;;
esac
