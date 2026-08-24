# User Guide

How to convert a film, and what every control does.

- [The short version](#the-short-version)
- [Source: what your file holds](#source-what-your-file-holds)
- [2D Source: the biggest improvement available](#2d-source-the-biggest-improvement-available)
- [Recovery: pulling the eyes apart](#recovery-pulling-the-eyes-apart)
- [Grade](#grade)
- [Destination](#destination)
- [Preview](#preview)
- [Range and alignment](#range-and-alignment)
- [Presets](#presets)
- [Troubleshooting](#troubleshooting)

---

## The short version

1. Open a film, or drop it on the window.
2. Set **Source → This file holds** to match what it actually is.
3. If you have a 2D version of the same film, open it too and say what it is.
4. Scrub to a frame worth judging — a face, a shot with real depth.
5. Adjust the sliders, watching the preview.
6. Choose a **Destination** and press Convert.

Most films need nothing beyond steps 1–2 and 5–6. The defaults are the values
the original AviSynth post recommends as a starting point.

---

## Source: what your file holds

This must match reality or nothing else will make sense.

**An anaglyph** — one image with the two eyes encoded into colour channels.
Choose the colour mode: red/cyan is much the most common, then green/magenta,
then red/blue. Recovery settings apply.

**A side-by-side pair** or **a top-and-bottom pair** — already stereo. Nothing
needs recovering; the two eyes are simply taken apart, and the Recovery section
disappears because none of it applies.

**One eye, with the other in a second file** — two separate per-eye files. Open
the left with *Open Left Eye…* and the right with *Open Right Eye…*. If yours
are the other way round, tick **Swap eyes** rather than reopening them.

### Anamorphic

Only for packed pairs, and it matters.

Broadcast and disc stereo usually squeeze each eye to half size so the pair fits
one ordinary frame: a 1920×1080 file holding two 960×1080 eyes, each meant to be
seen at the full 1920×1080. **Tick anamorphic** and each eye is stretched back.

Full-resolution packing — a 3840×1080 frame holding two 1920×1080 eyes — needs
no stretch. **Leave it clear.**

If you get it wrong you will see it immediately: people come out **too narrow**
if it wanted ticking, **too wide** if it did not.

### Transfer

Which gamma curve the file uses. sRGB is right almost always. BT.709 suits some
broadcast masters. Linear is for material already in linear light, which is
rare. If unsure, leave it — the difference is subtle and sRGB is the safe guess.

---

## 2D Source: the biggest improvement available

Optional, and worth going to some trouble for.

The anaglyph's own colours are a blend of two views, and are wrong wherever
those views disagree — which is exactly where the depth is. A 2D transfer of the
same film fixes that. Many 3D discs carry one.

Once opened, say what it is:

**Colour reference only.** Both eyes are still recovered from the anaglyph; this
file supplies only hue. Use this when you have a 2D version but do not know
which eye it corresponds to, or it is a centre view.

**This is the left eye** / **This is the right eye.** That eye is not recovered
at all. It passes straight through, untouched and perfect, and only the other
eye is reconstructed — using this same file for its colour. This is the best
result the method can give.

The two files must line up. See [Range and alignment](#range-and-alignment).

> Not used with two per-eye files: both eyes are already complete, so there is
> no colour to supply and no eye to stand in for. The button is greyed out.

---

## Recovery: pulling the eyes apart

Only shown for anaglyph sources.

### Colour blur

Each eye survives as brightness; colour has to come from somewhere else, blurred
so it covers the horizontal offset between the two views.

**Lower percentages blur harder.** Horizontal wants far more than vertical,
because that is the direction the eyes are displaced in — vertical blur only
helps with cameras that were misaligned.

- Too little: colour fringes survive around objects at depth.
- Too much: colour bleeds across edges.

Defaults are 5% horizontal, 20% vertical. Note these are **pixel radii**, not
fractions of frame width, so the same setting is proportionally gentler on a
1080p transfer than on a DVD-sized one.

### Reconstruction

**Offset** is the default and the right choice for real film. It never divides,
so noisy shadows stay clean.

**Scale** is sharper on a clean source and preserves colour more exactly, but it
divides by the reference's brightness — and a red/cyan anaglyph's shadows are
exactly where that approaches zero. On grainy or heavily compressed film it
breaks dark areas into cyan speckle. It scores several dB *higher* on synthetic
tests, which is precisely why it is not the default: those tests have no grain.

### Ghosting

Removes each eye's ghost from the other, for cross-talk baked in during
mastering. Percentages, both directions independently.

It will **not** fix fringing caused by disparity. If raising it darkens the
picture rather than cleaning it, what you are looking at is disparity and this
is the wrong tool. On a well-made BluRay you may need none at all.

### De-fringe

Softens the white edges that excessive sharpening leaves on DVD-era transfers.
`1.0` is off. Try 1.5–4.0 if you see thin bright outlines at colour boundaries.

---

## Grade

Per-eye brightness, contrast and saturation. Recovery routinely leaves one eye
darker or flatter than the other — the two anaglyph channels never carried equal
energy — so each eye is corrected separately.

A 2D source used as an eye passes through **ungraded**. That is deliberate: the
grade exists to bring the *recovered* eye into line with the perfect one.

---

## Destination

| Layout | Result |
|---|---|
| Side by side | One file, twice as wide. What most 3D displays and headsets expect. |
| Top and bottom | One file, twice as tall. |
| Two files, one per eye | `-left` and `-right` added to the name you choose. |
| Anaglyph | Muxed back for ordinary screens and the old glasses. |
| Left eye only / Right eye only | A flat 2D file. |

**Anaglyph colour mode** is independent of the source's. Recovering a red/cyan
transfer and writing green/magenta is perfectly reasonable — green/magenta holds
colour better and ghosts less on many screens.

**Eye order** says which eye is written first, labelled by position: *on the
left* for side-by-side, *on top* for top-and-bottom.

**Codec.** Hardware H.264 or HEVC are fast on Apple Silicon and right for
watching. Software versions are slower but byte-identical on any machine, which
matters if you need reproducibility. ProRes HQ is for keeping a master to grade
or re-encode later.

Pixel shape is carried through from the source, so a transfer with non-square
pixels comes out the right shape rather than stretched.

---

## Preview

The preview shows the conversion itself, not an approximation, and honours the
source's pixel shape. It is drawn at reduced resolution for speed, with the blur
settings rescaled to match, so what you tune against is what you get.

| View | Shows |
|---|---|
| Left / Right | One recovered eye on its own |
| Side by side | Both, as the output will be packed |
| Anaglyph | The result re-encoded, to check through the glasses on your desk |
| Difference | Where the two eyes disagree — this is the depth |

The readout under the buttons gives milliseconds per frame and the preview
scale, which is a fair guide to how long a full render will take.

---

## Range and alignment

Every source has its own in and out points. The main source's range decides the
length of the output; the others are read in step with it.

A 2D release and the anaglyph rarely start on the same frame — different
distributors, different logos, different credit rolls. **Align sources…** shows
both side by side with independent scrubbers:

1. Scrub each to the same visual moment. A cut is easiest to match on.
2. **Mark in on both.**
3. Scrub to where you want to finish and **Mark out on both**.

The smaller buttons set one side only, for nudging without disturbing a good
mark on the other. A live offset readout shows how far apart the two positions
are. Both ranges are shown underneath as timecodes.

Times are also accepted on the command line: `--start 3:15`, or `--start 5850f`
for an exact frame.

---

## Presets

**Save preset…** writes every setting as JSON. **Load preset…** brings it back.
Keep one per film, the way the original AviSynth workflow kept a `.avs` file per
movie.

The command line reads and writes the same files, so a look tuned in the app can
be rendered headlessly — useful for converting a feature overnight.

---

## Troubleshooting

**Everyone is too narrow, or too wide.** The anamorphic setting is wrong for
your file. Too narrow means it wanted ticking; too wide means it did not.

**Dark areas break into coloured speckle.** Reconstruction is set to Scale on
grainy material. Use Offset.

**Colour fringes survive around objects that stick out.** Either lower the
horizontal blur percentage, or — better — find a 2D version of the film. Where
the eyes are far apart no blur setting can fix it, because the anaglyph's colour
there is composed from two different points in the scene.

**Raising Ghosting darkens the picture instead of cleaning it.** That fringing
is disparity, not cross-talk. Put Ghosting back to zero.

**The 2D source is a different resolution.** That is fine and normal; it is
resized to fit. If it is a different *shape* you will be told, because resizing
will stretch it — a 16:9 transfer against a scope-cropped anaglyph needs cropping
to match before it is much use.

**"has no audio stream to copy".** The file named for audio has no audio track.
Leave the audio source unset for a silent film.

**The app cannot find ffmpeg.** Only possible in a development build. Run
`ana-convert-app --check` to see what it resolved. The bundled `.app` carries
its own and cannot hit this.
