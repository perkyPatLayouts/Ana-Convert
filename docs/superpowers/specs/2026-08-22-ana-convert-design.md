# Ana-Convert — design

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

## Not in v1

ColorCode anaglyph, interlaced output, GPU compute, batch queues, a legacy `.avs` parameter
importer, disparity-aware colour warping, Linux/Windows builds.
