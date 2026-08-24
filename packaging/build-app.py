#!/usr/bin/env python3
"""Builds Stereoscopic Converter.app for Apple Silicon.

The app carries its own ffmpeg. That is the whole point of bundling: a copy that
only runs where Homebrew happens to be installed is not an app, it is a
development setup. So ffmpeg, ffprobe and every library they reach are copied in
and their install names rewritten to point inside the bundle.

Run from the repository root:

    python3 packaging/build-app.py            # build and sign
    python3 packaging/build-app.py --verify   # also check it runs with an empty PATH
"""
import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKAGING = ROOT / "packaging"
APP = ROOT / "target" / "Stereoscopic Converter.app"
BINARY = "ana-convert-app"
# Anything under these prefixes ships with macOS and must not be copied.
SYSTEM_PREFIXES = ("/usr/lib", "/System")


def run(cmd, **kw):
    result = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if result.returncode != 0:
        sys.exit(f"failed: {' '.join(str(c) for c in cmd)}\n{result.stderr.strip()}")
    return result.stdout


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
    parser.add_argument("--sign", default="-", help="signing identity, default ad-hoc")
    args = parser.parse_args()

    print("building release binary…")
    run(["cargo", "build", "--release", "-p", "ana-app"], cwd=ROOT)
    built = ROOT / "target" / "release" / BINARY
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

    # Signing must come last: rewriting install names invalidates a signature,
    # and nested code has to be signed before the bundle that contains it.
    print(f"signing (identity {args.sign!r})…")
    for path in list(frameworks.iterdir()) + tools + [macos / BINARY]:
        run(["codesign", "--force", "--timestamp=none", "--sign", args.sign, str(path)])
    run(["codesign", "--force", "--timestamp=none", "--sign", args.sign, str(APP)])

    size = sum(f.stat().st_size for f in APP.rglob("*") if f.is_file())
    print(f"\n{APP}  ({size / 1024 / 1024:.0f} MB)")

    if args.verify:
        verify(macos)


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


if __name__ == "__main__":
    main()
