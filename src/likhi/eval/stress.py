"""likhi-stress: thousands of words, many typing habits.

Takes words with human romanizations (Dakshina test, BanglaTLit test words, the feedback set) and
rewrites each romanization the way different users type, then measures top-1/top-5 per habit and
lists the systematic misses. Styles:

  as_is        the attested romanization unchanged
  drop_vowels  inherent/short vowels dropped inside the word:  amar -> amr, tumi -> tmi
  double       long vowels doubled:                             amar -> aamar, ki -> kii
  a_for_o      o written as a (and vice versa) in the middle:   korchi -> karchi
  c_for_ch     ch -> c, chh -> ch:                               korchi -> korci
  s_for_sh     sh -> s, ss -> s:                                 shopno -> sopno
  y_for_j      j -> y / z, jh -> j:                              jonno -> yonno
  v_for_bh     bh -> v, ph -> f, w -> o:                         bhalo -> valo
  caps         random capitalisation:                           Amar, aMAR
  typo         one adjacent-key substitution (qwerty):          amar -> amsr
  no_h         h dropped after consonants:                       khub -> kub, dhonnobad -> donnobad

Usage:
  likhi-stress run --limit 1500 --shard 0/4        (run one shard, writes results/stress_*.json)
  likhi-stress report                               (aggregate all stress result files)
"""

from __future__ import annotations

import argparse
import json
import random
import re
import sys
import time
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path

from likhi.eval import datasets as ds
from likhi.eval.metrics import percentile, rank_of_gold
from likhi.eval.systems import load_system

RESULTS = Path(__file__).resolve().parents[3] / "results"

_QWERTY_NEIGHBOURS = {
    "q": "wa",
    "w": "qes",
    "e": "wrd",
    "r": "etf",
    "t": "ryg",
    "y": "tuh",
    "u": "yij",
    "i": "uok",
    "o": "ipl",
    "p": "o",
    "a": "qsz",
    "s": "awdz",
    "d": "sefx",
    "f": "drgc",
    "g": "fthv",
    "h": "gyjb",
    "j": "hukn",
    "k": "jilm",
    "l": "kop",
    "z": "asx",
    "x": "zsdc",
    "c": "xdfv",
    "v": "cfgb",
    "b": "vghn",
    "n": "bhjm",
    "m": "njk",
}
_VOWELS = "aeiou"


def _drop_vowels(r: str, rng: random.Random) -> str:
    # drop each interior a/o with prob 0.7, keep first char and last char
    if len(r) < 4:
        return r
    out = [r[0]]
    for ch in r[1:-1]:
        if ch in "ao" and rng.random() < 0.7:
            continue
        out.append(ch)
    out.append(r[-1])
    return "".join(out)


def _double(r: str, rng: random.Random) -> str:
    out = []
    for ch in r:
        out.append(ch)
        if ch in "aiu" and rng.random() < 0.5:
            out.append(ch)
    return "".join(out)


def _a_for_o(r: str, rng: random.Random) -> str:
    if len(r) < 3:
        return r
    core = list(r[1:-1])
    for i, ch in enumerate(core):
        if ch == "o" and rng.random() < 0.6:
            core[i] = "a"
        elif ch == "a" and rng.random() < 0.3:
            core[i] = "o"
    return r[0] + "".join(core) + r[-1]


def _c_for_ch(r: str, rng: random.Random) -> str:
    return r.replace("chh", "ch").replace("ch", "c")


def _s_for_sh(r: str, rng: random.Random) -> str:
    return r.replace("sh", "s").replace("ss", "s")


def _y_for_j(r: str, rng: random.Random) -> str:
    r = r.replace("jh", "j")
    return re.sub(r"j", lambda m: rng.choice("yz"), r)


def _v_for_bh(r: str, rng: random.Random) -> str:
    return r.replace("bh", "v").replace("ph", "f").replace("w", "o")


def _caps(r: str, rng: random.Random) -> str:
    return "".join(ch.upper() if rng.random() < 0.3 else ch for ch in r)


def _typo(r: str, rng: random.Random) -> str:
    if len(r) < 4:
        return r
    i = rng.randrange(1, len(r) - 1)
    nb = _QWERTY_NEIGHBOURS.get(r[i])
    if not nb:
        return r
    return r[:i] + rng.choice(nb) + r[i + 1 :]


def _no_h(r: str, rng: random.Random) -> str:
    return re.sub(r"(?<=[kgcjtdpb])h", "", r)


STYLES = {
    "as_is": lambda r, rng: r,
    "drop_vowels": _drop_vowels,
    "double": _double,
    "a_for_o": _a_for_o,
    "c_for_ch": _c_for_ch,
    "s_for_sh": _s_for_sh,
    "y_for_j": _y_for_j,
    "v_for_bh": _v_for_bh,
    "caps": _caps,
    "typo": _typo,
    "no_h": _no_h,
}


