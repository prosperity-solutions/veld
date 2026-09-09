#!/usr/bin/env bash
# The earlier-Claude-sessions picker for this repo's panes (`ide.panes[].sessions`).
#
# Prints one row per line in veld's session-list contract:
#
#   <session-id>\t<what to show>\t<a quieter detail line>
#
# **Nothing about Claude Code reaches veld.** This script is the adapter, exactly
# as `pr-badge.sh` is the adapter for `gh`: it knows where Claude keeps its
# transcripts, and veld knows only "a value, and two strings to render". A Codex
# or Pi pane ships a different script and gets the same picker.
#
# Three parts of the contract this leans on:
#   - the value is what lands in ${veld.pane.token}, so the pane's own declared
#     `resume` (`claude --resume <id> …`) is what runs. This script cannot
#     contribute a command, only choose which session one opens.
#   - exit 0 with no output means "none here", and the pane offers no picker at
#     all — which is the right answer in a worktree nobody has used yet.
#   - a non-zero exit renders the picker in a failed state with our last stderr
#     line, so an error is worth writing there rather than swallowing.
#
# veld runs this with stdin closed, no terminal, `NO_COLOR=1`, a 10s deadline and
# a cap on how much it will read, from the worktree root.
set -uo pipefail

# Where Claude Code keeps a project's transcripts. Not a documented interface —
# it is read off disk precisely because the CLI exposes no "list my sessions for
# this directory" command — so every step below fails soft: a layout change costs
# the picker, never the pane.
#
# The directory name is the working directory with every character that is not a
# letter or a digit replaced by `-`. Derived here rather than hardcoded because
# panes run in whichever worktree you opened them in.
#
# One known divergence, left alone deliberately: `sed` substitutes per
# *character* and Claude Code per UTF-16 *code unit*, so a path containing an
# astral character (an emoji) produces one dash here and two there, and the
# picker is silently absent for that worktree. Matching that in `sed` is not
# worth what it costs; a worktree directory named with an emoji is rare and the
# failure is confined to the picker.
#
# **A lister does NOT run in your login shell.** The pane does — `$SHELL -l -i -c`
# — but veld runs this one directly with the daemon's environment plus a resolved
# `PATH`. So `CLAUDE_CONFIG_DIR` exported from `.zshrc` is visible to `claude` in
# the pane and *not* here, and the mismatch is silent: this script would look in
# the default directory, find nothing, exit 0, and the picker simply would not
# appear. If you move Claude's config directory, set it in `veld.json` instead —
# `"argv": ["scripts/veld/claude-sessions.sh", "--config-dir", "/path"]` — where
# both halves can see it.
config_dir="${CLAUDE_CONFIG_DIR:-}"
if [ "${1:-}" = "--config-dir" ] && [ -n "${2:-}" ]; then
  config_dir="$2"
fi
# `${HOME:-}` and an explicit emptiness check, not a bare `$HOME`: `set -u` would
# abort with status 1 under a stripped environment (a launchd job, a container),
# and the picker would render "failed" for a machine that simply has no sessions.
if [ -z "$config_dir" ]; then
  # Checked before it is used, not by string-matching the result: `HOME=/` (some
  # containers set that for root) makes the naive join `//.claude`, which an
  # equality test against `/.claude` misses.
  [ -n "${HOME:-}" ] || exit 0
  config_dir="$HOME/.claude"
fi

slug=$(printf '%s' "$PWD" | sed 's/[^A-Za-z0-9]/-/g')
dir="$config_dir/projects/$slug"
[ -d "$dir" ] || exit 0

