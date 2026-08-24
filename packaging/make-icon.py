#!/usr/bin/env python3
"""Draws the app icon.

Generated rather than checked in, so there is no binary asset to lose track of
and the design lives somewhere it can be read and changed.

Two overlapping frames, one red and one cyan, offset horizontally — an anaglyph
reduced to the one thing that makes it an anaglyph.
"""
import math
import struct
import subprocess
import sys
import zlib
from pathlib import Path

BG_TOP, BG_BOTTOM = (0x1B, 0x1E, 0x28), (0x0D, 0x0F, 0x16)
RED, CYAN = (0xE8, 0x3B, 0x3B), (0x2E, 0xC8, 0xD0)


def rounded(x, y, rect, radius):
    """Signed coverage of a rounded rectangle, anti-aliased at the edge."""
    x0, y0, x1, y1 = rect
    cx = max(x0 + radius - x, 0, x - (x1 - radius))
    cy = max(y0 + radius - y, 0, y - (y1 - radius))
    outside = math.hypot(cx, cy) - radius
    if x < x0 or x > x1 or y < y0 or y > y1:
        inset = min(x - x0, x1 - x, y - y0, y1 - y)
        if inset < -1:
            return 0.0
    return max(0.0, min(1.0, 0.5 - outside))


def draw(size):
    px = bytearray(size * size * 4)
    s = size / 1024.0
    pad = 240 * s
    shift = 118 * s
    radius = 96 * s
    # The two eye views, offset in opposite directions. Kept well inside the
    # plate so the coloured fringes are a large part of what you see — at 32
    # pixels that offset is the only thing that says "anaglyph".
    left = (pad - shift, pad, size - pad - shift, size - pad)
    right = (pad + shift, pad, size - pad + shift, size - pad)
    outer = (60 * s, 60 * s, size - 60 * s, size - 60 * s)

    for y in range(size):
        for x in range(size):
            i = (y * size + x) * 4
            plate = rounded(x, y, outer, 200 * s)
            if plate <= 0.0:
                continue
            t = y / max(size - 1, 1)
            base = [
                BG_TOP[c] + (BG_BOTTOM[c] - BG_TOP[c]) * t for c in range(3)
            ]
            a = rounded(x, y, left, radius)
            b = rounded(x, y, right, radius)
            # Screen blending, because that is what the two filters do to light.
            for c in range(3):
                v = base[c] / 255.0
                v = 1 - (1 - v) * (1 - a * RED[c] / 255.0)
                v = 1 - (1 - v) * (1 - b * CYAN[c] / 255.0)
                px[i + c] = int(max(0.0, min(1.0, v)) * 255)
            px[i + 3] = int(plate * 255)
    return bytes(px)


def write_png(path, size, data):
    raw = b"".join(
        b"\x00" + data[y * size * 4 : (y + 1) * size * 4] for y in range(size)
    )
    def chunk(tag, body):
        return (
            struct.pack(">I", len(body))
            + tag
            + body
            + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF)
        )
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(png)


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "packaging/AppIcon.icns")
    iconset = out.with_suffix(".iconset")
    iconset.mkdir(parents=True, exist_ok=True)
    # The sizes macOS asks for, each drawn rather than scaled so small ones
    # stay crisp.
    for size, names in {
        16: ["icon_16x16.png"],
        32: ["icon_16x16@2x.png", "icon_32x32.png"],
        64: ["icon_32x32@2x.png"],
        128: ["icon_128x128.png"],
        256: ["icon_128x128@2x.png", "icon_256x256.png"],
        512: ["icon_256x256@2x.png", "icon_512x512.png"],
        1024: ["icon_512x512@2x.png"],
    }.items():
        data = draw(size)
        for name in names:
            write_png(iconset / name, size, data)
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)
    print(f"wrote {out} ({out.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()
