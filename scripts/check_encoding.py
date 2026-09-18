"""Fail if any source file has a byte-order mark or looks double-encoded.

This exists because it has happened twice, both times the same way: a PowerShell
`Get-Content | Set-Content` round trip reads UTF-8 as the system code page and writes it back as
UTF-8, which turns Bengali into mojibake and adds a BOM. The first time it corrupted the settings
window's source; the second time it corrupted the lexicon builder's comments. Neither was caught by
a compiler, because both files still parsed.

    python scripts/check_encoding.py          # report and exit non-zero
    python scripts/check_encoding.py --fix     # strip BOMs, then report what is still wrong

Mojibake cannot be repaired automatically -- cp1252 has no mapping for some bytes, so the round trip
loses them rather than transforming them -- which is why this checks rather than trusting a fix.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

PATTERNS = [
    "engine/src/**/*.rs",
    "engine/tests/**/*.rs",
    "shell/src/**/*.rs",
    "src/**/*.py",
    "scripts/*.py",
    "tests/*.py",
    "app/*.cs",
    "installer/*.ps1",
    "installer/*.iss",
]

# Sequences that are almost certainly UTF-8 misread as a single-byte code page: a Bengali
# character (U+09xx) double-encodes to "à¦.." or "à§..", and the joiners to "â€.".
MOJIBAKE = ("à¦", "à§", "â", "â", "Ã")


def files() -> list[Path]:
    out: list[Path] = []
    here = Path(__file__).resolve()
    for pattern in PATTERNS:
        for path in REPO.glob(pattern):
            if "target" in path.parts or ".venv" in path.parts:
                continue
            # This file holds the mojibake sequences as literals, so it matches itself.
            if path.resolve() == here:
                continue
            out.append(path)
    return sorted(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--fix", action="store_true", help="strip BOMs in place")
    args = ap.parse_args()

    boms: list[Path] = []
    garbled: list[Path] = []
    for path in files():
        data = path.read_bytes()
        if data.startswith(b"\xef\xbb\xbf"):
            boms.append(path)
            if args.fix:
                path.write_bytes(data[3:])
                data = data[3:]
        text = data.decode("utf-8", errors="replace")
        if any(m in text for m in MOJIBAKE) or "�" in text:
            garbled.append(path)

    for path in boms:
        rel = path.relative_to(REPO)
        print(f"{'fixed' if args.fix else 'BOM  '}: {rel}")
    for path in garbled:
        print(f"MOJIBAKE: {path.relative_to(REPO)}  (not repairable automatically)")

    if not boms and not garbled:
        print(f"{len(files())} source files: no BOMs, no mojibake")
        return 0
    if garbled:
        return 1
    return 0 if args.fix else 1


if __name__ == "__main__":
    sys.exit(main())
