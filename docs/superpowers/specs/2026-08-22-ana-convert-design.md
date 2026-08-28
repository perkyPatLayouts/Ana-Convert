# Stereoscopic Converter — design notes

Why the algorithm works the way it does, and the bugs that shaped it. For using
the program see the [User Guide](../../USER-GUIDE.md); for working on it, see
[Developing](../../DEVELOPING.md).

Anaglyph 3D → full-colour stereo video. Apple Silicon first, portable by construction.

Reimplements the AviSynth `AnaExtract.avs` algorithm published at
<https://vrtifacts.com/dump-those-silly-colored-3d-glassess/> (GPLv3),
reworked to run in 32-bit float with a live preview tuning loop.

## Settled decisions

| Decision | Choice |
|---|---|
| Stack | Rust workspace, `egui`/`eframe` GUI — recompiles for Linux/Windows |
| Media I/O | Bundled `ffmpeg` binary driven over pipes |
| Numerics | 32-bit float, linear light, true Gaussian blur, full-res 4:4:4 |
| Licence | GPL-3.0-or-later, inherited from the original |

## The algorithm

Per frame, from anaglyph `A`, colour reference `C` (the 2D release if one exists, else `A`),
and an optional 2D mono frame `M`:

1. **Linearise** `A` and `C` (sRGB or BT.709 EOTF). Blur and colour restoration are physical
   operations on light and belong in linear space.
2. **Extract** each eye's surviving signal by its [`EyeProjection`] — the linear combination of
   RGB that eye's filter passes:
   - red/cyan: left `R`, right `G` (blue skipped; the original found it noisy)
   - green/magenta: left `G`, right `0.746·R + 0.254·B`
   - red/blue: left `R`, right `B`
   - ColorCode: left `0.229·R + 0.771·G`, right `B`

   Weights always sum to 1, so a neutral grey of value `v` projects to `v`.
3. **Cross-talk correction** — `L -= k_L·R`, clamp, rescale by `1/(1-k_L)`; then the same for `R`
   against the *already corrected* `L`. **Runs in gamma space**, even when the rest of the
   pipeline is linear (see "Two corrections" below).
4. **De-fringe** — optional horizontal Gaussian per eye, for peaking artefacts on DVD transfers.
5. **Blur the colour reference** — separable Gaussian, strong horizontally (disparity is
   horizontal), weak vertically. Parameters stay in the original's `decimate` percentages,
   mapped to `sigma = (100/decimate - 1) / 2`.
6. **Restore colour** — drive `C` until its own projected value matches the eye's:
   - `Scale` (default): `out = C · signal / P(C)` — preserves chromaticity exactly.
   - `Offset`: `out = C + (signal - P(C))` — preserves colour differences.

   Both are *exact* when `C` is accurate. A `SHADOW_FLOOR` lift on both sides keeps the divide
   finite and decays to neutral grey as the reference darkens.
7. **De-linearise**, then **grade** per eye (contrast about mid-grey, then brightness, then
   saturation) — in gamma space, where the original's parameters were dialled in.
8. **Mono substitution** — a supplied 2D eye replaces its side entirely, bypassing the grade,
   which exists to bring the *recovered* eye into line with it.
9. **Swap / compose / resize** — SBS, TB, or two streams; optional Lanczos-3 resize.

## Two corrections the ground-truth test caught

Both were invisible to the original implementation, which had no reference to score against.

**Projection consistency.** Extraction yields a *channel*, not a luminance. Treating an RC left
eye's red channel as its luminance renders a saturated red box about four times too bright — the
recovered image scored *worse* than the raw anaglyph (15.0 dB vs a 21.5 dB baseline). Extraction
and restoration must share one projection. Fixing this took the left eye to 20.5 dB and luma from
21.6 to 30.4 dB. The original AviSynth carries the same flaw via `MergeChroma`.

**Cross-talk space.** Ghosting in a release is baked in during mastering as arithmetic on
gamma-encoded channels. A gamma-domain mix is not linear, so subtracting it in linear light cannot
invert it: on a synthetic leaky master, correcting in linear made the result 4 dB *worse* than
leaving the leak alone. Correcting in gamma recovers essentially all of it (25.4 → 30.5 dB, against
30.4 dB for a clean source).

