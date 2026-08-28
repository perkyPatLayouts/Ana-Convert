#!/usr/bin/env python3
"""Draws the figures in docs/USER-GUIDE.md.

The figures are produced by running the real converter over a synthetic stereo
scene, not mocked up, so a picture in the guide cannot claim something the code
does not do. Regenerate them after any change that alters what the output looks
like:

    python3 docs/make-figures.py

Written with the standard library alone — the project has no Python
dependencies, and a figure script is not worth introducing one for.
"""
import struct
import subprocess
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
IMAGES = ROOT / "docs" / "images"
CLI = ROOT / "target" / "release" / "ana-convert"

# One eye. Small enough to read on a documentation page at full size.
W, H = 480, 270
HORIZON = 170


def run(cmd):
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"failed: {' '.join(str(c) for c in cmd)}\n{result.stderr.strip()}")
    return result.stdout


def write_png(path: Path, width: int, height: int, pixels: bytearray):
    """A minimal 8-bit RGB PNG, filter type 0 on every row."""
    rows = b"".join(
        b"\x00" + bytes(pixels[y * width * 3 : (y + 1) * width * 3]) for y in range(height)
    )

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def put(buf, width, x, y, rgb):
    if 0 <= x < width and 0 <= y < H:
        i = (y * width + x) * 3
        buf[i : i + 3] = bytes(rgb)


def rect(buf, width, x0, y0, w, h, rgb):
    for y in range(y0, y0 + h):
        for x in range(x0, x0 + w):
            put(buf, width, x, y, rgb)


def disc(buf, width, cx, cy, r, rgb):
    for y in range(cy - r, cy + r + 1):
        for x in range(cx - r, cx + r + 1):
            if (x - cx) ** 2 + (y - cy) ** 2 <= r * r:
                put(buf, width, x, y, rgb)


def eye(shift: int) -> bytearray:
    """One eye of the scene.

    `shift` moves an object by half its disparity, so passing the negative and
    positive halves produces a stereo pair. Depths chosen to give the guide
    something at the screen, something behind it and something well in front.
    """
    buf = bytearray(W * H * 3)
    for y in range(H):
        if y < HORIZON:  # sky, darkening upward
            t = y / HORIZON
            colour = (int(40 + 90 * t), int(70 + 110 * t), int(130 + 90 * t))
        else:  # ground
            t = (y - HORIZON) / (H - HORIZON)
            colour = (int(90 - 40 * t), int(70 - 30 * t), int(50 - 20 * t))
        rect(buf, W, 0, y, W, 1, colour)

    # Far ridge: behind the screen, so the eyes see it offset outward.
    far = shift * 3 // 2
    for x in range(W):
        peak = int(28 * abs(((x + 90) % 200) / 100.0 - 1.0))
        rect(buf, W, x + far, HORIZON - 34 + peak, 1, 34 - peak, (58, 66, 86))

    # Mid marker: sits exactly on the screen plane, so it never moves.
    rect(buf, W, 300, HORIZON - 46, 34, 46, (196, 150, 60))
    rect(buf, W, 296, HORIZON - 52, 42, 8, (168, 124, 44))

    # Near ball: well in front of the screen, the thing that hurts to watch.
    disc(buf, W, 168 - shift * 2, HORIZON - 18, 30, (208, 72, 56))
    disc(buf, W, 158 - shift * 2, HORIZON - 28, 10, (236, 148, 132))
    return buf


def side_by_side() -> bytearray:
    """The pair packed into one frame, which is what the CLI is fed."""
    left, right = eye(-4), eye(+4)
    out = bytearray(W * 2 * H * 3)
    for y in range(H):
        row = (y * W * 2) * 3
        out[row : row + W * 3] = left[y * W * 3 : (y + 1) * W * 3]
        out[row + W * 3 : row + W * 6] = right[y * W * 3 : (y + 1) * W * 3]
    return out


def first_frame_to_png(video: Path, png: Path):
    """Pulls frame one out as raw RGB and saves it, so figures show real output."""
    probe = run([
        "ffprobe", "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=width,height", "-of", "csv=p=0", str(video),
    ])
    width, height = (int(v) for v in probe.strip().split(",")[:2])
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(video), "-frames:v", "1",
         "-pix_fmt", "rgb24", "-f", "rawvideo", "-"],
        capture_output=True,
    ).stdout
    write_png(png, width, height, bytearray(raw[: width * height * 3]))


def main():
    if not CLI.exists():
        sys.exit(f"{CLI} is missing — run: cargo build --release -p ana-cli")
    IMAGES.mkdir(parents=True, exist_ok=True)
    work = ROOT / "target" / "figures"
    work.mkdir(parents=True, exist_ok=True)

    # The scene, as a one-second side-by-side clip for the converter to read.
    write_png(work / "pair.png", W * 2, H, side_by_side())
    run(["ffmpeg", "-y", "-v", "error", "-loop", "1", "-i", str(work / "pair.png"),
         "-t", "0.4", "-r", "25", "-pix_fmt", "yuv444p", "-c:v", "libx264",
         "-qp", "0", str(work / "pair.mp4")])

    def convert(name, *args):
        out = work / f"{name}.mkv"
        run([str(CLI), "render", "-i", str(work / "pair.mp4"), "-o", str(out),
             "--source", "sbs", *args])
        first_frame_to_png(out, IMAGES / f"{name}.png")
        return out

    # What the guide shows, each one the converter's actual output.
    convert("scene-left", "--layout", "left")
    convert("scene-anaglyph", "--layout", "anaglyph")
    convert("scene-side-by-side", "--layout", "sbs")

    # Convergence, shown as anaglyph because that makes the disparity of every
    # object directly visible as the width of its colour fringe.
    for name, amount in [("converge-negative", "-3"), ("converge-zero", "0"),
                         ("converge-positive", "3")]:
        convert(name, "--layout", "anaglyph", "--convergence", amount)

    # The headline: take the anaglyph just produced and recover a full-colour
    # eye from it. Beside scene-left.png — the eye it was made from — this is
    # the honest before and after, including what recovery does not get back.
    anaglyph = work / "scene-anaglyph.mkv"
    recovered = work / "recovered-left.mkv"
    run([str(CLI), "render", "-i", str(anaglyph), "-o", str(recovered),
         "--source", "anaglyph", "--layout", "left"])
    first_frame_to_png(recovered, IMAGES / "recovered-left.png")

    for figure in sorted(IMAGES.glob("*.png")):
        print(f"  {figure.relative_to(ROOT)}  {figure.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
