#!/usr/bin/env bash
#
# CI gate: every pull request must carry a stamp proving it came through
# `docs/ship.md`. Run from the `ship` job in `.github/workflows/ci.yml`; see that
# job for the environment it expects. `--selftest` runs the fixtures at the
# bottom and needs no environment.
#
# Four decisions here are load-bearing, and three of them are scar tissue.
#
#   * **The stamp must be on the head commit.** Not "any commit in the PR
#     range" — that made a stamp inherited rather than earned. A squash merge
#     leaves the original stamped commit unreachable from `main`, so continuing
#     on the same branch (or re-creating a shipped branch name) left the *next*
#     PR passing with no workflow run at all, reported as a green tick. Ordinary
#     developer behaviour, no attacker required. The workflow therefore stamps
#     every commit it makes; see its Step 5.
#   * **There is no bootstrap path.** An earlier version fell back to the pull
#     request's own helper when the base commit had none, which let a PR ship a
#     neutered helper and certify itself. Two successive attempts to scope that
#     fallback safely — first on helper readability, then on whether the `ship`
#     job existed at base — were both shown to fail open under an ordinary
#     rename or reindent. It existed for exactly one commit in this repo's
#     history, so it is gone: the PR that introduced this gate carried the
#     maintainer's `no-ship` waiver instead, and `--selftest` running in CI is
#     what demonstrates the logic. A missing helper at base is now a gate
#     malfunction, and it says so rather than blaming the author.
#   * **It does not read any document.** An earlier version recovered the
#     helper's argument by regex-grepping prose, which coupled the gate to one
#     line's wrapping. `verify` needs no argument.
#   * **It distinguishes four outcomes, not two.** "No stamp" and "a stamp that
#     does not match" are different situations with different remedies, and a
#     gate that cannot run is neither. Collapsing them meant a mid-PR branch
#     rename produced "you skipped the workflow" — a false accusation, on a
#     message that tells the reader not to investigate.
set -euo pipefail

HELPER=scripts/dev/prmeta.sh
WORKFLOW=.github/workflows/ci.yml

# Quoted array, expanded quoted. `renovate[bot]` is a valid glob and the gate's
# cwd holds the pull request's own files, so an unquoted `for bot in $BOTS` let a
# fork PR containing a file named `renovateb` rewrite the exemption to
# `renovateb` — and a login of that name was then exempt with no stamp. Both
# halves of that were attacker-supplied. It also silently removed the real bot
# exemption, which is the same bug pointing the other way.
BOTS=('renovate[bot]' 'dependabot[bot]')

# Blames the gate, not the author. Every path that reaches this is a bug here or
# a broken checkout — never something the contributor did wrong.
malfunction() {
  echo "❌ The ship gate could not run: $1" >&2
  echo >&2
  echo "   This is a fault in the gate or the checkout, NOT a problem with your" >&2
  echo "   pull request, and it is not something you can fix by re-stamping." >&2
  echo "   Report it, or read tests/validate-ship-stamp.sh." >&2
  exit 1
}

