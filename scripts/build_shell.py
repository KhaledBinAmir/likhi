"""Build the Rust text service for both architectures.

    python scripts/build_shell.py            # -> dist/shell/x64/LikhiTextService.dll and x86/

Two DLLs because the text service is loaded into the process that has focus: 64-bit applications
load the x64 build, 32-bit ones the x86 build, and Windows picks whichever matches. Both are
registered by the installer with the corresponding regsvr32.

Needs the Rust toolchain (rustup) with the x86_64- and i686-pc-windows-msvc targets, and the
Visual Studio Build Tools C++ workload for the linker. rustc finds the linker itself; no vcvars.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
CRATE = REPO / "shell"
ICON = REPO / "windows" / "pime" / "likhi" / "icon.ico"

TARGETS = {
    "x64": "x86_64-pc-windows-msvc",
    "x86": "i686-pc-windows-msvc",
}


def cargo() -> str:
    found = shutil.which("cargo")
    if found:
        return found
    home = Path(os.environ.get("USERPROFILE", str(Path.home()))) / ".cargo" / "bin" / "cargo.exe"
    if home.exists():
        return str(home)
    raise SystemExit("cargo not found; install rustup (winget install Rustlang.Rustup)")


def build(out: Path, release: bool) -> None:
    profile = "release" if release else "debug"
    for arch, triple in TARGETS.items():
        cmd = [cargo(), "build", "--target", triple]
        if release:
            cmd.append("--release")
        print(f"[shell] {arch}: {' '.join(cmd[1:])}", flush=True)
        result = subprocess.run(cmd, cwd=CRATE)
        if result.returncode != 0:
            raise SystemExit(f"cargo build failed for {triple}")
        built = CRATE / "target" / triple / profile / "likhi_tsf.dll"
        dest = out / arch
        dest.mkdir(parents=True, exist_ok=True)
        shutil.copy2(built, dest / "LikhiTextService.dll")
        shutil.copy2(ICON, dest / "likhi.ico")
        size = (dest / "LikhiTextService.dll").stat().st_size
        print(f"[shell] {arch}: {dest / 'LikhiTextService.dll'} ({size / 1024:.0f} KB)")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=REPO / "dist" / "shell")
    ap.add_argument("--debug", action="store_true", help="debug build (faster to compile, has symbols)")
    args = ap.parse_args(argv)
    build(args.out, release=not args.debug)
    return 0


if __name__ == "__main__":
    sys.exit(main())
