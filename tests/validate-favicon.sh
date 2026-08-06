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
# propagates as a failure listing every surface still carrying the old one. The
# mark itself is `logo.svg` translated +8/+8 into the 48-box tile, which is the
# same geometry `desktop/scripts/make-icons.py` rasterises for the app icon;
# that relationship is prose in docs/branding.md, not something this gate can
# check, because it would mean parsing path data.
#
# The `/ide` shell (crates/veld-daemon/ui/index.html) is checked in source form,
# and its built single-file bundle is checked too whenever one exists — Vite
# copies the tag through untouched today, and this is what would notice if a
# future Vite stopped doing that.
# Deliberately out of scope: the feedback overlay (it is injected into someone
# else's page and must never touch that page's icon) and
# crates/veld-daemon/frontend/dev/index.html (a dev harness, never served).
#
# The Desktop waiting screen is listed even though an Electron BrowserWindow
# renders no page favicon at all: the copy is inert today, and it is here so the
# page stays correct if it is ever opened anywhere that does show one.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CANONICAL="$REPO_ROOT/website/favicon.svg"

# Every surface that inlines the mark. Keep in sync with the branding table in
# docs/branding.md — and with the reverse scan at the bottom, which is what
# catches a *new* surface that never registered itself here.
SURFACES=(
  "website/index.html"
  "crates/veld-gateway/src/pages.rs"
  "crates/veld-daemon/assets/management-ui.html"
  "crates/veld-daemon/ui/index.html"
  "desktop/src/main.js"
)

# Built artifacts: checked when present, never required. A clean checkout has
# none, and CI's `ui` job has the interesting one.
OPTIONAL_SURFACES=(
  "crates/veld-daemon/ui/dist/index.html"
)

fail() {
  echo "error: $*" >&2
  exit 1
}

# A gate that has rotted into checking nothing passes trivially, which reads as
# a clean bill of health — the same reason the workflow-gate script beside this
# one self-tests. An emptied SURFACES array would print "all 0 surfaces match".
[ "${#SURFACES[@]}" -ge 5 ] || fail "SURFACES lists ${#SURFACES[@]} entries, expected at least 5 — see docs/branding.md"

[ -f "$CANONICAL" ] || fail "canonical favicon missing: website/favicon.svg"

# `$(cat …)` strips the trailing newline(s), so what is left must contain no
# newline of its own — deliberately not a `wc -l` test, which counts newline
# *bytes*: a valid one-line file saved without a trailing newline counts 0, and
# a genuinely two-line file saved without one counts 1 and would pass.
svg="$(cat "$CANONICAL")"
svg="${svg%$'\r'}"   # a core.autocrlf checkout would otherwise derive a URI ending in CR
case "$svg" in
  *$'\n'*) fail "website/favicon.svg must be a single line (it is inlined into an href)" ;;
  *$'\r'*) fail "website/favicon.svg contains a carriage return — check out with LF endings" ;;
esac

# The escaping the data-URI needs, and the assertion that nothing else needs any.
# `#` starts a fragment, so an unescaped one truncates the icon at the first fill
# colour; `%` would be read as the start of an escape; `"` would close the href;
# `&` opens a character reference in an HTML attribute. The last three are about
# the *host* languages the literal is pasted into: a backtick or `${` would
# interpolate inside the JS template literal, and `{title}`/`{wordmark}`/`{body}`
# are substituted across the whole gateway SHELL constant including this line.
# None of them can occur in a mark made of paths — the point of the guard is
# that they stay impossible rather than being noticed later.
case "$svg" in
  *'%'*)  fail "website/favicon.svg contains a literal '%' — encode it as %25" ;;
  *'"'*)  fail "website/favicon.svg must use single-quoted attributes (a '\"' closes the href)" ;;
  *'&'*)  fail "website/favicon.svg contains '&', which starts a character reference in an href" ;;
  *'\`'*) fail "website/favicon.svg contains a backtick, which breaks the JS template literal in desktop/src/main.js" ;;
  *'$'*)  fail "website/favicon.svg contains '\$', which can interpolate in desktop/src/main.js" ;;
  *'{'*|*'}'*) fail "website/favicon.svg contains a brace, which collides with the {title}/{wordmark}/{body} substitution in crates/veld-gateway/src/pages.rs" ;;
esac
expected="data:image/svg+xml,${svg//#/%23}"

missing=0
check_surface() {
  if grep -aqF -- "$expected" "$REPO_ROOT/$1"; then
    echo "  ok   $1"
  else
    echo "  FAIL $1" >&2
    missing=1
  fi
}

for surface in "${SURFACES[@]}"; do
  [ -f "$REPO_ROOT/$surface" ] || fail "surface listed but not found: $surface"
  check_surface "$surface"
done

stale_build=0
for surface in "${OPTIONAL_SURFACES[@]}"; do
  if [ -f "$REPO_ROOT/$surface" ]; then
    before="$missing"
    check_surface "$surface"
    [ "$missing" = "$before" ] || stale_build=1
  else
    echo "  skip $surface (not built)"
  fi
done

# A built artifact lags its source by definition, so say so rather than sending
# someone to edit a generated file.
[ "$stale_build" = "0" ] || echo "  (a built artifact is stale — rebuild it, or delete it and re-run)" >&2

if [ "$missing" != "0" ]; then
  cat >&2 <<EOF

error: the favicon above does not match website/favicon.svg on every surface.

Expected each listed file to contain this exact string:

$expected

Fix by copying it verbatim into the surface's icon link (or, if the mark itself
changed, edit website/favicon.svg and re-run to see who lags).
EOF
  exit 1
fi

# Reverse direction: a *new* page that inlines an icon and never registers here
# is exactly the drift this gate exists to stop — the /ide shell had grown its
# own hand-drawn mark and the v1 dashboard had none, and no check saw either.
# `-a` because crates/veld-daemon/ui/src/App.tsx contains a NUL byte, which
# makes grep skip the file silently (AGENTS.md).
unregistered=""
while IFS= read -r hit; do
  [ -n "$hit" ] || continue
  case " ${SURFACES[*]} ${OPTIONAL_SURFACES[*]} " in
    *" $hit "*) ;;
    *) unregistered="$unregistered  $hit"$'\n' ;;
  esac
done <<EOF
$(cd "$REPO_ROOT" && grep -ralE "rel=[\"']icon[\"']" . \
    --exclude-dir=node_modules --exclude-dir=target --exclude-dir=.git \
    --exclude-dir=dist --exclude-dir=.veld-dev \
    --exclude="*.md" --exclude="validate-favicon.sh" 2>/dev/null \
  | sed 's|^\./||' | sort)
EOF

if [ -n "$unregistered" ]; then
  printf 'error: these files inline an icon link but are not in SURFACES:\n%s\n' "$unregistered" >&2
  echo "Add each to SURFACES (and to the branding table in docs/branding.md), or" >&2
  echo "exclude it here with a reason if it is genuinely not a served page." >&2
  exit 1
fi

echo "favicon: all ${#SURFACES[@]} surfaces match website/favicon.svg"
