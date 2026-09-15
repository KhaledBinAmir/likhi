"""likhi-data: build runtime artifacts from the raw datasets.

likhi-data lexicon [--out models/lexicon] [--wiki-lines N]
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="likhi-data", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser(
        "lexicon", help="build unigram counts, romanization index and phonetic-key index"
    )
    p.add_argument("--out", type=Path, default=REPO / "models" / "lexicon")
    p.add_argument("--wiki-lines", type=int, help="limit Wikipedia lines (for quick builds)")
    p.add_argument(
        "--min-wiki", type=int, default=2, help="min Wikipedia count for corpus-only words"
    )
    args = ap.parse_args(argv)
    if args.cmd == "lexicon":
        from likhi.data.build_lexicon import build

        build(args.out, wiki_lines=args.wiki_lines, min_wiki=args.min_wiki)
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
