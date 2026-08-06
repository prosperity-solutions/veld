#!/usr/bin/env bash
# Assert every user-facing surface ships the *same* favicon mark.
#
# Usage: ./tests/validate-favicon.sh
#
# The brand mark (rounded dark tile, white `V`, accent dot — docs/branding.md)
# is inlined as a data-URI on every page a Veld binary serves, because those
# pages must stay self-contained: no external request for an icon. That means
# the same bytes live in five files across three languages, and nothing in any
# type system ties them together. This script is that tie.
#
# `website/favicon.svg` is the canonical copy — the only one that is a real SVG
# file rather than an escaped literal — and the expected data-URI is *derived*
# from it here, so a change to the mark is a one-file edit that this gate then
# propagates as a failure listing every surface still carrying the old one.
#
# The `/ide` shell (crates/veld-daemon/ui/index.html) is checked in source form;
# Vite copies the tag through to the built single-file bundle untouched.
# Deliberately out of scope: the feedback overlay (it is injected into someone
# else's page and must never touch that page's icon) and
# crates/veld-daemon/frontend/dev/index.html (a dev harness, never served).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CANONICAL="$REPO_ROOT/website/favicon.svg"

# Every surface that inlines the mark. Keep in sync with the branding table in
# docs/branding.md.
SURFACES=(
  "website/index.html"
  "crates/veld-gateway/src/pages.rs"
  "crates/veld-daemon/assets/management-ui.html"
  "crates/veld-daemon/ui/index.html"
  "desktop/src/main.js"
)

fail() {
  echo "error: $*" >&2
  exit 1
}

[ -f "$CANONICAL" ] || fail "canonical favicon missing: website/favicon.svg"

# One line, so the derivation below is a substring an HTML attribute can hold.
if [ "$(wc -l <"$CANONICAL" | tr -d ' ')" != "1" ]; then
  fail "website/favicon.svg must be a single line (it is inlined into an href)"
fi
svg="$(cat "$CANONICAL")"

# The escaping the data-URI needs, and the assertion that nothing else needs any.
# `#` starts a fragment, so an unescaped one truncates the icon at the first
# fill colour; `%` would be read as the start of an escape; `"` would close the
# href. Anything else in an SVG path (`<`, `>`, `'`, spaces) is carried
# literally by every browser and keeps the literal greppable.
case "$svg" in
  *'%'*) fail "website/favicon.svg contains a literal '%' — encode it as %25" ;;
  *'"'*) fail "website/favicon.svg must use single-quoted attributes (a '\"' closes the href)" ;;
esac
expected="data:image/svg+xml,${svg//#/%23}"

missing=0
for surface in "${SURFACES[@]}"; do
  path="$REPO_ROOT/$surface"
  [ -f "$path" ] || fail "surface listed but not found: $surface"
  if grep -aqF -- "$expected" "$path"; then
    echo "  ok   $surface"
  else
    echo "  FAIL $surface" >&2
    missing=1
  fi
done

if [ "$missing" != "0" ]; then
  cat >&2 <<EOF

error: the favicon above does not match website/favicon.svg on every surface.

Expected each listed file to contain this exact string:

$expected

Fix by copying it verbatim into the surface's \`rel="icon"\` link (or, if the
mark itself changed, edit website/favicon.svg and re-run to see who lags).
EOF
  exit 1
fi

echo "favicon: all ${#SURFACES[@]} surfaces match website/favicon.svg"
