#!/usr/bin/env bash
#
# Fetch the stock Caddy binary Veld ships, verified against the hash pinned in
# scripts/release/caddy.sha512.
#
#   scripts/release/fetch-caddy.sh <goos> <goarch> <output-path>
#
#   goos:   darwin | linux     goarch: amd64 | arm64
#
# The hash is pinned in this repository, not read from the release it came
# from: a checksums file fetched next to the archive would vouch for whatever
# the download served.
set -euo pipefail

goos=${1:?usage: fetch-caddy.sh <goos> <goarch> <output-path>}
goarch=${2:?usage: fetch-caddy.sh <goos> <goarch> <output-path>}
out=${3:?usage: fetch-caddy.sh <goos> <goarch> <output-path>}

case "$goos" in
  darwin) os=mac ;;
  linux) os=linux ;;
  *) echo "fetch-caddy: unsupported goos '$goos'" >&2; exit 2 ;;
esac
case "$goarch" in
  amd64 | arm64) ;;
  *) echo "fetch-caddy: unsupported goarch '$goarch'" >&2; exit 2 ;;
esac

pins="$(dirname "$0")/caddy.sha512"
line=$(grep -E "^[0-9a-f]{128}  caddy_[0-9.]+_${os}_${goarch}\.tar\.gz$" "$pins") || {
  echo "fetch-caddy: no pinned hash for ${os}_${goarch} in $pins" >&2
  exit 1
}
want=${line%%  *}
archive=${line##*  }
version=${archive#caddy_}
version=${version%%_*}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL --retry 3 -o "$tmp/$archive" \
  "https://github.com/caddyserver/caddy/releases/download/v${version}/${archive}"

if command -v sha512sum >/dev/null 2>&1; then
  got=$(sha512sum "$tmp/$archive" | cut -d' ' -f1)
else
  got=$(shasum -a 512 "$tmp/$archive" | cut -d' ' -f1)
fi
if [ "$got" != "$want" ]; then
  echo "fetch-caddy: $archive does not match its pinned SHA-512" >&2
  echo "  want $want" >&2
  echo "  got  $got" >&2
  exit 1
fi

tar -xzf "$tmp/$archive" -C "$tmp" caddy
mkdir -p "$(dirname "$out")"
install -m 0755 "$tmp/caddy" "$out"
echo "fetch-caddy: Caddy v${version} (${os}_${goarch}) -> $out"
