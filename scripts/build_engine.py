"""Build the Rust engine and stage it for the installer.

    python scripts/build_engine.py            # -> dist/engine/

Produces the whole engine side of the product:

    dist/engine/likhi-server.exe        the server
    dist/engine/models/lexicon/*.lkx    the tables
    dist/engine/models/indicxlit/*      the transliteration model
    dist/engine/models/avro.json        the rule tables

This replaces `build_runtime.py`, which packaged an embedded CPython, NumPy, marisa-trie and the
Likhi package -- about 65 MB and a 630 ms cold start -- to run the same engine. The Python is still
the reference implementation and still runs the research harness; it is simply no longer what ships.

The server finds `models` relative to its own executable, so the layout above is load-bearing: move
the binary without moving the tables and the engine starts, finds nothing, and exits.

x64 only, as the Python runtime was: the engine is a separate process, so only the text service DLL
needs to exist for both architectures.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
CRATE = REPO / "engine"
TARGET = "x86_64-pc-windows-msvc"


def log(msg: str) -> None:
    print(f"[engine] {msg}", flush=True)


def cargo() -> str:
    found = shutil.which("cargo")
    if found:
        return found
    home = Path(os.environ.get("USERPROFILE", str(Path.home()))) / ".cargo" / "bin" / "cargo.exe"
    if home.exists():
        return str(home)
    raise SystemExit("cargo not found; install rustup (winget install Rustlang.Rustup)")


def sizeof(path: Path) -> str:
    total = sum(f.stat().st_size for f in path.rglob("*") if f.is_file())
    return f"{total / 1e6:.1f} MB"


def product_version() -> str:
    """The version in the installer script, which is the one place a release's number is written.

    The engine compares it against the newest signed release to decide whether to offer an update,
    so it has to be the same number the installer and the release carry. Reading it from here keeps
    it that way without anyone having to remember to change it twice.
    """
    iss = (REPO / "installer" / "likhi.iss").read_text(encoding="utf-8")
    m = re.search(r'^#define AppVersion "([0-9]+\.[0-9]+\.[0-9]+)"', iss, re.MULTILINE)
    if not m:
        raise SystemExit("no '#define AppVersion \"x.y.z\"' in installer/likhi.iss")
    return m.group(1)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", type=Path, default=REPO / "dist" / "engine")
    ap.add_argument("--data", type=Path, default=REPO / "models" / "rust")
    ap.add_argument("--skip-build", action="store_true", help="stage an already-built binary")
    args = ap.parse_args()

    if not args.skip_build:
        version = product_version()
        log(f"cargo build --release --target {TARGET}  (version {version})")
        # A build made here is a release build and knows its version, which switches the updater
        # on. A plain `cargo build` has no version and leaves it off, so a development engine never
        # offers to replace itself with an older published one.
        result = subprocess.run(
            [cargo(), "build", "--release", "--target", TARGET],
            cwd=CRATE,
            env={**os.environ, "LIKHI_VERSION": version},
        )
        if result.returncode != 0:
            raise SystemExit("cargo build failed")

    exe = CRATE / "target" / TARGET / "release" / "likhi-server.exe"
    if not exe.exists():
        raise SystemExit(f"missing {exe}")

    if not (args.data / "indicxlit" / "model.lkw").exists():
        raise SystemExit(
            f"missing converted data in {args.data}; run scripts/build_rust_data.py first"
        )

    if args.out.exists():
        shutil.rmtree(args.out)
    args.out.mkdir(parents=True)

    shutil.copy2(exe, args.out / "likhi-server.exe")
    log(f"likhi-server.exe: {exe.stat().st_size / 1e6:.1f} MB")

    models = args.out / "models"
    shutil.copytree(args.data, models, ignore=shutil.ignore_patterns("__pycache__", "*.part"))
    log(f"models: {sizeof(models)}")

    log(f"built {args.out} ({sizeof(args.out)})")


if __name__ == "__main__":
    main()