gate() {
  : "${HEAD:?HEAD (head sha) is required}"
  : "${BRANCH:?BRANCH (head ref) is required}"
  : "${BASE:?BASE (base sha) is required}"
  local actor=${ACTOR:-} labels=${LABELS:-} body=${PR_BODY:-} bot

  for bot in "${BOTS[@]}"; do
    if [ "$actor" = "$bot" ]; then
      echo "✅ $actor is an automation account — ship stamp not required."
      return 0
    fi
  done

  # The maintainer's override. A label needs write access, so it is the one
  # escape an agent cannot take by itself: `SHIP-OVERRIDE` in a PR body is a
  # request, and this label is the answer. Parsed rather than grepped — `toJSON`
  # emits JSON, and a substring match also fired on a label whose own name
  # contained the token.
  if [ -n "$labels" ]; then
    local lstatus=0
    python3 -c 'import json,sys
try: v = json.loads(sys.argv[1] or "[]")
except ValueError: sys.exit(2)
sys.exit(0 if "no-ship" in v else 1)' "$labels" || lstatus=$?
    if [ "$lstatus" -eq 0 ]; then
      echo "✅ 'no-ship' label present — a maintainer waived this PR."
      return 0
    elif [ "$lstatus" -eq 2 ]; then
      malfunction "LABELS was not valid JSON"
    fi
  fi

  git cat-file -e "$BASE:$HELPER" 2>/dev/null \
    || malfunction "$HELPER does not exist at the base commit ($BASE)"

  local work
  work=$(mktemp -d)
  # shellcheck disable=SC2064  # expand now; $work is out of scope at trap time
  trap "rm -rf '$work'" RETURN
  git show "$BASE:$HELPER" > "$work/helper.sh" \
    || malfunction "could not read $HELPER from the base commit"
  chmod +x "$work/helper.sh"

  # Every trailer value on its own line. An earlier version piped them through
  # `tr -d '[:space:]'`, which glued two stamps into `v1AAAAv1BBBB` — reachable
  # by re-stamping, or by squashing two stamped commits — and then rejected it
  # with the generic message.
  local values value seen=0
  values=$(git log --format='%(trailers:key=Ship-Stamp,valueonly)' -1 "$HEAD") \
    || malfunction "could not read the commit message of $HEAD"

  while IFS= read -r value; do
    value=${value//[[:space:]]/}
    [ -n "$value" ] || continue
    seen=$((seen + 1))
    # Only a v1 value is ours. A `v2…` left the prefix intact under `${v#v1}`
    # and failed indistinguishably from a wrong value.
    case "$value" in v1*) ;; *) continue ;; esac
    if "$work/helper.sh" verify "$BRANCH" "${value#v1}"; then
      echo "✅ Ship stamp verified on $HEAD."
      return 0
    fi
  done <<< "$values"

  if [ "$seen" -gt 0 ]; then
    cat >&2 <<EOF

❌ This commit carries a ship stamp, but not one that matches this branch.

Branch: $BRANCH
Commit: $HEAD

You did run the workflow — this is not that accusation. The stamp is derived
from the branch name, so the usual cause is that the branch was renamed after
it was stamped: GitHub keeps the pull request and updates its head ref, and the
commit message does not follow.

Re-stamp the head commit for the current branch name — docs/ship.md, Step 5 —
and push. A wrong version prefix (anything but 'v1') lands here too.

EOF
    return 1
  fi

  cat >&2 <<'EOF'

❌ This PR was not created through the ship workflow.

Its head commit carries no ship stamp.

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
# Same reasoning as the repo's other gates, which all run selftest-first for the
# reason `ci.yml` spells out: a gate that has rotted into "always passes" passes
# the real check trivially, and that reads as a clean bill of health. This
# gate's rot mode is a stamp that verifies when it should not.
#
# Two fixtures are regressions for fail-open bugs this script actually shipped
# with — `inherited stamp` and `glob file` — and two assertions deliberately
# read the REAL repository rather than a fixture, because what made the previous
# bootstrap unsafe was precisely that the only thing checking it was a synthetic
# file the test wrote itself.

REAL_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
STAMP_ARG=sextant-4417
FAILURES=0

check() {
  local label=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then
    printf '  ok    %-44s %s\n' "$label" "$got"
  else
    printf '  FAIL  %-44s want %s, got %s\n' "$label" "$want" "$got"
    FAILURES=$((FAILURES + 1))
  fi
}

new_repo() {  # $1 dir · $2 helper-at-base (yes/no)
  local d=$1
  mkdir -p "$d/scripts/dev" && git -C "$d" init -q -b main
  git -C "$d" config user.email t@example.com
  git -C "$d" config user.name Test
  [ "$2" = yes ] && cp "$REAL_ROOT/$HELPER" "$d/$HELPER"
  echo base > "$d/f.txt"
  git -C "$d" add -A && git -C "$d" commit -qm base
  # Tagged, because the fixtures commit on `main` itself — resolving BASE as
  # `main` would make it the head commit and quietly defeat the "helper absent
  # at base" fixture, which is exactly what it did on the first run.
  git -C "$d" tag base-ref
}

add_commit() {  # $1 dir · $2 trailer block ("" = none)
  local d=$1 msg="feat: a change"
  cp "$REAL_ROOT/$HELPER" "$d/$HELPER"; chmod +x "$d/$HELPER"
  date +%s%N > "$d/f.txt"
  [ -n "$2" ] && msg="$msg

$2"
  git -C "$d" add -A && git -C "$d" commit -qm "$msg"
}

