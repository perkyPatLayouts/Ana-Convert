# Download

Stereoscopic Converter runs on Apple Silicon Macs — M1 or later, macOS 11 Big
Sur or newer. It carries its own ffmpeg, so there is nothing else to install.

Intel Macs are not supported. Every binary in the app is arm64, and on an Intel
machine it will not start.

---

## Install with Homebrew

The shortest route, and the one that avoids the warning described below.

```bash
brew install --cask --no-quarantine perkypatlayouts/tap/stereoscopic-converter
```

`--no-quarantine` matters. Leave it out and macOS will refuse to open the app.

---

## Or download the disk image

**[StereoscopicConverter-0.1.0.dmg](https://github.com/perkyPatLayouts/Ana-Convert/releases/latest)**

Open it and drag the app to Applications. Then run this once:

```bash
xattr -dr com.apple.quarantine "/Applications/Stereoscopic Converter.app"
```

Without it you will see the warning below.

---

## Check what you downloaded

Every release publishes a `.sha256` file next to the disk image. Since you are
being asked to trust the download rather than Apple's check on it, it is worth
knowing that what arrived is what was built:

```bash
shasum -a 256 -c StereoscopicConverter-0.1.0.dmg.sha256
```

`OK` means the file is byte-for-byte the one released. Anything else — a
truncated download, a proxy that rewrote it, a file from somewhere other than
the Releases page — will not match.

This proves the image was not altered in transit. It cannot tell you the build
itself is trustworthy; only reading the source and building it yourself does
that, and the last section explains how.

The Homebrew route checks the same digest automatically: it is written into the
cask, and `brew` refuses an image that does not match.

---

## "The application is damaged and can't be opened"

The app is not damaged. This is the message macOS shows for an application that
has not been through Apple's notarisation service, and it is worth explaining
rather than hiding, because it looks far more alarming than what it means.

Notarisation requires membership of the Apple Developer Program at $99 a year.
This app is signed, but with an ad-hoc signature that carries no developer
identity, so Gatekeeper has nothing to check the app against and assumes the
worst. Every downloaded file also arrives with a quarantine flag, and it is the
combination of the two that produces the message.

The `xattr` command above removes that quarantine flag from this one app, which
lets it open normally. It changes nothing else, and it does not disable
Gatekeeper or affect any other application.

You are being asked to trust the download instead of Apple's check on it. That
is a real trade, so if you would rather not make it:

## Build it yourself

The source is the whole app, and a build on your own machine is never
quarantined.

```bash
git clone https://github.com/perkyPatLayouts/Ana-Convert
cd Ana-Convert
brew install ffmpeg          # only needed to build; the app bundles its own copy
python3 packaging/build-app.py --verify
open target
```

`--verify` checks the signature and runs the bundled ffmpeg with `PATH` emptied,
proving the app depends on nothing outside itself.

---

## Licence and source

Stereoscopic Converter is free software under the
[GPL-3.0-or-later](https://www.gnu.org/licenses/gpl-3.0.html). The source is at
[github.com/perkyPatLayouts/Ana-Convert](https://github.com/perkyPatLayouts/Ana-Convert).

The app bundles **FFmpeg 9.0.1** — the build refuses to package any other
version, and `target/vendored-ffmpeg.txt` records the SHA-256 of every binary
that went in. It is built with `--enable-gpl --enable-version3`
and also covered by the GPL. Its corresponding source is available from
[ffmpeg.org/releases](https://ffmpeg.org/releases/) as `ffmpeg-9.0.1.tar.xz`,
and the build configuration used is the
[Homebrew ffmpeg formula](https://github.com/Homebrew/homebrew-core/blob/main/Formula/f/ffmpeg.rb).
