#!/usr/bin/env python3
"""Regenerate desktop/assets/ from the Veld mark.

Outputs (committed; run this only when the brand changes):

  assets/icon.png             1024², the app icon — the favicon's shape (rounded
                              dark tile, white V, accent dot), inset in a
                              transparent margin the way macOS expects.
  assets/trayTemplate.png     18², the menu-bar icon, plus @2x. The bare mark in
  assets/trayTemplate@2x.png  black + alpha: a macOS *template* image, so the OS
                              tints it for the current menu bar. The same mark the
                              Hammerspoon widget shows
                              (integrations/hammerspoon/Veld.spoon/icon.png) — that
                              asset predates this script and is NOT regenerated
                              here, so the two drift if the mark changes.

**Why a rasteriser here instead of a tool.** The mark is two shapes — a polygon
and a circle (`logo.svg`; every V segment is a straight line) — so drawing it
analytically is both exact and shorter than wiring up a dependency. The two
obvious alternatives were tried and rejected:

* `qlmanage` (QuickLook/WebKit) composites thumbnails on an **opaque white
  background** and pads anything below its minimum size. That shipped a menu-bar
  icon that was a white tile with a dark V — a template image is alpha, so an
  opaque render is a solid blob — and it is invisible in a preview against a light
  background, which is how it got past review.
* ImageMagick's own SVG renderer is visibly blobby at icon sizes, and its
  `-resize` dropped the alpha channel to grayscale, which is the other half of the
  same bug.

Python's standard library writes PNG (zlib + a CRC) in a dozen lines, so there is
nothing to install, nothing macOS-only, and the output is byte-identical on every
machine.
"""

from __future__ import annotations

import pathlib
import struct
import zlib

# --- the mark, in logo.svg's 32×32 viewBox ----------------------------------
# The V as a polygon (the path has no curves) and the dot as a circle. Kept as
# geometry rather than as an SVG string so there is nothing to parse.
V_POLYGON = [
    (13.2, 28.0),
    (4.0, 4.0),
    (8.4, 4.0),
    (15.7, 23.8),
    (15.8, 23.8),
    (23.1, 4.0),
    (27.5, 4.0),
    (18.3, 28.0),
]
DOT_CENTER = (24.5, 26.5)
DOT_RADIUS = 2.5
# The mark's own bounds inside that viewBox — it is not centred in it, and an icon
# that inherits that looks off-centre for no visible reason.
MARK_BOX = (4.0, 4.0, 27.5, 29.0)

# These are copies, and there is no way around that: the brand's accent lives as
# `#C4F56A` in `website/index.html`, as an `oklch()` token in the UI's stylesheet,
# and as a literal here — a PNG cannot reference a CSS variable. So changing the
# accent means changing it in all three, which `docs/branding.md` records as the
# rule. Written as hex bytes rather than a named constant so a search for
# `C4F56A` finds this file too.
TILE = (0x0A, 0x0A, 0x0B)  # favicon tile, docs/branding.md
WHITE = (0xFF, 0xFF, 0xFF)
ACCENT = (0xC4, 0xF5, 0x6A)
BLACK = (0x00, 0x00, 0x00)

SUPERSAMPLE = 4  # 16 samples per pixel: enough for a straight-edged glyph

ASSETS = pathlib.Path(__file__).resolve().parent.parent / "assets"


def write_png(path: pathlib.Path, width: int, height: int, pixels: bytearray) -> None:
    """Write RGBA8. `pixels` is row-major, 4 bytes per pixel."""

    def chunk(kind: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        )

    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)  # filter type 0 (None) — the images are tiny
        raw += pixels[y * stride : (y + 1) * stride]
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(png)


def in_polygon(x: float, y: float, poly: list[tuple[float, float]]) -> bool:
    """Even-odd containment (ray cast to the right)."""
    inside = False
    n = len(poly)
    for i in range(n):
        x0, y0 = poly[i]
        x1, y1 = poly[(i + 1) % n]
        if (y0 > y) != (y1 > y):
            t = (y - y0) / (y1 - y0)
            if x < x0 + t * (x1 - x0):
                inside = not inside
    return inside


