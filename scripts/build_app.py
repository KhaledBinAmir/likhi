"""Build dist/Likhi.exe, the window people open from the Start menu.

Compiled with the C# compiler that ships inside every .NET Framework installation, so the build
needs nothing installed and the result needs no runtime: .NET Framework 4 is present on every
Windows 10 and 11. About twenty kilobytes, against roughly ten megabytes to add tkinter to the
engine's embedded Python for the sake of one window.

    python scripts/build_app.py
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SOURCE = REPO / "app" / "LikhiApp.cs"
ICON = REPO / "windows" / "pime" / "likhi" / "icon.ico"


def find_csc() -> Path:
    """The newest in-box C# compiler. Framework64 first: the app is built for 64-bit Windows."""
    root = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "Microsoft.NET"
    candidates = sorted(root.glob("Framework64/v4.*/csc.exe")) + sorted(
        root.glob("Framework/v4.*/csc.exe")
    )
    if not candidates:
        raise SystemExit(
            f"no C# compiler under {root}. It ships with .NET Framework 4, which is part of "
            "Windows; a machine without it cannot build this, though it can still run the result."
        )
    return candidates[-1]


def build(out: Path) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    csc = find_csc()
    cmd = [
        str(csc),
        "/nologo",
        "/target:winexe",  # no console window
        "/optimize+",
        "/platform:anycpu",
        f"/out:{out}",
        f"/win32icon:{ICON}",
        "/reference:System.dll",
        "/reference:System.Drawing.dll",
        "/reference:System.Windows.Forms.dll",
        str(SOURCE),
    ]
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stdout + result.stderr)
        raise SystemExit(f"csc failed with exit code {result.returncode}")
    if result.stdout.strip():
        print(result.stdout.strip())
    print(f"[app] built {out} ({out.stat().st_size / 1024:.0f} KB) with {csc.parent.name}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--out", type=Path, default=REPO / "dist" / "Likhi.exe")
    args = ap.parse_args(argv)
    build(args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