## Architecture

```
crates/
├── ana-core/     pure image maths — no I/O, no ffmpeg          [M1: done]
├── ana-media/    ffmpeg discovery, probe, decode/encode pipes   [M2: done]
├── ana-pipeline/ orchestration, progress, cancellation          [M3]
├── ana-cli/      headless render; integration-test driver       [M3]
└── ana-app/      egui GUI with live preview                     [M4]
```

`ana-core` is I/O-free: every stage is `fn(&FrameF32, params) -> FrameF32`, testable without
ffmpeg or media files. `frame` · `transfer` · `extract` · `leak` · `blur` · `restore` · `grade` ·
`compose` · `params` · `pipeline`.

## Verification

- `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt --check`.
- `tests/ground_truth.rs` generates a stereo pair, derives the anaglyph from it, recovers, and
  scores against the original. Current figures on a deliberately adversarial scene (hard-edged
  saturated blocks at up to 8px disparity, far harsher than film):

  | Case | Result |
  |---|---|
  | Perfect reference, no colour blur | 66.3 dB (RC), 66.6 dB (GM) |
  | Default settings, 2D reference | 20.5 dB left, 19.1 dB right |
  | Right-eye luma, default settings | 30.4 dB |
  | Colour blur trade-off (5% / 25% / off) | 18.2 / 26.2 / 66.3 dB |
  | Leaky master, corrected | 25.4 → 30.5 dB |

  Absolute bars in the suite are regression guards set below these, not quality targets.
- `ANA_DUMP=<dir> cargo test --test ground_truth` writes PPMs for eyeballing.
- Per milestone, a real anaglyph clip rendered and viewed on the stereo display — the only test
  that catches perceptual problems.

## What a real film taught us

First contact with an actual release — *Comin' At Ya!* (1981), 708x276 red/cyan, no 2D version
available — changed two decisions that every synthetic test had got wrong.

**`Offset` is the default, not `Scale`.** Scale broke every dark area into cyan speckle. The cause
is measurable: in that film's shadows the red channel averages 7/255 while green and blue sit at 22
and 28. Scale divides by the reference's projected value — the near-zero red channel — so green and
blue get multiplied by a large, noisy ratio and dark pixels come back as bright cyan dots. Offset
never divides, so it degrades to "keep the reference colour" and is visibly clean.

Scale still scores ~3 dB *higher* on the synthetic scene, and that is the point: synthetic PSNR was
the wrong judge. The adversarial test scene has no grain, no compression and no cyan-cast shadows,
so it could not see the failure that matters.

**Raising `SHADOW_FLOOR` does not fix it.** The obvious theory was that the divide guard (1e-4) sat
below 8-bit quantisation noise (one code near black is 3e-4 in linear light). Raising it to eight
code values was tried against the film and changed nothing visible, while costing Scale 27 dB of
precision on clean sources. It was reverted. The amplification comes from the ratio between a pixel's
unblurred signal and a *blurred neighbourhood* reference, which stays large well above any floor.

**Self-colouring cannot fix high-disparity colour, at any blur setting.** Where the two eyes are far
apart, the anaglyph's colour at a pixel is composed from two different points in the scene. Blur
smears that error rather than resolving it, and no value of `decimate` helped. Cross-talk correction
does not help either — it is for ghosting, and at 30% it simply crushed the highlights to black.
This is the limitation the original post addresses by asking for the 2D release, and it is the
strongest argument for the mono/colour-reference path.

## Aligning two releases

A 2D release and the anaglyph it accompanies rarely start on the same frame, and may not run the
same length. Every source therefore carries its own `SourceTrim { start, end }`, and the anaglyph's
range is the timeline: it decides the output length, and the others are read in step with it.
`end` is inclusive, because it names a frame someone looked at and marked.

This replaces the old `mono_frame_offset`, which could only express a constant shift and could not
bound the range. An offset is now just `mono_trim.start - anaglyph_trim.start`.

Sources are *seeked* to their start rather than decoded and discarded — a trim beginning three
minutes in returns in under two seconds instead of grinding through 5,850 frames. `Decoder::open()`
delegates to `open_at()`, and `grab_frame` shares the same `apply_seek`, so "frame N" has exactly
one definition across the preview and the render.

