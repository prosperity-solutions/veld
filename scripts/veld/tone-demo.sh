#!/usr/bin/env bash
# A badge that walks through every tone, one click at a time.
#
# **This exists to be looked at.** Badge colours are the one part of
# `ide.extensions` no test can check — a contrast ratio is arithmetic, but "is
# this yellow readable at 12px on this monitor" is not — so this puts all five
# tones one click apart in a real top bar.
#
# It doubles as the worked example of two things that are easy to get wrong when
# writing an extension:
#
#   - **State belongs outside the script.** A badge command is re-run from
#     scratch every time, so anything it needs to remember lives on disk. This
#     uses `.veld-dev/`, which is gitignored, rather than the worktree.
#   - **A badge cannot advance itself.** Clicking a *badge* opens its link or
#     runs an action; it does not re-run the badge's own command. So the cycling
#     is an `action` (`tone-demo-next`) that this badge offers by id, and veld
#     re-reads the badges after any action runs — which is what makes the colour
#     change land immediately.
#
# Delete this, its `action` sibling, and both `veld.json` entries once the tones
# are settled. Nothing else depends on it.
set -uo pipefail

state_dir="${VELD_TONE_DEMO_DIR:-.veld-dev}"
state="$state_dir/tone-demo"
tones=(neutral info success warning danger)
# A glyph per tone as well, so the `icon` field of the contract is exercised too
# — and so it is obvious at a glance that an icon from the output overrides the
# one the declaration carries.
icons=(circle-check eye check hourglass alert-triangle)

if [ "${1:-}" = "next" ]; then
  mkdir -p "$state_dir"
  current=$(cat "$state" 2>/dev/null || echo 0)
  case "$current" in
    '' | *[!0-9]*) current=0 ;;
  esac
  echo $(((current + 1) % ${#tones[@]})) >"$state"
  exit 0
fi

index=$(cat "$state" 2>/dev/null || echo 0)
case "$index" in
  '' | *[!0-9]*) index=0 ;;
esac
index=$((index % ${#tones[@]}))
tone="${tones[$index]}"
icon="${icons[$index]}"

printf '{"text":"tone: %s","tone":"%s","icon":"%s","tooltip":"Badge tone %d of %d. Right-click to refresh; click for the next one.","actions":[{"id":"tone-demo-next","label":"Next tone"}]}\n' \
  "$tone" "$tone" "$icon" "$((index + 1))" "${#tones[@]}"
