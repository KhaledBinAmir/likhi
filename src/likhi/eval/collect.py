"""likhi-collect: build a personal test set of (romanized, Bangla) sentence pairs.

The file is JSON Lines at data/personal/personal.jsonl (git-ignored). Each line:
    {"roman": "ami ekhon office e jacchi", "gold": ["আমি এখন অফিসে যাচ্ছি"], "tags": ["daily"]}

Workflows:
    likhi-collect add                       interactive: type roman, then gold, repeat
    likhi-collect import banglish.txt       one roman sentence per line -> template to fill gold into
    likhi-collect fill template.jsonl       interactive gold entry for a template
    likhi-collect stats                     counts, tags, token coverage
    likhi-collect validate                  checks alignment and Unicode issues

Tips for a set that reflects how you really type: take romanized text you actually sent in the
past (old chats), do not "clean it up", and write the gold the way you would want it to appear.
Add alternates when two spellings are both fine (জন্য / জন্যে).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

from likhi.engine.textnorm import canonical, has_bengali

PERSONAL = Path(__file__).resolve().parents[3] / "data" / "personal" / "personal.jsonl"


def _read(path: Path) -> list[dict]:
    if not path.exists():
        return []
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#"):
                out.append(json.loads(line))
    return out


def _append(path: Path, obj: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(obj, ensure_ascii=False) + "\n")


def _parse_gold(text: str) -> list[str]:
    """Gold entry: alternates separated by ' | '."""
    return [canonical(g.strip()) for g in text.split("|") if g.strip()]


def cmd_add(args: argparse.Namespace) -> int:
    print("Enter pairs. Empty roman line to stop. Alternates in gold with ' | '. Tags after '#'.")
    n = 0
    while True:
        try:
            roman = input("roman> ").strip()
        except EOFError:
            break
        if not roman:
            break
        gold = input("gold > ").strip()
        tags: list[str] = []
        if "#" in gold:
            gold, tag_s = gold.split("#", 1)
            tags = [t.strip() for t in tag_s.split(",") if t.strip()]
        golds = _parse_gold(gold)
        if not golds or not all(has_bengali(g) for g in golds):
            print("  gold must be Bangla; skipped")
            continue
        _append(PERSONAL, {"roman": roman, "gold": golds, "tags": tags})
        n += 1
    print(f"added {n} pairs to {PERSONAL}")
    return 0


def cmd_import(args: argparse.Namespace) -> int:
    src = Path(args.file)
    out = Path(args.out or src.with_suffix(".template.jsonl"))
    lines = [ln.strip() for ln in src.read_text(encoding="utf-8").splitlines() if ln.strip()]
    with open(out, "w", encoding="utf-8") as f:
        for ln in lines:
            f.write(json.dumps({"roman": ln, "gold": [], "tags": []}, ensure_ascii=False) + "\n")
    print(f"wrote {len(lines)} template lines to {out}; fill gold with: likhi-collect fill {out}")
    return 0


def cmd_fill(args: argparse.Namespace) -> int:
    path = Path(args.file)
    rows = _read(path)
    todo = [r for r in rows if not r.get("gold")]
    print(f"{len(todo)} of {len(rows)} lines need gold. Empty gold skips; 'q' quits.")
    for r in todo:
        print(f"\nroman: {r['roman']}")
        try:
            gold = input("gold > ").strip()
        except EOFError:
            break
        if gold == "q":
            break
        if not gold:
            continue
        tags: list[str] = []
        if "#" in gold:
            gold, tag_s = gold.split("#", 1)
            tags = [t.strip() for t in tag_s.split(",") if t.strip()]
        r["gold"] = _parse_gold(gold)
        r["tags"] = tags
        _append(PERSONAL, r)
    # rewrite the template without the finished rows
    remaining = [r for r in rows if not r.get("gold")]
    with open(path, "w", encoding="utf-8") as f:
        for r in remaining:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"done; {len(remaining)} lines still without gold in {path}")
    return 0


def cmd_import_pairs(args: argparse.Namespace) -> int:
    """Two-line pairs: a roman line followed by its Bangla line; blank lines separate pairs.
    Lines starting with '#' are comments. A Bangla line may hold alternates separated by ' | '."""
    lines = Path(args.file).read_text(encoding="utf-8").splitlines()
    pairs: list[tuple[str, str]] = []
    buf: list[str] = []
    for ln in lines + [""]:
        s = ln.strip()
        if s.startswith("#"):
            continue
        if not s:
            if len(buf) >= 2:
                pairs.append((buf[0], buf[1]))
            elif len(buf) == 1:
                print(f"  skipped (no Bangla line): {buf[0][:60]}")
            buf = []
            continue
        buf.append(s)
    n = 0
    for roman, gold in pairs:
        golds = _parse_gold(gold)
        if not golds or not all(has_bengali(g) for g in golds) or has_bengali(roman):
            print(f"  skipped (roman/Bangla order?): {roman[:40]} / {gold[:40]}")
            continue
        _append(PERSONAL, {"roman": roman, "gold": golds, "tags": [args.tag] if args.tag else []})
        n += 1
    print(f"added {n} pairs to {PERSONAL}")
    return 0


_RE_WA_LINE = re.compile(
    r"^‎?\[?(?P<date>\d{1,2}[./-]\d{1,2}[./-]\d{2,4}),?\s+(?P<time>\d{1,2}:\d{2}(?::\d{2})?\s?(?:[APap][Mm])?)\]?\s*[-–]?\s*(?P<name>[^:]{1,60}?):\s(?P<msg>.*)$"
)


def cmd_import_whatsapp(args: argparse.Namespace) -> int:
    """Pull your own romanized-Bangla messages out of a WhatsApp 'Export chat' text file.

    Keeps lines by the given sender that are Latin letters only, at least ``--min-words`` words,
    and not obviously English (a small stop-list heuristic). Writes a template JSONL to fill gold
    into with ``likhi-collect fill``. Nothing is sent anywhere; this is local text processing.
    """
    english_markers = {
        "the",
        "and",
        "is",
        "are",
        "you",
        "your",
        "this",
        "that",
        "with",
        "for",
        "have",
        "will",
        "please",
        "thanks",
        "thank",
        "ok",
        "okay",
        "yes",
        "no",
        "not",
        "can",
        "what",
        "when",
    }
    text = Path(args.file).read_text(encoding="utf-8", errors="replace").splitlines()
    out = Path(args.out or (Path(args.file).with_suffix(".template.jsonl")))
    me = args.me.strip().casefold()
    kept: list[str] = []
    for ln in text:
        m = _RE_WA_LINE.match(ln.strip())
        if not m:
            continue
        if m.group("name").strip().casefold() != me:
            continue
        msg = m.group("msg").strip()
        if "<Media omitted>" in msg or "http" in msg or "@" in msg:
            continue
        if has_bengali(msg) or not re.fullmatch(r"[A-Za-z0-9 ,.?!'\-]+", msg):
            continue
        words = [w.casefold() for w in re.findall(r"[A-Za-z']+", msg)]
        if len(words) < args.min_words:
            continue
        eng = sum(1 for w in words if w in english_markers)
        if eng / max(1, len(words)) > 0.34:
            continue
        kept.append(msg)
    seen: set[str] = set()
    uniq = [k for k in kept if not (k.casefold() in seen or seen.add(k.casefold()))]
    if args.limit:
        uniq = uniq[: args.limit]
    with open(out, "w", encoding="utf-8") as f:
        for msg in uniq:
            f.write(
                json.dumps({"roman": msg, "gold": [], "tags": ["chat"]}, ensure_ascii=False) + "\n"
            )
    print(f"kept {len(uniq)} of your messages -> {out}; next: likhi-collect fill {out}")
    return 0


def cmd_stats(args: argparse.Namespace) -> int:
    rows = _read(PERSONAL)
    if not rows:
        print(f"no data at {PERSONAL}")
        return 1
    toks = sum(len(r["roman"].split()) for r in rows)
    tags = Counter(t for r in rows for t in r.get("tags", []))
    alts = sum(1 for r in rows if len(r["gold"]) > 1)
    print(f"sentences: {len(rows)}   roman tokens: {toks}   with alternates: {alts}")
    print("tags:", dict(tags.most_common()))
    aligned = sum(
        1 for r in rows if any(len(r["roman"].split()) == len(g.split()) for g in r["gold"])
    )
    print(
        f"positionally alignable (same token count): {aligned} ({100 * aligned / len(rows):.0f}%)"
    )
    return 0


def cmd_validate(args: argparse.Namespace) -> int:
    rows = _read(PERSONAL)
    bad = 0
    for i, r in enumerate(rows, 1):
        if not r.get("roman") or not r.get("gold"):
            print(f"line {i}: missing roman or gold")
            bad += 1
            continue
        for g in r["gold"]:
            if not has_bengali(g):
                print(f"line {i}: gold without Bangla: {g}")
                bad += 1
            if canonical(g) != g:
                print(
                    f"line {i}: gold not in canonical Unicode form (will be normalized on load): {g}"
                )
    print(f"{len(rows)} rows, {bad} problems")
    return 1 if bad else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="likhi-collect",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("add").set_defaults(fn=cmd_add)
    p = sub.add_parser("import")
    p.add_argument("file")
    p.add_argument("--out")
    p.set_defaults(fn=cmd_import)
    p = sub.add_parser("fill")
    p.add_argument("file")
    p.set_defaults(fn=cmd_fill)
    p = sub.add_parser("import-pairs", help="two-line roman/Bangla pairs separated by blank lines")
    p.add_argument("file")
    p.add_argument("--tag", default="")
    p.set_defaults(fn=cmd_import_pairs)
    p = sub.add_parser(
        "import-whatsapp", help="your own Banglish lines from a WhatsApp chat export"
    )
    p.add_argument("file")
    p.add_argument("--me", required=True, help="your display name exactly as in the export")
    p.add_argument("--out")
    p.add_argument("--min-words", type=int, default=3)
    p.add_argument("--limit", type=int, default=400)
    p.set_defaults(fn=cmd_import_whatsapp)
    sub.add_parser("stats").set_defaults(fn=cmd_stats)
    sub.add_parser("validate").set_defaults(fn=cmd_validate)
    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
