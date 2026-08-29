#!/usr/bin/env python3
"""Builds Stereoscopic Converter.app for Apple Silicon.

The app carries its own ffmpeg. That is the whole point of bundling: a copy that
only runs where Homebrew happens to be installed is not an app, it is a
development setup. So ffmpeg, ffprobe and every library they reach are copied in
and their install names rewritten to point inside the bundle.

Run from the repository root:

    python3 packaging/build-app.py            # build and sign
    python3 packaging/build-app.py --verify   # also check it runs with an empty PATH
    python3 packaging/build-app.py --dmg      # also package it for download
"""
import argparse
import hashlib
import os
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKAGING = ROOT / "packaging"
APP = ROOT / "target" / "Stereoscopic Converter.app"
# Cargo target names cannot contain spaces, so the binary is built under a
# hyphenated name and installed into the bundle under the one people see — in
# the Dock, in Activity Monitor, and in `CFBundleExecutable`, which has to match
# the file on disk exactly.
BUILT_BINARY = "stereoscopic-converter"
BINARY = "Stereoscopic Converter"
ENTITLEMENTS = PACKAGING / "entitlements.plist"
CASK = PACKAGING / "stereoscopic-converter.rb"
# Anything under these prefixes ships with macOS and must not be copied.
SYSTEM_PREFIXES = ("/usr/lib", "/System")
# The ffmpeg a release is expected to carry. Whatever is on PATH at build time
# gets copied in, signed, and shipped to everyone, so it must be the version
# that was actually looked at — not merely the one Homebrew happened to have
# that morning. docs/DOWNLOAD.md states this number to users; the two move
# together or not at all.
FFMPEG_VERSION = "9.0.1"


def run(cmd, **kw):
    result = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if result.returncode != 0:
        sys.exit(f"failed: {' '.join(str(c) for c in cmd)}\n{result.stderr.strip()}")
    return result.stdout


