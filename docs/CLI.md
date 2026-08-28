# Command line reference

`ana-convert` does everything the app does, without a window. It is the right
tool for converting a feature overnight, for batching a shelf of films, and for
reproducing a result exactly.

```
ana-convert <command> [options]

  render    convert a file
  probe     report what a file contains
  preset    write a preset of the default settings
```

Presets are the same JSON the app reads and writes, so a look tuned in the app
renders here unchanged.

---

## probe

```bash
ana-convert probe film.mkv
```

```
film.mkv
  size        708x276
  frame rate  29.969 fps
  frames      18165
  pixels      yuv420p (8-bit)
  pixel shape 0.88889 (not square)
  displays at 2.2802:1  (629x276 of square pixels)
    as Side by side   1416x276 displaying 4.5604:1
    as Top and bottom 708x552 displaying 1.1401:1
    as Left eye only  708x276 displaying 2.2802:1
  audio       yes
  length      10:06.13
```

The pixel shape and display lines are worth reading when a result comes out the
wrong shape: they say what the tool believes about the file, and what each
layout would produce.

---

## render

### Sources

| Flag | Meaning |
|---|---|
| `-i, --input <FILE>` | The film. Required. |
| `--source <KIND>` | `anaglyph` (default), `sbs`, `tb`, `two-files` |
| `--anamorphic` | Packed source squeezes each eye to half size |
| `--right-eye <FILE>` | The other eye, with `--source two-files` |
| `--format <MODE>` | Source anaglyph encoding: `red-cyan` (default), `green-magenta`, `red-blue`, `color-code` |
| `--colour <FILE>` | A 2D release to take colour from |
| `--mono <FILE>` | A 2D release to use verbatim as one eye |
| `--mono-eye <EYE>` | Which eye that is: `left` or `right` |
| `--audio <FILE>` | Where to copy audio from. Defaults to the input when it has any |

`--mono` implies `--colour` unless you give one explicitly: a file that *is* an
eye is also the best colour reference for the other.

### Destination

| Flag | Meaning |
|---|---|
| `-o, --out <FILE>` | Output. With `--layout separate`, the stem for `-left` and `-right` |
| `--layout <KIND>` | `sbs` (default), `tb`, `separate`, `anaglyph`, `left`, `right` |
| `--output-format <MODE>` | Anaglyph encoding to write: `red-cyan`, `green-magenta`, `red-blue`, `color-code`. Independent of `--format` |
| `--eye-order <ORDER>` | `left-first` (default) or `right-first` |
| `--swap-eyes` | Exchange the eyes before layout |
| `--size <WxH>` | Resize the finished frame. Never distorts: the display shape is preserved |
| `--codec <CODEC>` | `h264-hw` (default), `hevc-hw`, `h264`, `hevc`, `prores` |
| `--quality <0-100>` | Default 75 |

### Range

Positions take a time (`3:15`, `1:02:05`, `90`) or an exact frame with a
trailing `f` (`5850f`). A bare number is seconds.

| Flag | Meaning |
|---|---|
| `--start`, `--end` | Range within the main source. `--end` is inclusive |
| `--colour-start`, `--colour-end` | The same moment in the colour source |
| `--mono-start`, `--mono-end` | The same moment in the 2D eye source |

Setting `--start` and `--mono-start` to the same visual moment is what keeps two
differently edited releases in step. Sources are seeked, not decoded and
discarded, so starting an hour in costs nothing.

### Tuning

| Flag | Meaning |
|---|---|
| `--decimate-horiz <PERCENT>` | Horizontal colour blur. Lower blurs harder. Default 5 |
| `--decimate-vert <PERCENT>` | Vertical colour blur. Default 20 |
| `--leak-left`, `--leak-right <PERCENT>` | Cross-talk correction, `-100`..`100` |
| `--convergence <PERCENT>` | Horizontal convergence, `-10`..`10`. Narrows the output by the same percentage |
| `--preset <FILE>` | Load settings first; flags below override |
| `--save-preset <FILE>` | Write the settings actually used |

### Other

| Flag | Meaning |
|---|---|
| `--ffmpeg-dir <DIR>` | Use a specific ffmpeg. Errors rather than falling back |

---

## Examples

**A straightforward anaglyph to side-by-side.**

```bash
ana-convert render -i comin-at-ya.mkv -o comin-at-ya-stereo.mkv
```

**Using a 2D release as the left eye** — the best quality available. The left
eye passes through untouched; the right is rebuilt using the same file for
colour.

```bash
ana-convert render -i film-3d.mkv -o film-stereo.mkv \
  --mono film-2d.mkv --mono-eye left \
  --start 3:15 --mono-start 3:22
```

**Tuning a section, then rendering the feature with the same look.**

```bash
ana-convert render -i film.mkv -o test.mkv --start 20:00 --end 20:30 \
  --decimate-horiz 3 --leak-right 8 --save-preset film.json

ana-convert render -i film.mkv -o film-stereo.mkv --preset film.json
```

**A side-by-side file back to anaglyph, in green/magenta.**

```bash
ana-convert render -i sbs.mkv -o glasses.mkv \
  --source sbs --layout anaglyph --output-format green-magenta
```

**Easing a punishing shot.** Bringing the eyes together moves the plane of zero
parallax onto something nearer, so the thrown spear sits at the screen instead
of in the viewer's lap.

```bash
ana-convert render -i comin-at-ya.mkv -o comfortable.mkv \
  --start 42:10 --end 42:35 --convergence -2.5
```

**One eye out of a half-width broadcast file, as flat 2D.**

```bash
ana-convert render -i broadcast-3d.ts -o flat.mkv \
  --source sbs --anamorphic --layout left
```

**Two per-eye files into a single side-by-side.**

```bash
ana-convert render -i left.mkv --right-eye right.mkv --source two-files \
  -o paired.mkv --layout sbs
```

**A ProRes master to grade later.**

```bash
ana-convert render -i film.mkv -o master.mov --codec prores --layout separate
```

---

## Behaviour worth knowing

**Progress** is written to stderr and rewritten on one line, so piping stdout
stays clean.

**Ctrl-C** stops after the current frame, leaving a playable partial file and
exiting with status `130` — so a script checking the exit code will not mistake
a cancelled run for a finished one. A second Ctrl-C aborts immediately.

**Frame rate** is taken from the source rather than assumed, or the audio drifts
across a feature.

**Mismatched sources** are resized to the main source's geometry rather than
refused — a 2D release at another resolution is entirely normal. If the *shape*
differs you get a note, because resizing will stretch it.

**Exit codes**: `0` success, `1` failure, `130` cancelled.
