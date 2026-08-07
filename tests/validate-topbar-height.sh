#!/usr/bin/env bash
# Assert the Electron top bar's CSS height and the Desktop shell's constant agree.
#
# Usage: ./tests/validate-topbar-height.sh
#
# `.topbar.electron`'s height (crates/veld-daemon/ui/src/styles.css) is the row
# the frameless window draws veld's own controls into, and macOS draws the
# traffic lights on top of it. No CSS can move those buttons: their position is
# set once at window creation from `TOPBAR_HEIGHT` in desktop/src/main.js, which
# desktop/src/windows.js turns into `trafficLightPosition.y` — the value that
# centres a 12px light against the bar. So the same number lives in a stylesheet
# and in a main-process constant, in two languages, with nothing tying them
# together.
#
# Drift here is invisible to every other check in the repo: `TOPBAR_HEIGHT` has
# exactly one consumer, so tsc, biome, clippy and the node suites all stay green
# while the shipped app renders its window buttons off-centre against the bar —
# a defect only a human looking at the corner of a running Electron window can
# see. This script is that tie.
#
# Deliberately narrow: it checks the one pair that produces a visible defect.
# The traffic-light inset (`padding-left` on `.topbar.electron`) is arithmetic
# over `x: 13`, `TRAFFIC_LIGHT_SIZE` and macOS's own 20px button pitch — that
# last number is the OS's and exists nowhere in this repo, so a gate over it
# would be asserting a constant against itself.

set -euo pipefail

cd "$(dirname "$0")/.."

css="crates/veld-daemon/ui/src/styles.css"
js="desktop/src/main.js"

fail() {
  echo "topbar-height: $1" >&2
  exit 1
}

[ -f "$css" ] || fail "missing $css"
[ -f "$js" ] || fail "missing $js"

# The `height:` inside the `.topbar.electron { … }` block. `sed` range from the
# selector to its closing brace, then the first height in it — narrow on purpose,
# so an added declaration or a reordered block does not quietly match something
# else and report a pass.
css_height=$(sed -n '/^\.topbar\.electron[[:space:]]*{/,/^}/p' "$css" |
  sed -n 's/^[[:space:]]*height:[[:space:]]*\([0-9]\{1,\}\)px;.*/\1/p' |
  head -n 1)

js_height=$(sed -n 's/^const TOPBAR_HEIGHT = \([0-9]\{1,\}\);.*/\1/p' "$js" |
  head -n 1)

# An empty capture is the failure this gate most needs to survive: if either
# pattern stops matching (a refactor to a CSS variable, a `let`, a computed
# value), a naive comparison of "" with "" would *pass* and the gate would go
# quietly blind for the rest of its life.
[ -n "$css_height" ] || fail "could not read height from .topbar.electron in $css — if that block moved to a variable, update this gate rather than deleting it"
[ -n "$js_height" ] || fail "could not read TOPBAR_HEIGHT from $js — if it is no longer a literal, update this gate rather than deleting it"

if [ "$css_height" != "$js_height" ]; then
  fail "$css says .topbar.electron is ${css_height}px but $js says TOPBAR_HEIGHT is ${js_height}px — the macOS traffic lights are centred against the constant, so they will sit off the bar. Change both."
fi

echo "topbar-height: .topbar.electron and TOPBAR_HEIGHT agree (${css_height}px)"