In the app, "Align to anaglyph…" shows the two sources side by side with independent scrubbers and
a "mark this as the start of both" button. On the command line, `--start`, `--end`, `--mono-start`
and `--mono-end` take either a time (`3:15`) or an explicit frame (`5850f`).

### The frame rate trap this uncovered

Verifying a trimmed render against a pre-cut ground truth scored only 23.7 dB where identical
frames were expected. The seek was landing **5.8 frames early**, because `probe` preferred
ffprobe's `r_frame_rate`.

For a real disc rip, `r_frame_rate` reported a round `30/1` while the file actually runs at
29.9687. `r_frame_rate` is the *lowest rate that can represent every timestamp*, not the rate the
film runs at. `avg_frame_rate` is frames over duration, which is exactly the frame-to-time mapping
seeking needs. Preferring it took the same comparison to **47.1 dB** — identical bar the re-encode.

The same error was quietly affecting two other things: scrubbing the preview showed a frame six
early on that film, and encoding at 30 instead of 29.9687 would drift the audio by 0.6 seconds
across a feature.

## Working in both directions

The tool is no longer anaglyph-in only. `InputMode` says what the source holds:

* `Anaglyph` — recover a stereo pair, the original job.
* `Packed { packing, order, anamorphic }` — the frame already *is* a stereo pair, side by side or
  top and bottom, so nothing needs recovering. The two eyes are simply taken apart, and the
  recovery settings do not apply.

`anamorphic` handles the squeeze that lets a pair share one ordinary frame: a 1920x1080 file holding
two 960x1080 eyes, each of which represents a full 1920x1080 picture. Split naively everyone comes
out half as wide as they should be, so each eye is stretched back. Full-resolution packing (a
3840x1080 frame holding two 1920x1080 eyes) needs no stretch, which is the default.

`OutputLayout` gained three destinations to match: `Anaglyph` re-muxes a pair for the old glasses,
and `LeftOnly` / `RightOnly` write a single eye as a flat 2D file. Combined with the input modes
that covers the useful trips — anaglyph to stereo, stereo to anaglyph, stereo to one eye, and
repacking side-by-side as top-and-bottom.

Geometry for all of this lives in one place, `ConvertParams::output_geometry`, and a test asserts it
agrees with what conversion actually produces for every input mode and layout. The pipeline sizes
its encoders from that figure before decoding a single frame, so a disagreement would shear every
frame of a render.

## Pixel shape

Video pixels are not always square. The real transfer used for testing stores 708x276 with an 8:9
pixel aspect, so it is meant to be seen at 2.28:1 rather than the 2.57:1 its stored dimensions
imply. Raw video carries no such metadata, so decoding to `rgb24` drops it and encoding from raw
puts square pixels back — every non-square source came out stretched by 12.5%.

The shape is now read at probe time (`VideoInfo::sample_aspect`) and restored at encode time via
ffmpeg's `-aspect`. What it should be restored *to* depends on the layout, since stacking two eyes
changes the shape of the frame:

| | display aspect |
|---|---|
| one eye, anaglyph, or two separate files | eye shape |
| side by side | eye shape × 2 |
| top and bottom | eye shape ÷ 2 |

and the eye's own shape depends on the input: the whole frame for an anaglyph or a squeezed
anamorphic pair, half the frame's width for a full side-by-side pair, twice its height for a full
top-and-bottom one. `ConvertParams::output_display_aspect` holds all of that, and the preview pane
draws through the same figure — tuning against a stretched picture would be tuning against the
wrong picture.

An explicit output size changes how many pixels are stored, not what the picture looks like:
rendering that 2.28:1 source at 1920x1080 gives a 1920x1080 file that still displays at 2.28:1. A
resize should never distort.

### Why this took three attempts

The arithmetic was right after the first fix, and the rendered files were correct. What stayed wrong
was the *preview*, because the pane used `centered_and_justified` — which justifies, meaning it
stretches its child to fill the space, silently discarding the size it was handed. The picture
reverted to its texture's own shape, which for a 1280x576 side-by-side frame is 2.22:1 rather than
the 3.56:1 it should be seen at.