def rounded_rect(x: float, y: float, box: tuple[float, float, float, float], r: float) -> bool:
    x0, y0, x1, y1 = box
    if not (x0 <= x <= x1 and y0 <= y <= y1):
        return False
    # Only the four corner quadrants need the distance test.
    cx = x0 + r if x < x0 + r else (x1 - r if x > x1 - r else x)
    cy = y0 + r if y < y0 + r else (y1 - r if y > y1 - r else y)
    if cx == x or cy == y:
        return True
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def render(
    size: int,
    *,
    tile: bool,
    v_color: tuple[int, int, int],
    dot_color: tuple[int, int, int],
    margin: float,
    mark_scale: float,
) -> bytearray:
    """Rasterise the mark at `size`², optionally on a rounded tile.

    `margin` is the transparent inset as a fraction of the canvas — macOS draws its
    own shadow and expects an app icon's artwork to sit inside one, and a full-bleed
    tile reads as a bigger, blockier icon than everything beside it in the dock.
    `mark_scale` is the mark's height as a fraction of the *tile* (or of the canvas
    when there is no tile).
    """
    px = bytearray(size * size * 4)
    inset = margin * size
    tile_box = (inset, inset, size - inset, size - inset)
    tile_side = tile_box[2] - tile_box[0]
    # macOS's own icon grid puts the corner radius near 22% of the side.
    tile_radius = tile_side * 0.225

    mx0, my0, mx1, my1 = MARK_BOX
    mark_h = my1 - my0
    scale = (tile_side * mark_scale) / mark_h
    # Centre the mark's own bounds in the tile, not its viewBox.
    ox = tile_box[0] + tile_side / 2 - ((mx0 + mx1) / 2) * scale
    oy = tile_box[1] + tile_side / 2 - ((my0 + my1) / 2) * scale

    step = 1.0 / SUPERSAMPLE
    samples = SUPERSAMPLE * SUPERSAMPLE
    dcx, dcy = DOT_CENTER

    for py in range(size):
        for pxi in range(size):
            tile_hits = v_hits = dot_hits = 0
            for sy in range(SUPERSAMPLE):
                y = py + (sy + 0.5) * step
                for sx in range(SUPERSAMPLE):
                    x = pxi + (sx + 0.5) * step
                    if tile and rounded_rect(x, y, tile_box, tile_radius):
                        tile_hits += 1
                    # Into mark space.
                    ux = (x - ox) / scale
                    uy = (y - oy) / scale
                    # `elif`: the two glyph shapes are disjoint in this mark (the V
                    # spans x 13–18 where the dot sits at 22–27), so a sample belongs
                    # to at most one. If a future mark overlaps them, this and the
                    # sequential compositing below under-report coverage — a pixel
                    # split 8/8 would come out at alpha 0.75 rather than 1.0. Fix it
                    # there by accumulating glyph coverage once, not by adding a
                    # branch here.
                    if in_polygon(ux, uy, V_POLYGON):
                        v_hits += 1
                    elif (ux - dcx) ** 2 + (uy - dcy) ** 2 <= DOT_RADIUS**2:
                        dot_hits += 1
            if not (tile_hits or v_hits or dot_hits):
                continue
            # Composite back to front: tile, then the glyph over it.
            r = g = b = 0.0
            a = 0.0
            if tile_hits:
                cov = tile_hits / samples
                r, g, b, a = TILE[0] * cov, TILE[1] * cov, TILE[2] * cov, cov
            for hits, color in ((v_hits, v_color), (dot_hits, dot_color)):
                if not hits:
                    continue
                cov = hits / samples
                r = color[0] * cov + r * (1 - cov)
                g = color[1] * cov + g * (1 - cov)
                b = color[2] * cov + b * (1 - cov)
                a = cov + a * (1 - cov)
            i = (py * size + pxi) * 4
            # Un-premultiply, since PNG stores straight alpha.
            if a > 0:
                px[i] = min(255, round(r / a))
                px[i + 1] = min(255, round(g / a))
                px[i + 2] = min(255, round(b / a))
                px[i + 3] = min(255, round(a * 255))
    return px


def main() -> None:
    ASSETS.mkdir(parents=True, exist_ok=True)

    # App icon: the favicon's tile, inset for macOS's shadow, colour and all.
    size = 1024
    write_png(
        ASSETS / "icon.png",
        size,
        size,
        render(size, tile=True, v_color=WHITE, dot_color=ACCENT, margin=0.09, mark_scale=0.56),
    )

    # Menu-bar icon: the bare mark in black + alpha. No tile, no colour — macOS
    # tints a template image, so anything else here is thrown away, and the accent
    # dot survives as a shape. Nearly edge to edge: a menu bar is 18px tall and the
    # OS supplies its own padding.
    for name, px_size in (("trayTemplate.png", 18), ("trayTemplate@2x.png", 36)):
        write_png(
            ASSETS / name,
            px_size,
            px_size,
            render(
                px_size,
                tile=False,
                v_color=BLACK,
                dot_color=BLACK,
                margin=0.0,
                mark_scale=0.92,
            ),
        )

    for f in sorted(ASSETS.iterdir()):
        print(f"{f.relative_to(ASSETS.parent.parent)}  {f.stat().st_size} B")


if __name__ == "__main__":
    main()
