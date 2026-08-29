#!/usr/bin/env python3
"""Draws docs/sample/stereo-sample-sbs.mp4, the clip to learn the app on.

A synthetic stereo scene rather than a film, because a sample has to be
redistributable and has to have known depths. Everything in it sits at a
disparity this script chose, so what the convergence control does can be
checked against what it should do rather than guessed at.

The scene is built to exercise the interesting cases, including the one that
does not work:

  * a ridge behind the screen, a post exactly on it, and a ball well in front,
    so all three kinds of parallax are on screen at once
  * the ball's depth changes as it crosses, so convergence has to be judged
    against a moving target the way it is on a real film
  * saturated red and blue, the channels anaglyph splits on, so what colour
    recovery gets back — and what it does not — is visible rather than theoretical
  * the ball reaches a disparity high enough to fringe badly, which is the
    limitation the README is honest about and which no setting fixes

Run from the repository root:

    python3 docs/make-sample.py

Written with the standard library alone — the project has no Python
dependencies, and a sample script is not worth introducing one for.
"""
import json
import subprocess
import sys
from math import cos, pi, sin
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SAMPLE = ROOT / "docs" / "sample" / "stereo-sample-sbs.mp4"

# One eye. Small enough to commit, large enough that the fringing on the near
# ball is plainly visible rather than a suggestion.
W, H = 640, 360
FPS = 25
SECONDS = 10
FRAMES = FPS * SECONDS
HORIZON = 232

# Half the disparity of an object one unit behind the screen, in pixels. Every
# depth below is a multiple of this, so the whole scene can be pushed or pulled
# by changing one number.
UNIT = 5.0


def run(cmd, **kw):
    result = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if result.returncode != 0:
        sys.exit(f"failed: {' '.join(str(c) for c in cmd)}\n{result.stderr.strip()}")
    return result.stdout


def put(buf, x, y, rgb):
    if 0 <= x < W and 0 <= y < H:
        i = (y * W + x) * 3
        buf[i : i + 3] = rgb


def rect(buf, x0, y0, w, h, rgb):
    rgb = bytes(rgb)
    for y in range(y0, y0 + h):
        if not 0 <= y < H:
            continue
        x_from, x_to = max(x0, 0), min(x0 + w, W)
        if x_from < x_to:
            i = (y * W + x_from) * 3
            buf[i : i + (x_to - x_from) * 3] = rgb * (x_to - x_from)


def disc(buf, cx, cy, r, rgb):
    rgb = bytes(rgb)
    for y in range(cy - r, cy + r + 1):
        if not 0 <= y < H:
            continue
        span = int((r * r - (y - cy) ** 2) ** 0.5) if abs(y - cy) <= r else 0
        x_from, x_to = max(cx - span, 0), min(cx + span + 1, W)
        if x_from < x_to:
            i = (y * W + x_from) * 3
            buf[i : i + (x_to - x_from) * 3] = rgb * (x_to - x_from)


def backdrop(shift: float) -> bytearray:
    """Everything that never moves, for one eye.

    Built once per eye and copied per frame: at 250 frames a per-pixel redraw
    of the sky would dominate the runtime for no reason.
    """
    buf = bytearray(W * H * 3)

    for y in range(H):
        if y < HORIZON:  # sky, darkening upward
            t = y / HORIZON
            colour = (int(38 + 92 * t), int(66 + 112 * t), int(128 + 92 * t))
        else:  # ground
            t = (y - HORIZON) / (H - HORIZON)
            colour = (int(92 - 42 * t), int(72 - 32 * t), int(52 - 22 * t))
        rect(buf, 0, y, W, 1, colour)

    # Far ridge: behind the screen, so each eye sees it offset outward.
    far = round(shift * 1.5)
    for x in range(W):
        peak = int(38 * abs(((x + 120) % 260) / 130.0 - 1.0))
        rect(buf, x + far, HORIZON - 46 + peak, 1, 46 - peak, (56, 64, 84))

    # A line of posts running back towards the horizon, each one further away
    # than the last. Depth you can count, rather than depth you can only feel.
    for i, (px, py, ph) in enumerate(
        [(96, 46, 1.9), (196, 34, 1.4), (286, 26, 1.0), (356, 20, 0.7)]
    ):
        offset = round(shift * ph)
        rect(buf, px + offset, HORIZON - py, 7, py, (150, 120, 84))
        rect(buf, px - 3 + offset, HORIZON - py - 6, 13, 7, (188, 152, 104))

    # The screen-plane marker: identical in both eyes, so it is the one thing
    # that never moves however the convergence is set. Everything else is
    # judged against it.
    rect(buf, 452, HORIZON - 96, 30, 96, (198, 150, 58))
    rect(buf, 446, HORIZON - 104, 42, 10, (168, 122, 42))
    rect(buf, 458, HORIZON - 88, 18, 18, (86, 62, 20))

    return buf


