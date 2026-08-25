# Developing

Rust workspace, five crates, no build script and no code generation.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
python3 packaging/build-app.py --verify
```

`ffmpeg` on `PATH` is required: most of the media tests generate their own
fixtures with it, and the bundling step copies it into the app.

---

## Layout

```
crates/
├── ana-core/      the conversion itself. No I/O, no processes
├── ana-media/     ffmpeg discovery, probing, decode and encode
├── ana-pipeline/  a whole file: sources in, converted video out
├── ana-cli/       the ana-convert binary
└── ana-app/       the window
packaging/         icon, Info.plist, bundler
```

Roughly 12,800 lines, half of them tests.

Dependencies run one way: `core → media → pipeline → {cli, app}`.

### ana-core

Pure image maths. Every stage is a function from frames and settings to frames,
so the whole algorithm can be tested without ffmpeg or a single media file.

| Module | Does |
|---|---|
| `frame` | `FrameF32`, planar float RGB, and 8/16-bit conversion |
| `transfer` | sRGB and BT.709 curves, sign-preserving |
| `extract` | Anaglyph → per-eye signal, and the inverse. Owns `AnaglyphFormat` |
| `leak` | Cross-talk correction |
| `blur` | Separable blur: exact Gaussian small, three box passes large |
| `restore` | Recombining eye brightness with reference colour |
| `grade` | Per-eye brightness, contrast, saturation |
| `compose` | Stacking, Lanczos resize, conforming, aspect comparison |
| `packed` | Splitting side-by-side and top-and-bottom, with anamorphic |
| `params` | `ConvertParams`, trims, geometry and aspect arithmetic |
| `timecode` | Frames ↔ times |
| `pipeline` | The per-frame conversion, wired together |

### ana-media

Drives `ffmpeg` as a child process, moving raw frames over pipes rather than
linking `libav*`. The build stays identical on every platform, every codec comes
free, and the licensing is simple. The cost is a bundled binary and a process
per stream.

`Decoder::open()` delegates to `open_at()`, and `grab_frame` shares the same
`apply_seek`, so "frame N" has exactly one definition across the preview and the
render. They must never disagree.

### ana-app

The egui layer is deliberately thin. Anything with logic — preview caching,
preview-resolution scaling, view composition, drop routing, image sizing —
lives in functions that can be tested without a window.

`effective_params()` is the single place the app's own state (the 2D source's
role) is folded into `ConvertParams`, and both the preview and the render go
through it. Preview/render divergence has caused several bugs here; one
function makes it impossible.

---

## Testing

Tests are written first, and watched to fail, before the code that satisfies
them. That is not ceremony. Several times a test written against the *intent*
disagreed with an implementation that looked obviously right, and it was the
implementation that was wrong.

Three kinds carry most of the weight:

**Ground truth.** `ana-core/tests/ground_truth.rs` generates a stereo pair,
derives an anaglyph from it, recovers it, and scores the result against the
original. Real anaglyph releases have no surviving full-colour master, so this
is the only way to know whether recovery is right rather than merely plausible.
It found two real algorithm errors.

Some of its assertions are about *relationships* rather than absolute numbers —
that ColorCode's amber eye beats its blue eye by a wide margin, say. Those
survive tuning in a way that a bare threshold does not, and they fail for the
right reason when something breaks.

**Headless UI.** `ana-app/tests/preview_shape.rs` renders the real widget with
`egui_kittest` and measures the rectangle it paints. Three aspect-ratio faults
reached the user because the arithmetic was correct and the *drawing* was wrong;
nothing in a screenshot-free suite could see that. Putting the old layout back
fails three of these four tests immediately.

**Representativeness.** `ana-app/tests/preview_is_representative.rs` compares a
preview-sized conversion against the full render reduced to the same size. The
preview is only defensible if it shows what the render will produce.

Absolute figures in the ground-truth suite are *regression guards*, set below
measured values, not quality targets. They say so.

### Performance

```bash
cargo test --release -p ana-core --test profile -- --nocapture --ignored
```

Measure in release. A debug build is roughly ten times slower and will send you
optimising the wrong thing — the colour blur once looked like 92% of frame time
because it genuinely was, but only release measurements made that legible.

---

## Packaging

```bash
python3 packaging/build-app.py --verify
```

Produces `target/Stereoscopic Converter.app`, 49 MB, Apple Silicon.

The app carries its own ffmpeg. A copy that only runs where Homebrew happens to
be installed is not an app, it is a development setup — so `ffmpeg`, `ffprobe`
and the nineteen libraries they reach are copied in and their install names
rewritten to point inside the bundle. `locate()` already prefers a copy beside
the executable, which is where they land.

Signing comes last: rewriting install names invalidates a signature, and nested
code must be signed before the bundle containing it.

`--verify` checks the signature, runs the bundled ffmpeg with `PATH` emptied,
and fails if any library still refers to `/opt` or `/usr/local`.

The icon is drawn by `packaging/make-icon.py` rather than checked in, so there
is no binary asset to lose and the design can be read.

Signing is **ad-hoc**. Distribution needs an Apple Developer ID, `--options
runtime` and Apple's notary service; without that Gatekeeper quarantines the app
on any machine it did not come from. `--sign "Developer ID Application: …"`
takes an identity when you have one.

---

## Porting

Nothing outside `packaging/` is macOS-specific. The GUI is egui, the media layer
is ffmpeg over pipes, and the maths is plain Rust. Linux and Windows builds
should be a matter of writing the equivalent bundling step — `locate()` already
handles `.exe` suffixes and the absence of an execute bit.

The one Apple-only piece is the VideoToolbox codec choice, which is already just
one option among five.

---

## Conventions

- Comments explain *why*, not *what*. If a value is not arbitrary, the comment
  says what fixes it.
- Test names are sentences describing the behaviour, not the function under test.
- Assertions carry messages with the actual values. A bare `assert!(x < y)` at
  three in the morning is worth very little.
- British spelling in prose and identifiers (`colour`), except where an external
  API forces otherwise — or where the word is a trade name, as in `ColorCode`.

## Adding an anaglyph encoding

`AnaglyphFormat` owns its variants, `ALL` and `label()`, so a new one appears in
every menu and every format loop without touching the app or the CLI. What it
needs is a projection in `projections()` — the linear combination of RGB each
filter passes, with weights summing to one — and an arm in `encode_anaglyph`.

Add it to `ALL` as well. `label()`, `projections()` and `encode_anaglyph` are
exhaustive matches, so the compiler will stop you there, but `ALL` is a
hand-written array and nothing forces a new variant into it — and a format
missing from `ALL` is a format no menu ever shows. `every_format_is_offered_and_named`
guards the list against losing an entry; it cannot know about a variant nobody
added to it. `OutputLayout` carries the same pair, for the same reason:
right-eye-only output was fully implemented and tested but unreachable for
exactly this reason.