def build_items(limit_per_source: int, seed: int) -> list[dict]:
    rng = random.Random(seed)
    items: list[dict] = []
    sources = {
        "dakshina": ds.dakshina_lexicon("test", grouped=True).items,
        "chat": ds.banglatlit_word_pairs("test", grouped=True).items,
        "feedback": ds.feedback_words().items,
    }
    for src, its in sources.items():
        its = list(its)
        rng.shuffle(its)
        for it in its[:limit_per_source]:
            for style, fn in STYLES.items():
                variant = fn(it.roman, rng)
                if not variant or not re.search(r"[A-Za-z]", variant):
                    continue
                items.append(
                    {
                        "src": src,
                        "style": style,
                        "roman": variant,
                        "orig": it.roman,
                        "golds": list(it.golds),
                    }
                )
    return items


def cmd_run(args: argparse.Namespace) -> int:
    from likhi.engine.threads import limit_blas_threads

    limit_blas_threads()
    items = build_items(args.limit, args.seed)
    shard_i, shard_n = (int(x) for x in args.shard.split("/"))
    items = [it for i, it in enumerate(items) if i % shard_n == shard_i]
    system = load_system(args.system)
    t0 = time.time()
    out = []
    lat = []
    for n, it in enumerate(items, 1):
        s = time.perf_counter()
        cands = system.suggest(it["roman"], k=5)
        lat.append((time.perf_counter() - s) * 1000)
        rank = rank_of_gold(cands, it["golds"])
        out.append({**it, "got": cands, "rank": rank})
        if n % 500 == 0:
            print(f"[stress] {n}/{len(items)} {time.time() - t0:.0f}s", flush=True)
    RESULTS.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    path = RESULTS / f"stress_{stamp}_{args.system}_shard{shard_i}of{shard_n}.json"
    path.write_text(
        json.dumps(
            {
                "system": args.system,
                "seed": args.seed,
                "limit_per_source": args.limit,
                "shard": args.shard,
                "latency_p50": percentile(lat, 50),
                "latency_p95": percentile(lat, 95),
                "items": out,
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    print(f"[stress] wrote {len(out)} items to {path} in {time.time() - t0:.0f}s")
    return 0


def cmd_report(args: argparse.Namespace) -> int:
    files = sorted(RESULTS.glob("stress_*.json"))
    if args.latest:
        # keep only files from the most recent run (same stamp prefix up to the shard part)
        stamps = sorted({f.name.split("_")[1] for f in files})
        files = [f for f in files if f.name.split("_")[1] == stamps[-1]] if stamps else []
    items: list[dict] = []
    for f in files:
        items.extend(json.loads(f.read_text(encoding="utf-8"))["items"])
    if not items:
        print("no stress results")
        return 1
    by = defaultdict(lambda: [0, 0, 0])  # n, top1, top5
    for it in items:
        for key in (
            (it["src"], it["style"]),
            ("ALL", it["style"]),
            (it["src"], "ALL"),
            ("ALL", "ALL"),
        ):
            b = by[key]
            b[0] += 1
            b[1] += it["rank"] == 1
            b[2] += it["rank"] is not None and it["rank"] <= 5
    print(f"{'source':10} {'style':12} {'n':>6} {'top1':>6} {'top5':>6}")
    for (src, style), (n, t1, t5) in sorted(by.items()):
        print(f"{src:10} {style:12} {n:6} {100 * t1 / n:6.1f} {100 * t5 / n:6.1f}")
    # systematic misses: words whose as_is form is right but a style breaks it (habit-specific)
    ok_as_is = {
        (it["src"], it["orig"]) for it in items if it["style"] == "as_is" and it["rank"] == 1
    }
    broken: Counter[str] = Counter()
    examples: dict[str, list[str]] = defaultdict(list)
    for it in items:
        if it["style"] != "as_is" and (it["src"], it["orig"]) in ok_as_is and it["rank"] is None:
            broken[it["style"]] += 1
            if len(examples[it["style"]]) < args.examples:
                examples[it["style"]].append(
                    f"{it['orig']} -> {it['roman']}: got {it['got'][:3]} wanted {it['golds'][:2]}"
                )
    print("\nHabits that break words the engine otherwise gets right (gold missing from top-5):")
    for style, n in broken.most_common():
        print(f"  {style:12} {n}")
        for ex in examples[style]:
            print(f"      {ex}")
    # misses even in the plain form
    plain_miss = [it for it in items if it["style"] == "as_is" and it["rank"] is None]
    print(f"\nPlain-form words with gold outside top-5: {len(plain_miss)}")
    for it in plain_miss[: args.examples * 2]:
        print(f"      {it['roman']}: got {it['got'][:3]} wanted {it['golds'][:2]}")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="likhi-stress",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--system", default="likhi")
    r.add_argument("--limit", type=int, default=1500, help="words per source before styles")
    r.add_argument("--seed", type=int, default=7)
    r.add_argument("--shard", default="0/1")
    r.set_defaults(fn=cmd_run)
    p = sub.add_parser("report")
    p.add_argument("--latest", action="store_true", help="only the most recent run")
    p.add_argument("--examples", type=int, default=6)
    p.set_defaults(fn=cmd_report)
    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