def timecode_strip(buf, frame: int):
    """A marker that crosses the frame once, plus a tick every second.

    On the screen plane, so it carries no depth of its own. It is what makes a
    still from this clip identifiable: scrub anywhere and the strip says where
    you are, which is exactly what is needed to check that a preview and a
    render agree about which frame they are on.
    """
    top = H - 16
    rect(buf, 0, top, W, 16, (16, 16, 20))
    for second in range(SECONDS + 1):
        x = round(second * (W - 1) / SECONDS)
        rect(buf, x, top + 10, 2, 6, (90, 96, 110))
    x = round(frame * (W - 1) / max(FRAMES - 1, 1))
    rect(buf, x - 2, top + 2, 5, 12, (240, 240, 245))


def eye(shift: float, frame: int, base: bytearray) -> bytearray:
    """One eye of one frame.

    `shift` is half the disparity of a unit depth: negative for the left eye,
    positive for the right. An object behind the screen is drawn at `+ shift`,
    one in front at `- shift`.
    """
    buf = bytearray(base)
    t = frame / FRAMES

    # A card drifting steadily behind the screen — constant depth, so its
    # fringe never changes while the ball's does.
    card = round(shift * 0.8)
    cx = int(W * 0.12 + W * 0.7 * t)
    rect(buf, cx + card, HORIZON - 150, 54, 34, (44, 92, 176))
    rect(buf, cx + 6 + card, HORIZON - 144, 42, 22, (92, 148, 226))

    # The near ball. It crosses the frame while its depth swings from just in
    # front of the screen to far enough forward that the colour fringing
    # becomes obvious — the case the recovery cannot solve without a 2D source.
    depth = 1.0 + 3.0 * (0.5 - 0.5 * cos(2 * pi * t))
    near = round(shift * depth)
    bx = int(W * 0.18 + W * 0.62 * t)
    by = HORIZON - 40 - int(46 * abs(sin(2 * pi * t)))
    disc(buf, bx - near, by, 34, (206, 54, 44))
    disc(buf, bx - near - 11, by - 12, 12, (238, 152, 138))

    timecode_strip(buf, frame)
    return buf


def side_by_side(frame: int, left_base: bytearray, right_base: bytearray) -> bytes:
    """The pair packed into one frame, left eye first."""
    left = eye(-UNIT, frame, left_base)
    right = eye(+UNIT, frame, right_base)
    out = bytearray(W * 2 * H * 3)
    row = W * 3
    for y in range(H):
        at = y * row * 2
        out[at : at + row] = left[y * row : (y + 1) * row]
        out[at + row : at + row * 2] = right[y * row : (y + 1) * row]
    return bytes(out)


def verify(path: Path):
    """Checks the file says what this script meant it to say.

    ffmpeg reports success on plenty of things that are not what was asked for,
    and a sample nobody has checked is a sample that teaches the wrong lesson.
    """
    # By name, not by position: ffprobe returns the fields in its own order,
    # not the order they were asked for.
    out = run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height,nb_frames,avg_frame_rate,pix_fmt",
         "-of", "json", f"file:{path}"]
    )
    got = json.loads(out)["streams"][0]
    expected = {
        "width": W * 2,
        "height": H,
        "nb_frames": str(FRAMES),
        "avg_frame_rate": f"{FPS}/1",
        "pix_fmt": "yuv420p",
    }
    wrong = {k: (got.get(k), v) for k, v in expected.items() if got.get(k) != v}
    if wrong:
        for key, (actual, want) in wrong.items():
            print(f"  {key}: got {actual!r}, expected {want!r}", file=sys.stderr)
        sys.exit("  the clip is not what was asked for")
    print(
        f"  {got['width']}x{got['height']}, {got['nb_frames']} frames "
        f"at {got['avg_frame_rate']} fps, {got['pix_fmt']}"
    )


def main():
    SAMPLE.parent.mkdir(parents=True, exist_ok=True)
    print(f"drawing {FRAMES} frames…")

    left_base, right_base = backdrop(-UNIT), backdrop(+UNIT)

    # Frames go straight down a pipe. Writing 250 PNGs to disk only to have
    # ffmpeg read them back would be slower and would litter the tree.
    encoder = subprocess.Popen(
        ["ffmpeg", "-y", "-v", "error",
         "-f", "rawvideo", "-pix_fmt", "rgb24",
         "-s", f"{W * 2}x{H}", "-r", str(FPS), "-i", "-",
         "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18",
         # Closed GOPs a couple of seconds apart: seeking around a teaching
         # sample should be prompt, and it keeps the file honest for testing
         # the scrubber.
         "-g", str(FPS * 2),
         f"file:{SAMPLE}"],
        stdin=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        for frame in range(FRAMES):
            encoder.stdin.write(side_by_side(frame, left_base, right_base))
    except BrokenPipeError:
        pass
    encoder.stdin.close()
    if encoder.wait() != 0:
        sys.exit(f"ffmpeg failed:\n{encoder.stderr.read().decode().strip()}")

    verify(SAMPLE)
    print(f"\n{SAMPLE.relative_to(ROOT)}  {SAMPLE.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