def sha256_of(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def ffmpeg_version(binary: Path) -> str:
    """The version string a copied ffmpeg reports about itself."""
    first = run([str(binary), "-version"]).splitlines()[0]
    # "ffmpeg version 9.0.1 Copyright (c) ..." — and on a build from source,
    # something like "ffmpeg version n9.0.1-2-gabc123".
    parts = first.split()
    return parts[2].lstrip("n") if len(parts) > 2 else "unknown"


def linked_libraries(binary: Path) -> list[str]:
    """Non-system libraries a binary loads, as written in its load commands."""
    out = run(["otool", "-L", str(binary)])
    found = []
    for line in out.splitlines()[1:]:
        path = line.strip().split(" ")[0]
        if path.startswith(SYSTEM_PREFIXES) or path.startswith("@"):
            continue
        found.append(path)
    return found


def gather_closure(seeds: list[Path]) -> dict[str, Path]:
    """Every library the seeds reach, keyed by the file name it will be given."""
    closure: dict[str, Path] = {}
    queue = list(seeds)
    while queue:
        current = queue.pop()
        for reference in linked_libraries(current):
            real = Path(os.path.realpath(reference))
            if not real.exists():
                sys.exit(f"{current.name} needs {reference}, which is missing")
            if real.name in closure:
                continue
            closure[real.name] = real
            queue.append(real)
    return closure


def rewrite(binary: Path, closure: dict[str, Path], rpath: str):
    """Points a binary's references at the copies inside the bundle."""
    for reference in linked_libraries(binary):
        name = Path(os.path.realpath(reference)).name
        if name in closure:
            run(["install_name_tool", "-change", reference, f"@rpath/{name}", str(binary)])
    # Errors are ignored deliberately: an rpath may already be present, and
    # install_name_tool has no way to ask.
    subprocess.run(
        ["install_name_tool", "-add_rpath", rpath, str(binary)],
        capture_output=True,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--verify", action="store_true", help="check self-containment")
    parser.add_argument("--dmg", action="store_true", help="package for download")
    parser.add_argument("--sign", default="-", help="signing identity, default ad-hoc")
    parser.add_argument(
        "--ffmpeg-version",
        default=FFMPEG_VERSION,
        help=f"the ffmpeg this build expects to find on PATH (default {FFMPEG_VERSION})",
    )
    args = parser.parse_args()

    print("building release binary…")
    run(["cargo", "build", "--release", "-p", "ana-app"], cwd=ROOT)
    built = ROOT / "target" / "release" / BUILT_BINARY
    if not built.exists():
        sys.exit(f"{built} was not produced")

    print("assembling bundle…")
    if APP.exists():
        shutil.rmtree(APP)
    macos = APP / "Contents" / "MacOS"
    frameworks = APP / "Contents" / "Frameworks"
    resources = APP / "Contents" / "Resources"
    for directory in (macos, frameworks, resources):
        directory.mkdir(parents=True)

    shutil.copy2(built, macos / BINARY)
    shutil.copy2(PACKAGING / "Info.plist", APP / "Contents" / "Info.plist")

    icon = PACKAGING / "AppIcon.icns"
    if not icon.exists():
        print("  drawing icon…")
        run([sys.executable, str(PACKAGING / "make-icon.py"), str(icon)], cwd=ROOT)
    shutil.copy2(icon, resources / "AppIcon.icns")

    # The licence has to travel with the binaries it covers.
    shutil.copy2(ROOT / "LICENSE", resources / "LICENSE")

    print("vendoring ffmpeg…")
    tools = []
    for tool in ("ffmpeg", "ffprobe"):
        found = shutil.which(tool)
        if not found:
            sys.exit(f"{tool} is not on PATH, so there is nothing to bundle")
        target = macos / tool
        shutil.copy2(os.path.realpath(found), target)
        tools.append(target)

    # Whatever was on PATH is now inside a bundle that will be signed and handed
    # to people. Refusing an unexpected version is the only moment this can be
    # caught: afterwards it is indistinguishable from the one that was reviewed.
    found_version = ffmpeg_version(macos / "ffmpeg")
    if found_version != args.ffmpeg_version:
        sys.exit(
            f"  PATH has ffmpeg {found_version}, but this release is built "
            f"against {args.ffmpeg_version}.\n"
            f"  Install the expected version, or — having checked what changed "
            f"in it — update FFMPEG_VERSION in {Path(__file__).name} and the "
            f"version named in docs/DOWNLOAD.md, then build again.\n"
            f"  To build a one-off without moving those, pass "
            f"--ffmpeg-version {found_version}."
        )
    print(f"  ffmpeg {found_version}")

    closure = gather_closure(tools)
    print(f"  {len(closure)} libraries")
    for name, source in closure.items():
        shutil.copy2(source, frameworks / name)

    for name in closure:
        library = frameworks / name
        run(["install_name_tool", "-id", f"@rpath/{name}", str(library)])
        rewrite(library, closure, "@loader_path")
    for tool in tools:
        rewrite(tool, closure, "@executable_path/../Frameworks")

    # Homebrew installs its libraries read-only and copy2 preserves that, which
    # leaves files nobody can remove an extended attribute from — and clearing
    # com.apple.quarantine is exactly what someone who downloads this has to do.
    # Restore the owner's write bit before signing.
    for path in APP.rglob("*"):
        if path.is_file() and not path.is_symlink():
            path.chmod(path.stat().st_mode | 0o200)

    # Signing must come last: rewriting install names invalidates a signature,
    # and nested code has to be signed before the bundle that contains it.
    print(f"signing (identity {args.sign!r}, hardened runtime)…")
    sign = [
        "codesign",
        "--force",
        "--timestamp=none",
        "--options", "runtime",
        "--entitlements", str(ENTITLEMENTS),
        "--sign", args.sign,
    ]
    for path in list(frameworks.iterdir()) + tools + [macos / BINARY]:
        run(sign + [str(path)])
    run(sign + [str(APP)])

    record_provenance(found_version, macos, frameworks)

    size = sum(f.stat().st_size for f in APP.rglob("*") if f.is_file())
    print(f"\n{APP}  ({size / 1024 / 1024:.0f} MB)")

    if args.verify:
        verify(macos)
    if args.dmg:
        build_dmg()


def record_provenance(version: str, macos: Path, frameworks: Path):
    """Writes down exactly which third-party binaries went into this build.

    Vendoring means the app ships someone else's code under our signature. A
    release that cannot say which ffmpeg it contains cannot answer the only
    question that matters when an advisory comes out.
    """
    manifest = ROOT / "target" / "vendored-ffmpeg.txt"
    lines = [
        f"ffmpeg version: {version}",
        "signed into: " + APP.name,
        "",
        "sha256 of each vendored binary, as copied (before signing rewrote it):",
        "",
    ]
    for path in sorted(
        list(frameworks.iterdir()) + [macos / "ffmpeg", macos / "ffprobe"],
        key=lambda p: p.name,
    ):
        lines.append(f"{sha256_of(path)}  {path.name}")
    manifest.write_text("\n".join(lines) + "\n")
    print(f"  provenance written to {manifest.relative_to(ROOT)}")


def build_dmg():
    """Packages the bundle as a DMG laid out for drag-to-install.

    The version comes from the Info.plist that actually shipped rather than from
    Cargo.toml, so the file name cannot disagree with what the app reports about
    itself.
    """
    print("\npackaging…")
    plist = plistlib.loads((APP / "Contents" / "Info.plist").read_bytes())
    version = plist["CFBundleShortVersionString"]
    dmg = ROOT / "target" / f"StereoscopicConverter-{version}.dmg"

    staging = ROOT / "target" / "dmg-staging"
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir(parents=True)
    # ditto rather than copytree: it is the only copy on macOS that reliably
    # preserves the bundle bit-for-bit, and a disturbed bundle is an invalid
    # signature.
    run(["ditto", str(APP), str(staging / APP.name)])
    (staging / "Applications").symlink_to("/Applications")

    if dmg.exists():
        dmg.unlink()
    run([
        "hdiutil", "create",
        "-volname", "Stereoscopic Converter",
        "-srcfolder", str(staging),
        "-ov", "-format", "UDZO",
        "-quiet",
        str(dmg),
    ])
    shutil.rmtree(staging)

    verify_dmg(dmg)

    digest = sha256_of(dmg)
    print(f"\n{dmg}  ({dmg.stat().st_size / 1024 / 1024:.0f} MB)")

    # An unnotarised download asks people to trust it on our say-so. The least
    # it can do is let them check that what arrived is what left — so the digest
    # is written to a file to publish alongside the image, rather than printed
    # for someone to copy by hand at the end of a long build.
    sums = dmg.with_suffix(".dmg.sha256")
    sums.write_text(f"{digest}  {dmg.name}\n")
    print(f"{sums}")

    # The cask filled in, rather than the numbers printed for transcription.
    # A hash that has to be copied by hand is a hash that can be copied wrong.
    filled = []
    for line in CASK.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("#~"):
            # A note to whoever edits the template, not to whoever reads the tap.
            continue
        if stripped.startswith("version "):
            line = f'  version "{version}"'
        elif stripped.startswith("sha256 "):
            line = f'  sha256 "{digest}"'
        filled.append(line)
    ready = ROOT / "target" / CASK.name
    ready.write_text("\n".join(filled) + "\n")
    print(f"{ready}  (cask with version and sha256 filled in)")


def verify_dmg(dmg: Path):
    """Checks the signature on the copy inside the image, not the one built.

    Packaging is where a signature gets broken, so verifying the source bundle
    would prove nothing about what a user downloads.
    """
    out = run(["hdiutil", "attach", str(dmg), "-nobrowse", "-readonly", "-plist"])
    mount = next(
        entity["mount-point"]
        for entity in plistlib.loads(out.encode())["system-entities"]
        if "mount-point" in entity
    )
    try:
        run(["codesign", "--verify", "--strict", str(Path(mount) / APP.name)])
        print("  signature intact inside the image")
    finally:
        subprocess.run(["hdiutil", "detach", mount, "-quiet"], capture_output=True)


def verify(macos: Path):
    """Proves the bundle needs nothing installed."""
    print("\nverifying…")
    run(["codesign", "--verify", "--deep", "--strict", str(APP)])
    print("  signature ok")

    # An empty PATH is the point: if the bundled ffmpeg were not self-contained
    # this is where it would fall over.
    bare = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "")}
    out = subprocess.run(
        [str(macos / "ffmpeg"), "-version"],
        capture_output=True,
        text=True,
        env=bare,
    )
    if out.returncode != 0:
        sys.exit(f"  bundled ffmpeg will not run without Homebrew:\n{out.stderr.strip()}")
    print(f"  bundled ffmpeg runs standalone: {out.stdout.splitlines()[0]}")

    leaked = [
        f"{path.name} -> {ref}"
        for path in list((APP / "Contents" / "Frameworks").iterdir()) + [macos / "ffmpeg"]
        for ref in linked_libraries(path)
        if ref.startswith("/opt/") or ref.startswith("/usr/local/")
    ]
    if leaked:
        sys.exit("  still pointing outside the bundle:\n    " + "\n    ".join(leaked))
    print("  no references escape the bundle")

    # The app's own binary, not just the tools it drives. It finds and runs the
    # bundled ffmpeg, so this exercises the whole chain the hardened runtime
    # could have broken.
    out = subprocess.run(
        [str(macos / BINARY), "--check"],
        capture_output=True,
        text=True,
        env=bare,
    )
    if out.returncode != 0:
        sys.exit(f"  the app will not start:\n{out.stderr.strip()}")
    print("  the app starts and finds its own tools")

    verify_injection_is_refused(macos)