Nothing in the suite could see it: the sizing function was correct and tested, and the fault lived
in the drawing. `egui_kittest` now renders the real widget headlessly and measures the rectangle it
paints, so the shape on screen is checked rather than the shape intended. Putting the old layout
back fails three of those four tests immediately.

One wart: ffmpeg derives the stored pixel ratio from a decimal, so an exact 8:9 source comes back as
5612:6313 — the same shape to four decimal places, but not the same rational. Carrying the ratio
through as integers would fix it and has not been worth the plumbing.

## Performance

Measured on an M-series Mac (6 performance + 4 efficiency cores), 1080p, release build.
Reproduce with `cargo test --release -p ana-core --test profile -- --nocapture --ignored`.

| Stage | Time | Rate |
|---|---|---|
| Full `process_frame`, default settings | 56 ms | **17.8 fps** |
| — colour blur | 28 ms | |
| — everything else | 26 ms | |

Two findings worth keeping:

* **The colour blur was 92% of frame time** (311 ms of 337 ms) as a direct Gaussian convolution:
  the default sigma of 9.5 needs 59 taps per pixel. Replacing it with three box passes made it
  O(1) per pixel — 311 ms → 28 ms — and, more usefully, made the cost *independent of sigma*, so
  dragging a blur slider in the preview cannot change the frame rate. Measured spread stays within
  0.1% of the requested sigma at the default setting. Below sigma 4 the exact kernel is still used,
  where box passes approximate poorly and exactness is nearly free.
* **The remaining per-pixel work is memory-bandwidth bound, not compute bound.** Parallelising it
  across rayon measured no faster and was reverted. `process_frame` touches roughly fifteen
  full-frame buffers (25 MB each at 1080p); the next real gain is fusing passes to cut allocations,
  not adding threads. Not worth doing until something demands it.

## The 2D source is one thing, not two

A 2D release of the same film can serve two purposes, and they are not alternatives so much as
degrees of knowledge:

* **Colour reference only** — both eyes are still recovered from the anaglyph, and the file supplies
  only hue. This is what removes the cast, because the anaglyph's own colours are a blend of two
  views and are wrong wherever those views disagree, which is exactly where the depth is.
* **It *is* the left/right eye** — that eye is not recovered at all. It passes straight through,
  perfect, and only the other eye is reconstructed, using this same file for its colour.

The second is strictly better and is the best result the method can give. So the app offers one 2D
source slot with a role rather than two slots, and when the role names an eye the file is handed to
the renderer as both `colour` and `mono`. The CLI does the same: `--mono` implies `--colour` unless
one is given explicitly.

The role lives in the app rather than in `ConvertParams`, so it has to be folded in — and it is
folded in for the preview and the render through one `effective_params()`, because the alternative
is the two quietly disagreeing about what they are showing. That has happened enough times already.

## Three ways in, six ways out

`InputMode` says what the source holds — an anaglyph to recover, a packed pair to
split, or two files that are already the eyes — and `OutputLayout` says what to
write. Every combination works, including repacking side-by-side as
top-and-bottom, and muxing a modern per-eye pair back into something the old
glasses can watch.

All three input modes end with the same steps: grade, substitute a 2D eye if one
was given, swap if asked. Those live in one `finish_pair` rather than being
written out three times, because three copies is how they drift apart.

Adding the third mode is also what forced `Sources.anaglyph` to be renamed
`primary`. It had stopped being only an anaglyph when packed sources arrived; a
third meaning would have made it actively misleading.

### A menu constant that hid a finished feature

`OutputLayout::ALL` drives the destination menu, and it listed five of the six
layouts. Right-eye-only output was implemented, tested and reachable from the
command line, and simply could not be chosen in the app. There is now a test
asserting that every layout which exists appears in `ALL` — the sort of thing
that feels redundant right up until it is not.

## The theme that never applied

The app looked washed out, and the reason was not the palette. `set_visuals`
dresses only the theme currently in use; on a Mac set to Light the app fell back
to egui's default light palette, and section headings tuned as light cyan for a
dark background landed on near-white.

