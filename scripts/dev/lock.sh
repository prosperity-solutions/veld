#!/usr/bin/env bash
#
# The maintainer's lock: one switch that stops outside work on this repository.
#
#   scripts/dev/lock.sh setup           # once: team, ruleset (off), label
#   scripts/dev/lock.sh on "<reason>"   # lock, and tell everyone why
#   scripts/dev/lock.sh off             # unlock
#   scripts/dev/lock.sh status          # exit 1 when locked and you are not an owner
#
# The switch is the repository ruleset named `Lock`: on means its enforcement is
# `active`. While it is, only the members of the bypass team can update `main` —
# nobody else can merge, org owners included, until somebody turns it off. That
# turning-off is the one way around it, and it lands in the org's audit log.
#
# The ruleset is the switch, not a topic or an Actions variable, because it is
# both the wall and readable by anyone: GitHub serves a public repo's rulesets
# without credentials. So an agent on a fresh clone, a contributor whose token
# has no Actions scope, and CI all see the same answer, and the answer cannot
# disagree with what GitHub actually enforces.
#
# The reason is a pinned issue labelled `veld-lock`, opened by an owner. Humans
# see it on the repo page; the hook and CI quote it.
#
# While locked:
#   * `hook` — Claude Code's UserPromptSubmit hook refuses every prompt in this
#     repo for anyone not in OWNERS (.claude/settings.json).
#   * `ci`   — the `Lock` check (.github/workflows/lock.yml) fails a pull request
#     whose author or pusher is not in OWNERS.
#   * Release stands down: the ruleset would reject semantic-release's push, so
#     merges made under the lock are released by the first push after it lifts.
set -euo pipefail

REPO=prosperity-solutions/veld
RULESET=Lock
LABEL=veld-lock
TEAM=veld-lock-owners
# Who may work while locked. Changing this list takes a merge, which the lock
# itself prevents. Keep it in step with the members of TEAM.
OWNERS=(peter-adam-dy)

mode=${1:-status}

is_owner() {
  local who=$1 o
  for o in "${OWNERS[@]}"; do
    [ "$who" = "$o" ] && return 0
  done
  return 1
}

# GET on a public endpoint: gh when it is there and authenticated (higher rate
# limit), else anonymous curl. Prints nothing on failure.
api_get() {
  local out=""
  if command -v gh >/dev/null 2>&1; then
    out=$(gh api "$1" 2>/dev/null) || out=""
  fi
  if [ -z "$out" ] && command -v curl >/dev/null 2>&1; then
    out=$(curl -fsS --max-time 5 -H 'Accept: application/vnd.github+json' \
      "https://api.github.com/$1" 2>/dev/null) || out=""
  fi
  printf '%s' "$out"
}

