# Developing

Rust workspace, five crates, no build script and no code generation.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
python3 packaging/build-app.py --verify --dmg
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
| `compose` | Stacking, Lanczos resize, conforming, aspect comparison, convergence |
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

`--verify` checks the signature, runs the bundled ffmpeg *and the app itself*
with `PATH` emptied, fails if any library still refers to `/opt` or
`/usr/local`, and proves that `DYLD_INSERT_LIBRARIES` cannot load code into the
app.

The ffmpeg that gets vendored is whatever is on `PATH`, so the build refuses to
proceed unless it reports the version in `FFMPEG_VERSION` — otherwise a release
ships whichever ffmpeg Homebrew happened to have that morning, signed by us, and
afterwards there is no way to tell which one it was. `target/vendored-ffmpeg.txt`
records the SHA-256 of every third-party binary that went in.

**When ffmpeg publishes a security release**, that is a release of this app too.
The bundle is the only ffmpeg its users have and nothing updates it for them:
bump `FFMPEG_VERSION`, the version in [DOWNLOAD.md](DOWNLOAD.md), and cut a new
image.

The icon is drawn by `packaging/make-icon.py` rather than checked in, so there
is no binary asset to lose and the design can be read.

The sample clip in the user guide is drawn the same way, by
`docs/make-sample.py` — a ten-second side-by-side scene with known depths, built
so that a ridge sits behind the screen, a post exactly on it, and a ball far
enough in front to fringe badly. Known depths are the point: what Convergence
does to it can be checked against what it should do. The script verifies its own
output rather than trusting ffmpeg's exit code, and
`crates/ana-pipeline/tests/sample_clip.rs` holds the committed file to the shape
the guide describes — including that its two halves actually differ, since a
stereo sample whose eyes match is a 2D sample.

The figures in the user guide are generated the same way, by
`docs/make-figures.py`, which builds a synthetic stereo scene and runs the real
converter over it. A figure therefore cannot claim something the code does not
do, and the honest artefacts show up too — the recovery figure has the halo that
high disparity always leaves without a 2D reference. Regenerate after any change
that alters what output looks like. The PNGs are committed because GitHub has to
serve them; the script is what defines them.

Signing is **ad-hoc**, which on Apple Silicon is not optional — an unsigned
binary will not execute at all — but carries no developer identity, so
Gatekeeper rejects a downloaded copy. `--sign "Developer ID Application: …"`
takes a real identity if you ever have one. Notarising on top of that also needs
a secure timestamp, which is still off.
[Download and install](DOWNLOAD.md) is how the app is distributed without one.

The hardened runtime **is** on, with
`packaging/entitlements.plist`. It is what stops `DYLD_INSERT_LIBRARIES` loading
someone else's dylib into the app before its own code runs, and stops another
process attaching to it — `--verify` demonstrates the first rather than assuming
it.

Getting it required giving up library validation, and the reason is worth
recording because it looks like a mistake otherwise. Library validation demands
that every loaded library carry the same Team ID as the process loading it. An
ad-hoc signature carries no Team ID, and macOS does not treat two absent Team IDs
as a match — so the bundled ffmpeg is refused its own vendored `libavcodec` and
dies at `dyld`. There is no way around it without a Developer ID.

What that leaves open: anything already running as your user can replace a dylib
inside the installed bundle, and it will be loaded. That is the same exposure as
replacing the executable, which nothing prevents either once quarantine has been
cleared. A Developer ID and `--options library-validation` is the only real fix,
and it costs $99 a year.

## Packaging for download

`build-app.py --dmg` writes `target/StereoscopicConverter-<version>.dmg` with an
`/Applications` symlink beside the app. The version is read from the
`Info.plist` that shipped rather than from `Cargo.toml`, so the file name cannot
disagree with what the app reports about itself.

Two more files come out beside it, and both belong in the release:

- `StereoscopicConverter-<version>.dmg.sha256` — publish it as a release asset.
  It is the only way someone taking the direct download can tell that what
  arrived is what was built, and an unnotarised app has no other answer.
- `stereoscopic-converter.rb` — the cask with version and digest already
  substituted, to copy into the tap. The digest is never transcribed by hand;
  the one in `packaging/` is a template and its `sha256` is a placeholder that
  will not match anything.

It verifies the signature on the copy *inside* the mounted image. Packaging is
where a signature gets broken, so verifying the bundle it was made from would
prove nothing about what anyone downloads.

One trap is worth knowing about. Homebrew installs its libraries read-only and
`shutil.copy2` preserves the mode, so the vendored dylibs arrive with no owner
write bit — and clearing an extended attribute requires write permission. Since
everyone who downloads an unnotarised app has to run `xattr -dr
com.apple.quarantine` on it, that produced a Permission denied for every nested
binary. The bundler now restores the write bit before signing.


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

## Where a new control goes

`ana-app` draws its settings panel from `visible_sections()`, a list rather than
a stack of conditionals, so which controls a given source can reach is something
a test asserts. Convergence was written, tested and shipped inside the recovery
section — which only anaglyph sources draw — so it worked for every input mode
while being reachable from one. Right-eye-only output was lost the same way,
missing from `OutputLayout::ALL`. Both faults were invisible to a suite that
tested only the engine.

A control that applies to every source belongs in the unconditional part of that
list, and the test that says so belongs with it.

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
