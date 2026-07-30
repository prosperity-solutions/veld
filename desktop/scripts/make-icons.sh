#!/usr/bin/env bash
#
# Regenerate desktop/assets/ from the repo's canonical brand assets.
#
# Two outputs, two sources, both already canonical per docs/branding.md:
#
#   assets/icon.png            1024², the app icon — the favicon's shape (rounded
#                              dark square, white V, accent dot), which is what
#                              veld already shows in a browser tab.
#   assets/trayTemplate.png    18², the menu-bar icon, plus @2x. Drawn from
#   assets/trayTemplate@2x.png `logo.svg` — the same mark the Hammerspoon
#                              menu-bar widget uses (integrations/hammerspoon/
#                              Veld.spoon/icon.png), so the two menu-bar
#                              presences are one identity.
#
# **A template image, so it is black + alpha and nothing else.** macOS tints a
# `*Template` image for the current menu bar, which is the only way one asset can
# be legible in both light and dark mode; the accent dot survives as a shape, not
# as a colour. (The Hammerspoon widget sets its icon non-template and so is white
# on a light menu bar — worth fixing there, separately.)
#
# Rasterising is QuickLook (`qlmanage`), i.e. WebKit: macOS-only, but the outputs
# are committed, so this only runs when the brand changes. ImageMagick's built-in
# SVG renderer was tried first and its curves are visibly blobby at icon sizes.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
assets="$here/../assets"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$assets"

command -v qlmanage >/dev/null || {
  echo "qlmanage not found — this script is macOS-only (assets are committed)." >&2
  exit 1
}

# The mark's paths, lifted from the repo root's logo.svg (32² viewBox) and the
# data-URI favicon in website/index.html (48² viewBox). Duplicated here rather
# than parsed out of an HTML attribute: a generator that greps a marketing page is
# the more surprising coupling. docs/branding.md names both as canonical.
mark_v='M13.2 28L4 4H8.4L15.7 23.8H15.8L23.1 4H27.5L18.3 28H13.2Z'
mark_dot='M24.5 29C25.8807 29 27 27.8807 27 26.5C27 25.1193 25.8807 24 24.5 24C23.1193 24 22 25.1193 22 26.5C22 27.8807 23.1193 29 24.5 29Z'
tile='M40 0H8C3.58 0 0 3.58 0 8V40C0 44.42 3.58 48 8 48H40C44.42 48 48 44.42 48 40V8C48 3.58 44.42 0 40 0Z'
tile_v='M21.1 36L11.9 12H16.3L23.6 31.8H23.7L31 12H35.4L26.2 36H21.1Z'
tile_dot='M32.5 36C33.88 36 35 34.88 35 33.5C35 32.12 33.88 31 32.5 31C31.12 31 30 32.12 30 33.5C30 34.88 31.12 36 32.5 36Z'

render() { # <svg-file> <size> <dest>
  rm -f "$tmp/$(basename "$1").png"
  qlmanage -t -s "$2" -o "$tmp" "$1" >/dev/null 2>&1
  mv "$tmp/$(basename "$1").png" "$3"
}

cat >"$tmp/icon.svg" <<EOF
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 48 48">
<path d="$tile" fill="#0A0A0B"/>
<path d="$tile_v" fill="#FFFFFF"/>
<path d="$tile_dot" fill="#C4F56A"/>
</svg>
EOF
render "$tmp/icon.svg" 1024 "$assets/icon.png"

# Rendered large and then downsampled, **not** asked for at 18px: QuickLook has a
# minimum thumbnail size and pads below it, which produced a nearly empty 18²
# image. Downsampling a 512² render is also simply better AA at this size.
cat >"$tmp/trayTemplate.svg" <<EOF
<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 32 32">
<path d="$mark_v" fill="#000000"/>
<path d="$mark_dot" fill="#000000"/>
</svg>
EOF
render "$tmp/trayTemplate.svg" 512 "$tmp/tray-big.png"
command -v magick >/dev/null || {
  echo "ImageMagick (magick) not found — needed to downsample the tray icon." >&2
  exit 1
}
magick "$tmp/tray-big.png" -filter Lanczos -resize 18x18 "$assets/trayTemplate.png"
magick "$tmp/tray-big.png" -filter Lanczos -resize 36x36 "$assets/trayTemplate@2x.png"

echo "Wrote:"
ls -l "$assets"