# Prints `locked`, `unlocked` or `unknown` on the first line and, when locked,
# the reason on the rest. "unknown" is a fact in its own right: the caller
# decides what an unreadable switch means, not this function.
lock_state() {
  local rulesets state
  rulesets=$(api_get "repos/$REPO/rulesets")
  state=$(python3 -c '
import json, sys
try:
    rs = json.loads(sys.argv[1])
    active = any(r["name"] == sys.argv[2] and r["enforcement"] == "active" for r in rs)
except Exception:
    print("unknown"); sys.exit()
print("locked" if active else "unlocked")
' "$rulesets" "$RULESET")
  echo "$state"
  [ "$state" = locked ] || return 0
  python3 -c '
import json, sys
reason = "(no reason given)"
try:
    for i in json.loads(sys.argv[1]):
        # A label needs only triage access, so a labelled issue proves
        # nothing by itself: only an owner-authored one speaks for the lock.
        if i["user"]["login"] in sys.argv[2:]:
            reason = ((i.get("body") or "").strip() or i["title"]) + "\n" + i["html_url"]
            break
except Exception:
    pass
print(reason)
' "$(api_get "repos/$REPO/issues?labels=$LABEL&state=open")" "${OWNERS[@]}"
}

github_login() {
  command -v gh >/dev/null 2>&1 || return 0
  gh api user --jq .login 2>/dev/null || true
}

locked_msg() {
  printf 'veld is locked by its maintainer. Do not start work on this repository —\n'
  printf 'no edits, no branches, no pull requests — until it is unlocked.\n\n'
  printf 'Reason: %s\n' "$1"
}

ruleset_id() {
  gh api "repos/$REPO/rulesets" --jq ".[] | select(.name == \"$RULESET\") | .id"
}

case "$mode" in
  setup)
    org=${REPO%%/*}
    if ! gh api "orgs/$org/teams/$TEAM" >/dev/null 2>&1; then
      gh api "orgs/$org/teams" -f name="$TEAM" -f privacy=closed \
        -f description="May merge to veld while it is locked (scripts/dev/lock.sh)" >/dev/null
      echo "created team $TEAM"
    fi
    for o in "${OWNERS[@]}"; do
      gh api -X PUT "orgs/$org/teams/$TEAM/memberships/$o" -f role=maintainer >/dev/null
    done
    # A team must have access to the repo to sit on a ruleset's bypass list.
    gh api -X PUT "orgs/$org/teams/$TEAM/repos/$REPO" -f permission=push >/dev/null
    gh label create "$LABEL" --repo "$REPO" --color B60205 \
      --description "Why veld is locked (scripts/dev/lock.sh)" 2>/dev/null || true
    if [ -z "$(ruleset_id)" ]; then
      team_id=$(gh api "orgs/$org/teams/$TEAM" --jq .id)
      gh api -X POST "repos/$REPO/rulesets" --input - >/dev/null <<JSON
{
  "name": "$RULESET",
  "target": "branch",
  "enforcement": "disabled",
  "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
  "rules": [{"type": "update"}, {"type": "deletion"}, {"type": "non_fast_forward"}],
  "bypass_actors": [{"actor_id": $team_id, "actor_type": "Team", "bypass_mode": "always"}]
}
JSON
      echo "created ruleset $RULESET (off)"
    fi
    echo "✅ setup done — lock with: scripts/dev/lock.sh on \"<reason>\""
    ;;
  on)
    reason=${2:?usage: lock.sh on \"<reason>\"}
    id=$(ruleset_id)
    [ -n "$id" ] || { echo "no $RULESET ruleset — run: scripts/dev/lock.sh setup" >&2; exit 1; }
    issue=$(gh issue list --repo "$REPO" --label "$LABEL" --state open --json number --jq '.[0].number')
    if [ -n "$issue" ]; then
      gh issue edit "$issue" --repo "$REPO" --body "$reason" >/dev/null
    else
      url=$(gh issue create --repo "$REPO" --label "$LABEL" \
        --title "🔒 veld is locked" --body "$reason")
      issue=${url##*/}
    fi
    gh issue pin "$issue" --repo "$REPO" >/dev/null 2>&1 || true
    gh api -X PUT "repos/$REPO/rulesets/$id" -f enforcement=active >/dev/null
    echo "🔒 $REPO is locked — #$issue"
    ;;
  off)
    id=$(ruleset_id)
    [ -n "$id" ] && gh api -X PUT "repos/$REPO/rulesets/$id" -f enforcement=disabled >/dev/null
    for issue in $(gh issue list --repo "$REPO" --label "$LABEL" --state open --json number --jq '.[].number'); do
      gh issue unpin "$issue" --repo "$REPO" >/dev/null 2>&1 || true
      gh issue close "$issue" --repo "$REPO" --comment "🔓 Unlocked." >/dev/null
    done
    echo "🔓 $REPO is unlocked."
    ;;
  status)
    out=$(lock_state)
    state=${out%%$'\n'*}
    case "$state" in
      unlocked) echo "🔓 unlocked" ;;
      unknown)
        echo "❔ could not read the lock (ruleset $RULESET on $REPO) — treat it as locked" >&2
        exit 1 ;;
      locked)
        reason=${out#*$'\n'}
        login=$(github_login)
        if [ -n "$login" ] && is_owner "$login"; then
          echo "🔒 locked — you ($login) are an owner, so you may work"
          echo "Reason: $reason"
        else
          echo "🔒 locked — ${login:-unknown GitHub user} is not an owner" >&2
          locked_msg "$reason" >&2
          exit 1
        fi ;;
    esac
    ;;
  hook)
    # UserPromptSubmit runs this on every prompt, so both lookups are cached:
    # the lock for a minute (how long a flip takes to reach a running agent),
    # the login for a day.
    cache=${TMPDIR:-/tmp}/veld-lock-$(id -u)
    mkdir -p "$cache"
    fresh() { [ -s "$1" ] && [ -z "$(find "$1" -mmin +"$2" 2>/dev/null)" ]; }
    fresh "$cache/state" 1 || lock_state > "$cache/state"
    out=$(cat "$cache/state")
    state=${out%%$'\n'*}
    [ "$state" = unlocked ] && exit 0
    fresh "$cache/login" 1440 || github_login > "$cache/login"
    login=$(cat "$cache/login")
    if [ "$state" = locked ]; then
      is_owner "$login" && exit 0
      python3 - "$(locked_msg "${out#*$'\n'}")" <<'PY'
import json, sys
print(json.dumps({"decision": "block", "reason": sys.argv[1]}))
PY
    else
      # Unreadable (offline, rate-limited). Blocking here would lock the owner
      # out of their own repo on a plane; GitHub's ruleset is the actual wall,
      # and CI fails closed.
      python3 - <<'PY'
import json
print(json.dumps({"hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": "Could not check whether veld is locked. Before changing "
    "anything, run scripts/dev/lock.sh status; if it exits non-zero, stop."}}))
PY
    fi
    ;;
  ci)
    # ACTORS: every login this run acts for — the PR author and whoever pushed.
    # Fails closed: an unreadable lock is a red check.
    : "${ACTORS:?ACTORS is required}"
    out=$(lock_state)
    state=${out%%$'\n'*}
    case "$state" in
      unlocked) echo "🔓 unlocked"; exit 0 ;;
      unknown) echo "::error::could not read the $RULESET ruleset on $REPO"; exit 1 ;;
    esac
    bad=""
    for a in $ACTORS; do
      is_owner "$a" || bad="$bad $a"
    done
    if [ -n "$bad" ]; then
      echo "::error::veld is locked by its maintainer; not an owner:$bad"
      locked_msg "${out#*$'\n'}"
      exit 1
    fi
    echo "🔒 locked — every actor is an owner ($ACTORS)"
    ;;
  locked)
    # For Release: exit 0 when locked, 1 when not. Unknown counts as unlocked
    # here, because the ruleset — not this answer — is what stops the push.
    out=$(lock_state)
    [ "${out%%$'\n'*}" = locked ]
    ;;
  *) echo "usage: lock.sh setup|on <reason>|off|status|hook|ci|locked" >&2; exit 2 ;;
esac
