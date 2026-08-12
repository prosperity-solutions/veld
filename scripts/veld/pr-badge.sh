#!/usr/bin/env bash
# The pull-request badge for this repo's top bar (`ide.extensions`).
#
# Prints one JSON object on stdout in veld's badge contract:
#
#   { "text": …, "tone": …, "tooltip": …, "href": …, "actions": [ … ] }
#
# **Nothing about GitHub reaches veld.** This script is the adapter: it knows
# `gh`, and veld knows only the contract. A GitLab project ships the same file
# calling `glab` and gets the same badge — which is the whole reason the
# provider-specific half lives in the repo rather than in veld.
#
# Three parts of the contract this leans on:
#   - `actions` name **declared** extension ids, never commands. veld resolves
#     each one against veld.json before offering it, so this script cannot
#     introduce something to run.
#   - exit 0 with no output means "nothing to show" and the badge is absent.
#   - a non-zero exit renders a failed badge with our last stderr line in its
#     tooltip, so an error is worth writing there rather than swallowing.
#
# veld runs this with stdin closed and no terminal attached, so an
# unauthenticated `gh` fails with its login hint instead of waiting forever.
set -uo pipefail

if ! command -v gh >/dev/null 2>&1; then
  # Unreachable in normal use — `requires_bin: ["gh"]` keeps the badge from
  # running at all without it. Kept so a hand-run explains itself.
  echo "gh (the GitHub CLI) is not installed — see https://cli.github.com" >&2
  exit 1
fi

branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || branch=""
if [ -z "$branch" ] || [ "$branch" = "HEAD" ]; then
  # A detached HEAD has no pull request to have. Not an error, nothing to show.
  exit 0
fi

# One API call for everything the badge shows. stderr is captured rather than
# printed straight through, because "no pull requests found" is an *expected*
# answer that deserves a badge of its own rather than a failure.
if ! payload=$(gh pr view "$branch" \
  --json number,state,isDraft,url,mergeable,statusCheckRollup 2>&1); then
  case "$payload" in
    *"no pull requests found"* | *"no open pull requests"* | *"Could not resolve"*)
      # The interesting empty state: no PR *yet*. The badge says so, and offers
      # the declared action that fixes it.
      printf '{"text":"No PR","tone":"neutral","tooltip":"No pull request for %s yet","actions":[{"id":"create-pr","label":"Create a pull request"}]}\n' \
        "$branch"
      exit 0
      ;;
    *)
      echo "$payload" >&2
      exit 1
      ;;
  esac
fi

# The rest is one transform over the payload already fetched, so the badge costs
# exactly one API call however many fields it renders.
printf '%s' "$payload" | python3 -c '
import json, sys

pr = json.load(sys.stdin)
branch = sys.argv[1]

# A check run reports `conclusion`; an older commit status reports `state`.
def outcome(check):
    return (check.get("conclusion") or check.get("state") or "").upper()

results = [outcome(c) for c in (pr.get("statusCheckRollup") or [])]
BAD = {"FAILURE", "ERROR", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED"}
WAITING = {"", "PENDING", "IN_PROGRESS", "QUEUED", "WAITING", "EXPECTED", "REQUESTED"}
if not results:
    checks = "none"
elif any(r in BAD for r in results):
    checks = "failing"
elif any(r in WAITING for r in results):
    checks = "pending"
else:
    checks = "passing"

state = pr.get("state", "")
tone, detail = "info", state.lower()
if state == "MERGED":
    tone, detail = "success", "merged"
elif state == "CLOSED":
    tone, detail = "neutral", "closed"
elif state == "OPEN":
    if pr.get("isDraft"):
        tone, detail = "neutral", "draft"
    elif checks == "failing":
        tone, detail = "danger", "checks failing"
    elif pr.get("mergeable") == "CONFLICTING":
        # Its own colour, because a conflicting PR stops `pull_request` events
        # firing at all — so its checks never report and it reads as unrun.
        tone, detail = "warning", "conflicting"
    elif checks == "pending":
        tone, detail = "warning", "checks running"
    elif checks == "passing":
        tone, detail = "success", "checks green"
    else:
        tone, detail = "info", "open"

badge = {
    "text": "PR #{} · {}".format(pr.get("number", "?"), detail),
    "tone": tone,
    "tooltip": "{}: {}. Click to open it in your browser.".format(branch, detail),
}
url = pr.get("url")
if url:
    badge["href"] = url
print(json.dumps(badge))
' "$branch"
