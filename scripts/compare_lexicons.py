"""Compare two lexicon directories entry by entry.

Used to check the Rust lexicon builder against the Python one it replaces. The Rust builder writes
`.lkx` directly; `build_rust_data.py` converts the Python builder's marisa tries to `.lkx`. If the
two agree on every key and record, the port is faithful.

    python scripts/compare_lexicons.py models/rust/lexicon models/rust-built/lexicon

Ranks are reported separately and are *expected* to differ: under marisa the prefix-scan order was a
LOUDS traversal, and the Rust builder replaces it with the order pairs were first seen, which
follows source quality. That changes how equal scores break, which is a behaviour change to measure
rather than a bug to fix -- see the accuracy check in the same run.
"""

from __future__ import annotations

import argparse
import sys
from collections import Counter
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "src"))

from likhi import lkx  # noqa: E402

TABLES = [
    ("unigrams", "<III"),
    ("romans", "<Ib"),
    ("keys", "<I"),
    ("prefixes", "<I"),
    ("bigrams", "<I"),
    ("bigram_totals", "<I"),
]


def load(directory: Path, name: str, fmt: str) -> Counter:
    """Multiset of (key, record). A multiset because duplicate keys are legal and meaningful."""
    table = lkx.open_table(directory, name, fmt)
    return Counter((k, tuple(r)) for k, r in table.items())


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("reference", type=Path)
    ap.add_argument("candidate", type=Path)
    ap.add_argument("--show", type=int, default=8, help="examples of each kind of difference")
    args = ap.parse_args()

    worst = 0
    for name, fmt in TABLES:
        a_path = args.reference / f"{name}.lkx"
        b_path = args.candidate / f"{name}.lkx"
        if not a_path.exists() or not b_path.exists():
            print(f"{name:15} SKIP (missing on one side)")
            continue
        a = load(args.reference, name, fmt)
        b = load(args.candidate, name, fmt)
        if a == b:
            print(f"{name:15} identical  ({sum(a.values()):,} entries)")
            continue

        only_a = a - b
        only_b = b - a
        worst = max(worst, 1)
        print(
            f"{name:15} DIFFER     reference {sum(a.values()):,}, candidate {sum(b.values()):,}; "
            f"{sum(only_a.values()):,} only in reference, {sum(only_b.values()):,} only in candidate"
        )
        for label, diff in (("only in reference", only_a), ("only in candidate", only_b)):
            for (key, rec), n in list(diff.items())[: args.show]:
                shown = key.encode("unicode_escape").decode("ascii")
                print(f"    {label}: {shown!r} -> {rec}" + (f" x{n}" if n > 1 else ""))
    return worst


if __name__ == "__main__":
    raise SystemExit(main())
