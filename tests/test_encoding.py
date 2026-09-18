"""No source file may carry a byte-order mark or mojibake.

Twice now a PowerShell `Get-Content | Set-Content` round trip has read UTF-8 as the system code page
and written it back, turning Bengali into mojibake and adding a BOM -- once in the settings window's
source, once in the lexicon builder's comments. Neither was caught by a compiler, because both files
still parsed. The damage is not reversible: cp1252 has no mapping for some of the bytes, so they are
replaced rather than transformed.

This project is full of Bengali string literals, so that failure is always one careless command
away. A test is cheaper than remembering.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


def test_no_boms_or_mojibake_in_sources() -> None:
    result = subprocess.run(
        [sys.executable, str(REPO / "scripts" / "check_encoding.py")],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=REPO,
    )
    assert result.returncode == 0, (
        "scripts/check_encoding.py found damaged source files:\n"
        f"{result.stdout}\n{result.stderr}\n"
        "Run `python scripts/check_encoding.py --fix` to strip BOMs. Mojibake has to be retyped."
    )