# `find`+`sort`, not `ls -t`: a directory with no matches makes `ls` write to
# stderr and exit non-zero, which the contract reads as "this script is broken"
# rather than "there is nothing here". Scoped to one directory, never a walk.
#
# `-printf` is GNU-only, so the mtime comes from `stat`, whose flag differs
# between BSD (macOS) and GNU.
#
# **GNU first, and the order is not cosmetic.** GNU's `-f` is `--file-system` and
# takes no argument, so `stat -f '%m' FILE` on Linux reads `%m` as a *second
# file operand*: it complains about `%m` on stderr (suppressed here) and prints a
# six-line filesystem dump for the real file **to stdout**, then exits 1. The
# `||` fires, so the substitution captures the dump *and* the epoch. BSD's `stat`
# has no `-c`, and rejects it with a usage message on stderr and nothing on
# stdout — which is what a clean fallback looks like. So the platform whose
# failure is clean goes second.
#
# The digit guard is the belt: whatever comes back, only a plain number is used,
# so an unforeseen third `stat` cannot put text into a sort key.
mtime() {
  local out
  out=$(stat -c '%Y' "$1" 2>/dev/null) || out=$(stat -f '%m' "$1" 2>/dev/null) || out=""
  case "$out" in
    '' | *[!0-9]*) echo 0 ;;
    *) echo "$out" ;;
  esac
}

# Newest first, and a hard limit well under veld's own 50: this is a picker, and
# a picker of 50 conversations is a directory listing. What you are looking for
# after a lunch break is in the top handful.
LIMIT=${VELD_CLAUDE_SESSIONS_LIMIT:-15}

rows=$(
  for file in "$dir"/*.jsonl; do
    [ -f "$file" ] || continue
    printf '%s\t%s\n' "$(mtime "$file")" "$file"
  done | sort -rn | head -n "$LIMIT"
)
[ -n "$rows" ] || exit 0

# Relative time in whole units. Written out rather than reached for with `date
# -d`, whose argument syntax is one of the sharper BSD/GNU differences.
ago() {
  local secs=$(( $(date +%s) - $1 ))
  if [ "$secs" -lt 3600 ]; then
    echo "$(( secs / 60 ))m ago"
  elif [ "$secs" -lt 86400 ]; then
    echo "$(( secs / 3600 ))h ago"
  else
    echo "$(( secs / 86400 ))d ago"
  fi
}

# The first thing the user actually typed, as the row's label. Best-effort and
# quiet: a transcript whose shape has moved on gives an empty summary and the row
# falls back to its timestamp, rather than the picker disappearing.
summary() {
  command -v python3 >/dev/null 2>&1 || return 0
  python3 - "$1" <<'PY' 2>/dev/null || true
import json, sys

def text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        for part in content:
            if isinstance(part, dict) and part.get("type") == "text":
                return part.get("text", "")
    return ""

with open(sys.argv[1], encoding="utf-8", errors="replace") as f:
    for line in f:
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("type") != "user":
            continue
        body = text((row.get("message") or {}).get("content"))
        # Skip the machinery: tool results and the CLI's own injected turns are
        # `type: "user"` too, and none of them is what the person asked for.
        if not body or body.startswith(("<", "Caveat:", "[Request interrupted")):
            continue
        # A slash command arrives as a user turn whose body is the whole skill
        # file, with what the person actually typed at the bottom under
        # `ARGUMENTS:`. Without this every `/ship` session in a repo that has one
        # gets the same first 90 characters of preamble as its label.
        marker = "\nARGUMENTS:"
        if marker in body:
            body = body.split(marker, 1)[1]
        # One line, and short — veld clips at 160 characters, and a row that
        # needs all of them is a row you cannot scan.
        summary = " ".join(body.split())[:90]
        if summary:
            print(summary)
        break
PY
}

printf '%s\n' "$rows" | while IFS=$'\t' read -r ts file; do
  id=$(basename "$file" .jsonl)
  # veld drops a row whose value is not a plain id, so a stray file in that
  # directory costs its own row and nothing else. Skipping it here keeps the
  # picker's "N line(s) skipped" note for genuine surprises.
  case "$id" in
    *[!A-Za-z0-9._:@/-]* | "" | [!A-Za-z0-9]*) continue ;;
  esac
  when=$(ago "$ts")
  what=$(summary "$file")
  if [ -n "$what" ]; then
    printf '%s\t%s · %s\t%s\n' "$id" "$when" "$what" "$id"
  else
    printf '%s\t%s\t%s\n' "$id" "$when" "$id"
  fi
done