stamp_for() { "$REAL_ROOT/$HELPER" "$STAMP_ARG" stamp "$1"; }

run_gate() {  # $1 dir · $2 branch · $3 actor · $4 labels · $5 body
  if (cd "$1" && BASE=$(git rev-parse base-ref) HEAD=$(git rev-parse HEAD) \
        BRANCH=$2 ACTOR=${3:-someone} LABELS=${4:-[]} PR_BODY=${5:-} \
        gate >/dev/null 2>&1); then echo accept; else echo reject; fi
}

selftest() {
  local tmp d
  tmp=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" EXIT
  echo "ship-stamp selftest:"

  d=$tmp/ok;   new_repo "$d" yes; add_commit "$d" "$(stamp_for feature-x)"
  check "valid stamp on the head commit"        accept "$(run_gate "$d" feature-x)"
  check "same stamp, different branch"          reject "$(run_gate "$d" feature-y)"

  d=$tmp/none; new_repo "$d" yes; add_commit "$d" ""
  check "no stamp at all"                       reject "$(run_gate "$d" feature-x)"

  # REGRESSION: a stamp on an earlier commit must not carry the head commit.
  # This is what let a stamp be inherited across a reused branch.
  d=$tmp/inh;  new_repo "$d" yes
  add_commit "$d" "$(stamp_for feature-x)"; add_commit "$d" ""
  check "inherited stamp (not on head)"         reject "$(run_gate "$d" feature-x)"

  # No bootstrap: a helper missing at base is a malfunction, never a pass.
  d=$tmp/noh;  new_repo "$d" no;  add_commit "$d" "$(stamp_for feature-x)"
  check "helper absent at base — no fallback"   reject "$(run_gate "$d" feature-x)"

  # Two trailers must not be glued together; a valid one among them still wins.
  d=$tmp/two;  new_repo "$d" yes
  add_commit "$d" "$(printf 'Ship-Stamp: v1 %s\n%s' deadbeefdeadbeef "$(stamp_for feature-x)")"
  check "two trailers, one valid"               accept "$(run_gate "$d" feature-x)"

  d=$tmp/v2;   new_repo "$d" yes; add_commit "$d" "Ship-Stamp: v2 deadbeefdeadbeef"
  check "unknown version prefix"                reject "$(run_gate "$d" feature-x)"

  d=$tmp/bot;  new_repo "$d" yes; add_commit "$d" ""
  check "renovate[bot] exempt, no stamp"        accept "$(run_gate "$d" feature-x 'renovate[bot]')"
  check "bare 'renovate' not exempt"            reject "$(run_gate "$d" feature-x 'renovate')"
  check "no-ship label waives"                  accept "$(run_gate "$d" feature-x someone '["no-ship"]')"
  check "label merely containing the token"     reject "$(run_gate "$d" feature-x someone '["not-no-ship"]')"
  check "malformed LABELS is a malfunction"     reject "$(run_gate "$d" feature-x someone 'not json')"
  check "SHIP-OVERRIDE alone does not waive"    reject "$(run_gate "$d" feature-x someone '[]' 'SHIP-OVERRIDE: x')"

  # REGRESSION: `renovate[bot]` is a glob and the gate's cwd holds the PR's own
  # files. A file named `renovateb` used to rewrite the exemption to `renovateb`.
  d=$tmp/glob; new_repo "$d" yes; add_commit "$d" ""
  : > "$d/renovateb"
  git -C "$d" add -A && git -C "$d" commit -qm "chore: a file named like a glob match"
  check "glob file cannot forge an exemption"   reject "$(run_gate "$d" feature-x 'renovateb')"
  check "glob file does not break real bot"     accept "$(run_gate "$d" feature-x 'renovate[bot]')"

  # Non-fixture assertions against the real repository. The previous bootstrap
  # was unsafe precisely because nothing checked the real files.
  if [ -f "$REAL_ROOT/$HELPER" ]; then
    check "real repo still has the helper"      present present
  else
    check "real repo still has the helper"      present missing
  fi
  if grep -q 'validate-ship-stamp.sh' "$REAL_ROOT/$WORKFLOW"; then
    check "real ci.yml still invokes this gate" wired wired
  else
    check "real ci.yml still invokes this gate" wired unwired
  fi

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
