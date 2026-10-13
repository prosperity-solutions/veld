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

for tool in gh python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    # Unreachable in normal use — `requires_bin` in veld.json keeps the badge from
    # running at all without these. Kept so a hand-run explains itself, and
    # because macOS ships a `python3` *stub* that is on PATH and fails when run:
    # `requires_bin` can only ask whether a name resolves.
    echo "$tool is not installed — the PR badge needs gh and python3" >&2
    exit 1
  fi
done

# `git rev-parse`, not `${veld.branch}`: veld slugifies that one (a branch name is
# chosen by whoever opened the PR you checked out), so `feat/foo` would arrive as
# `feat-foo` and gh would report no pull request for it.
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || branch=""
if [ -z "$branch" ] || [ "$branch" = "HEAD" ]; then
  # A detached HEAD has no pull request to have. Not an error, nothing to show.
  exit 0
fi

# One API call for everything the badge shows.
#
# **stdout and stderr are kept apart.** An earlier version used `2>&1` to capture
# gh's diagnostics for the empty-state check below, which also merged any warning
# gh writes on a *successful* call into the JSON — and then `json.load` threw and
# the badge's tooltip was a Python traceback. So stderr goes to a file and is only
# read on failure.
# An explicit template, not `mktemp -t`: BSD's `-t` takes a *prefix* and GNU's takes
# a *template* needing at least three trailing X's, so `mktemp -t veld-pr-badge`
# works on macOS and exits 1 on Linux ("too few X's") — which would leave $errfile
# empty, fail the redirection below before gh ever ran, and make this badge
# permanently red on every Linux install.
errfile=$(mktemp "${TMPDIR:-/tmp}/veld-pr-badge.XXXXXX")
trap 'rm -f "$errfile"' EXIT
if ! payload=$(gh pr view "$branch" \
  --json number,state,isDraft,url,mergeable,statusCheckRollup 2>"$errfile"); then
  payload=$(cat "$errfile")
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
import json, re, sys

pr = json.load(sys.stdin)
branch = sys.argv[1]

# A check run reports `conclusion`; an older commit status reports `state`.
def outcome(check):
    return (check.get("conclusion") or check.get("state") or "").upper()

# **The rollup is every check on the head commit, including superseded runs.**
# When a newer workflow run replaces an older one on the same commit (marking a
# draft ready, a concurrency group cancelling in progress), the jobs of the old
# run stay in the rollup as CANCELLED — and an `if: always()` gate job in it as
# FAILURE. Folding them in painted a PR red whose current run was fine.
#
# So only the newest run of each workflow counts. Run ids are monotonic, which
# is what makes the largest one the newest. This assumes every run of one
# workflow on a pull request head is the same kind of run, which holds when
# `push` is limited to the trunk, and that no two workflow files share a
# `name:` — either way, a run that was not superseded would be read as if it were.
def run_id(check):
    m = re.search(r"/actions/runs/(\d+)", check.get("detailsUrl") or "")
    return int(m.group(1)) if m else None

checks_in = pr.get("statusCheckRollup") or []
# A check with no workflow name is its own workflow: lumping those together
# would let one unrelated run supersede another.
def workflow(c):
    return c.get("workflowName") or "run {}".format(run_id(c))

newest_run = {}
for c in checks_in:
    rid = run_id(c)
    if rid is not None:
        newest_run[workflow(c)] = max(rid, newest_run.get(workflow(c), rid))

def is_newest(c):
    return run_id(c) == newest_run[workflow(c)]

# Every job of the newest run counts, same-named ones included: the rollup
# already holds only the latest attempt of a re-run job. A job the newest run
# skipped counts as skipped even if an older run failed it — the same call
# GitHub makes for a required check.
current = [c for c in checks_in if run_id(c) is not None and is_newest(c)]

# A job the old run reached and the new one has not yet is still to come — a
# gate that `needs:` everything only appears once the rest has finished. It
# reads as pending, not as absent, or the badge goes green before the gate has
# had its say. Only while the new run is still going, though: a job can also
# never reappear (renamed, or a matrix that expanded differently), and once the
# new run has finished a missing name is that, not a job still to come. Two
# kinds are skipped outright: a job the old run skipped (a draft run skips
# everything), and a matrix job skipped before expansion, whose name is the
# literal `${{ matrix.… }}` template.
running = {workflow(c) for c in current if (c.get("status") or "COMPLETED") != "COMPLETED"}
present = {(workflow(c), c.get("name")) for c in current}
results = [outcome(c) for c in current]
for c in checks_in:
    if run_id(c) is None or is_newest(c) or workflow(c) not in running:
        continue
    name = c.get("name") or ""
    if (workflow(c), name) in present:
        continue
    if outcome(c) == "SKIPPED" or "${{" in name:
        continue
    present.add((workflow(c), name))
    results.append("PENDING")

# Anything without a workflow run (a third-party check, a commit status) is
# deduplicated by name, keeping the newest: a check run still going beats any
# that finished, then the one that started last. (A commit status needs none of
# this — the rollup already holds one per context.)
latest = {}
for c in checks_in:
    if run_id(c) is not None:
        continue
    key = ("context", c["context"]) if c.get("context") else ("name", c.get("name"))
    rank = ((c.get("status") or "COMPLETED") != "COMPLETED", c.get("startedAt") or "")
    if key not in latest or rank >= latest[key][0]:
        latest[key] = (rank, c)
results += [outcome(c) for _, c in latest.values()]
BAD = {"FAILURE", "ERROR", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE"}
WAITING = {"", "PENDING", "IN_PROGRESS", "QUEUED", "WAITING", "EXPECTED", "REQUESTED"}
if not results:
    checks = "none"
elif any(r in BAD for r in results):
    checks = "failing"
elif any(r in WAITING for r in results):
    checks = "pending"
# A cancelled current run did not fail and did not pass: somebody stopped it,
# or a job ran past `timeout-minutes` (Actions reports that as cancelled too),
# and nothing re-ran it. Neither red nor green.
elif "CANCELLED" in results:
    checks = "cancelled"
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
    elif checks == "cancelled":
        tone, detail = "warning", "checks cancelled"
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