It now pins the preference to Dark *and* sets visuals for both themes, so there
is nothing to fall back to. Dark is also simply correct here: a picture being
judged wants a neutral dark surround, which is why every video tool has one.

egui's default text sizes are small for a desktop app read at arm's length —
body and button at 13px, and the "small" style used for every note and warning
at **9px**. All raised.

## ColorCode, and why it needed its own format

The original post listed ColorCode 3-D as unimplemented and "brutal". It is not brutal so much as
lopsided, and the lopsidedness is the point.

ColorCode is amber and blue. An amber filter passes red *and* green, so the left eye's projection is
a luminance-weighted mix of the two — `0.229·R + 0.771·G`, renormalised like magenta's — rather
than red alone as in red/blue. The right eye gets blue by itself. The *encodings* of red/blue and
ColorCode coincide; the extractions do not, and extraction is what recovery depends on.

Amber carries 93% of white's luminance and blue about 7%, so one eye arrives nearly complete and the
other supplies little more than parallax. On the synthetic scene the amber eye recovers to 27.3 dB
and the blue eye to 16.1. With a perfect unblurred reference it reaches 144 dB, which says the
projection is exact and everything below that is the format rather than the arithmetic.

The ground-truth test therefore asserts the *gap* between the eyes as well as the quality. The gap
is what characterises the format; if it ever narrowed, something would be wrong with the amber
projection.

One practical consequence: a 2D release used as the **right** eye replaces the weak one outright,
which matters more here than in any other encoding.

`AnaglyphFormat` now owns `ALL` and `label()`. The app previously kept its own list of three
formats — exactly the arrangement that had already let right-eye-only output go missing from the
destination menu — so a fourth would have compiled cleanly and silently failed to appear.

## The Mac app

`python3 packaging/build-app.py --verify` produces `target/Ana-Convert.app` — 49 MB, Apple Silicon.

The app carries its own ffmpeg. A copy that only runs where Homebrew happens to be installed is not
an app, it is a development setup, so `ffmpeg`, `ffprobe` and the nineteen libraries they reach are
copied into the bundle and their install names rewritten to point inside it. `locate()` already
preferred a copy sitting beside the executable, which is exactly where they land.

Signing comes last, because rewriting install names invalidates a signature, and nested code has to
be signed before the bundle containing it.

`--verify` proves the claim rather than asserting it: it checks the signature, runs the bundled
ffmpeg with an emptied `PATH`, and fails if any library still refers to `/opt` or `/usr/local`.
`ana-convert-app --check` reports which tools the app resolved and whether they run — worth having
because "it does nothing" and "it cannot find its tools" look identical from outside a bundle.

The icon is drawn by `packaging/make-icon.py` rather than checked in, so there is no binary asset to
lose and the design can be read.

Signing is ad-hoc. Real distribution needs an Apple Developer ID, `--options runtime`, and
submission to Apple's notary service; without that, Gatekeeper will quarantine the app on any
machine it did not come from.

## Convergence

Recovery gets the geometry the master had, which is not always the geometry
anyone should watch. Convergence shifts the eyes horizontally against each
other, which moves every object's parallax by the same amount and so relocates
the plane of zero parallax — the depth the viewer reads as the screen.

It reduces to two crops at different offsets: the eye that moves left keeps its
right-hand part, and vice versa. Nothing is resampled, so it costs no sharpness,
and both eyes stay identically sized. The percentage names the total separation,
which makes it also exactly the width given up, since only the overlap can be
kept.

It runs at the end of `finish_pair`, after the eye swap — so "right eye" means
the one the viewer's right eye sees rather than whichever the file held — and
before layout, so one insertion covers every input mode and every output.

Two things had to be got right. The display aspect must narrow with the crop or
every output stretches, so it is derived from the same integer widths as the
geometry rather than recomputed from the percentage. And the anaglyph path did
not call `finish_pair` at all: it repeated grading, substitution and swapping
inline, so the first implementation converged every input mode except the
commonest one. A test through the public entry point caught it; a test of the
helper had passed.

## Not in v1

Interlaced output, GPU compute, batch queues, a legacy `.avs` parameter importer,
disparity-aware colour warping, Linux/Windows builds.
