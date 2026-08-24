# Stereoscopic Converter

Recovers full-colour stereo video from anaglyph 3D, and converts between every
common stereo layout. macOS on Apple Silicon, with a live preview.

Anaglyph throws most of each eye away. The red channel carries one eye and the
cyan channels the other, so each eye survives as brightness alone and the colour
you see is a blend of both. This pulls a real stereo pair back out: each eye's
brightness comes from the channels that carried it, and colour is painted back
on from a heavily blurred reference.

It also works in the other direction — a side-by-side file back to anaglyph, a
single eye out as flat 2D, one layout repacked as another.

It reimplements the AviSynth `AnaExtract.avs` scripts published at
[vrtifacts.com](https://vrtifacts.com/dump-those-silly-colored-3d-glassess/),
reworked to run in 32-bit float with a preview you can tune against.

---

## What it converts

| From | To |
|---|---|
| Anaglyph (red/cyan, green/magenta, red/blue) | side-by-side, top-and-bottom, two files, one eye, anaglyph |
| Side-by-side or top-and-bottom, full or anamorphic | any of the above |
| Two files, one per eye | any of the above |

Audio is stream-copied from the source, never re-encoded.

## Getting it

```bash
python3 packaging/build-app.py --verify
open "target/Stereoscopic Converter.app"
```

The app carries its own ffmpeg — 49 MB, nothing to install. `--verify` proves it,
by checking the signature and running the bundled ffmpeg with `PATH` emptied.

Building needs Rust and, for the bundling step only, a Homebrew ffmpeg to copy
from (`brew install ffmpeg`).

## Using it

Open a film, or drop one on the window. Tell it what the file holds, scrub to a
frame worth judging, and move the sliders — the preview shows the conversion
itself, not an approximation of it. Then choose a destination and convert.

The [User Guide](docs/USER-GUIDE.md) covers every control and how to tune them.
There is also a Help button in the app.

For batch work there is a command line:

```bash
ana-convert render -i film.mkv -o stereo.mkv
```

See the [CLI reference](docs/CLI.md). Presets are the same JSON in both, so a
look tuned in the app renders headlessly.

## The single biggest improvement

If the disc carries a 2D version of the film, use it. The anaglyph's own colours
are a blend of two views and are wrong wherever those views disagree — which is
exactly where the depth is. A 2D transfer fixes that, and if it *is* one of the
eyes then that eye needs no reconstruction at all: it passes straight through,
perfect, and only the other is rebuilt.

## What it cannot do

High-disparity shots — something thrown at the camera — keep their colour
fringing when there is no 2D reference, at any setting. Where the two eyes are
far apart the anaglyph's colour at a pixel is composed from two different points
in the scene; blur smears that error rather than resolving it. This is a limit
of the method, not of the implementation.

ColorCode anaglyph is not supported. Neither is interlaced output.

## Documentation

- **[User Guide](docs/USER-GUIDE.md)** — the workflow, every control, and how to tune
- **[CLI reference](docs/CLI.md)** — commands, flags, worked examples
- **[Developing](docs/DEVELOPING.md)** — architecture, building, testing, packaging
- **[Design notes](docs/superpowers/specs/2026-08-22-ana-convert-design.md)** — why the algorithm works as it does, and the bugs that shaped it

## Licence

GPL-3.0-or-later, inherited from the original AviSynth scripts. The app bundles
FFmpeg, also under the GPL. See [COPYING](COPYING).
