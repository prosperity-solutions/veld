#!/usr/bin/env bash
# Assemble the GitHub Pages site for veld.oss.life.li into one directory.
#
#   website/build.sh [OUT]      # OUT defaults to _site/ at the repo root
#
# GitHub Pages serves files and nothing else: no redirects, no headers, no
# rewrites. Every route the old nginx config answered is therefore a *file*
# here, copied from its source of truth at deploy time. The Pages workflow
# deploys on every push to main, so each file tracks main exactly as the old
# redirects to raw.githubusercontent.com/.../main did.
#
# Then it checks the result, so CI fails on a missing route instead of the
# live site 404ing it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/_site}"
SRC="$ROOT/website"

# OUT is wiped first, so refuse anything that is not plainly a build directory.
case "$OUT" in
  "" | / | "$ROOT" | "$ROOT/" | "$HOME" | "$HOME/" | . | ..)
    echo "refusing to build into '$OUT'" >&2
    exit 2
    ;;
esac
rm -rf "$OUT"
mkdir -p "$OUT"

# The page and its assets. AGENTS.md and this script are not part of the site.
cp "$SRC"/index.html "$SRC"/og.png "$SRC"/favicon.svg "$SRC"/logo.svg \
  "$SRC"/logo-wordmark.svg "$SRC"/robots.txt "$SRC"/llms.txt "$SRC"/llms-full.txt "$OUT"/
cp -R "$SRC"/fonts "$OUT"/fonts

# Pages answers any unknown path with 404.html. The page is the whole site, so
# an unknown path shows it (with a 404 status), as nginx's fallback did. The
# page's fonts and llms links are relative, so pin the base to / — otherwise
# /a/b would ask for /a/fonts/… and render unstyled.
sed 's|<head>|<head>\n  <base href="/">|' "$SRC"/index.html >"$OUT"/404.html
grep -q '<base href="/">' "$OUT"/404.html || { echo "404.html: no <head> to put <base> in" >&2; exit 1; }

# `curl -fsSL https://veld.oss.life.li/get | bash`, and `veld self update`,
# fetch this. It used to be a 302 to main's install.sh; now it is that file.
cp "$ROOT"/install.sh "$OUT"/get

# `"$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json"` in every
# veld.json. Used to be a redirect to main's schema/ on GitHub.
mkdir -p "$OUT"/schema
cp -R "$ROOT"/schema/. "$OUT"/schema/

# llms.txt convention's well-known location (was a 301 to /llms.txt).
mkdir -p "$OUT"/.well-known
cp "$SRC"/llms.txt "$OUT"/.well-known/llms.txt

# Retired pages, formerly 301s to /. A page that sends the reader home is the
# closest a static host gets.
for page in humans agents; do
  mkdir -p "$OUT/$page"
  cat >"$OUT/$page/index.html" <<'HTML'
<!doctype html>
<meta charset="utf-8">
<title>veld</title>
<link rel="canonical" href="https://veld.oss.life.li/">
<meta http-equiv="refresh" content="0; url=/">
<a href="/">veld.oss.life.li</a>
HTML
done

# The custom domain. A workflow deploy ignores this file (the domain lives in
# the repo's Pages settings), but it documents the domain next to the site.
echo "veld.oss.life.li" >"$OUT"/CNAME

# --- check -----------------------------------------------------------------
fail=0
for f in index.html 404.html get llms.txt llms-full.txt .well-known/llms.txt robots.txt \
  favicon.svg og.png schema/v3/veld.schema.json humans/index.html agents/index.html; do
  [[ -s "$OUT/$f" ]] || { echo "missing route: /$f" >&2; fail=1; }
done
# The installer and `veld self update` refuse anything that does not start
# with a shebang (setup.rs `looks_like_install_script`).
head -c2 "$OUT/get" | grep -q '#!' || { echo "/get is not a shell script" >&2; fail=1; }
# Every font the page asks for has to be there; a missing one fails silently.
while read -r font; do
  [[ -s "$OUT/${font#/}" ]] || { echo "index.html references missing $font" >&2; fail=1; }
done < <(grep -oE '/?fonts/[A-Za-z0-9._-]+\.woff2' "$SRC/index.html" | sort -u)
[[ $fail -eq 0 ]] || exit 1

echo "site built in $OUT ($(find "$OUT" -type f | wc -l | tr -d ' ') files)"