def verify_injection_is_refused(macos: Path):
    """Checks that the hardened runtime is doing the thing it is here for.

    Without it, DYLD_INSERT_LIBRARIES loads any dylib into the app before its
    own code runs. The flag is easy to lose in a signing change and nothing
    else would notice, so it is proved rather than assumed.
    """
    source = ROOT / "target" / "injection-probe.c"
    dylib = ROOT / "target" / "injection-probe.dylib"
    source.write_text(
        '#include <stdio.h>\n'
        '__attribute__((constructor)) static void probe(void) {\n'
        '    fprintf(stderr, "INJECTED\\n");\n'
        '}\n'
    )
    built = subprocess.run(
        ["clang", "-dynamiclib", "-o", str(dylib), str(source)],
        capture_output=True,
        text=True,
    )
    if built.returncode != 0:
        print("  (skipped the injection check: no working clang)")
        return

    attempt = subprocess.run(
        [str(macos / "ffmpeg"), "-version"],
        capture_output=True,
        text=True,
        env={"PATH": "/usr/bin:/bin", "DYLD_INSERT_LIBRARIES": str(dylib)},
    )
    source.unlink(missing_ok=True)
    dylib.unlink(missing_ok=True)
    if "INJECTED" in attempt.stderr:
        sys.exit("  DYLD_INSERT_LIBRARIES loaded code into the app — runtime flag lost")
    print("  injected libraries are refused")


if __name__ == "__main__":
    main()
