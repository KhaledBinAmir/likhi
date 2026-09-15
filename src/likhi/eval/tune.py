"""Tune the ranker weights on dev sets by coordinate ascent.

Features for every dev item are computed once (the expensive part: tries + model scoring of the
top-N candidates under a weight-independent pre-ranking) and cached to disk. The search then only
re-scores cached features, so hundreds of weight settings cost seconds.

    likhi-tune cache   --dataset dakshina-dev:1500 --dataset banglatlit-val-words --dataset aksharantar-valid:800
    likhi-tune search  --metric top1
"""

from __future__ import annotations

import argparse
import json
import math
import pickle
import random
import sys
import time
from dataclasses import asdict
from pathlib import Path

from likhi.engine.core import DEFAULT_WEIGHTS, LikhiEngine
from likhi.engine.textnorm import match_key
from likhi.eval import datasets as ds

REPO = Path(__file__).resolve().parents[3]
CACHE = REPO / "data" / "cache" / "tune_features.pkl"
WEIGHTS_OUT = REPO / "models" / "lexicon" / "weights.json"


def _load_items(spec: str, seed: int = 13, augment: list[str] | None = None):
    """``spec`` = name[:sample]. With ``augment``, each item is also rewritten in the given typing
    habits (see likhi.eval.stress.STYLES) so the tuned weights reflect how people actually type,
    not only the attested spellings."""
    name, _, n = spec.partition(":")
    ws = ds.load_wordset(name)
    items = list(ws.items)
    rng = random.Random(seed)
    if n:
        rng.shuffle(items)
        items = items[: int(n)]
    if augment:
        from likhi.eval.stress import STYLES

        extra = []
        for it in items:
            for style in augment:
                variant = STYLES[style](it.roman, rng)
                if variant and variant != it.roman:
                    extra.append(ds.WordItem(variant, it.golds, it.weight, f"{it.source}+{style}"))
        items = items + extra
    return ws.name, items


def cmd_cache(args: argparse.Namespace) -> int:
    engine = LikhiEngine()
    rows = []
    t0 = time.time()
    augment = [s for s in (args.augment or "").split(",") if s]
    for spec in args.dataset:
        name, items = _load_items(spec, augment=augment)
        print(f"[tune] {name}: {len(items)} items", flush=True)
        for i, it in enumerate(items):
            feats = engine.candidates(
                it.roman, model_scored=args.model_scored, static_prescore=True
            )
            rows.append(
                {
                    "set": name,
                    "roman": it.roman,
                    "golds": [match_key(g) for g in it.golds],
                    "weight": it.weight,
                    "cands": {
                        w: (asdict(ft) | {"sources": sorted(ft.sources)}, engine.unigram_logp(w))
                        for w, ft in feats.items()
                    },
                }
            )
            if (i + 1) % 200 == 0:
                print(f"[tune]   {i + 1}/{len(items)}  {(time.time() - t0):.0f}s", flush=True)
    CACHE.parent.mkdir(parents=True, exist_ok=True)
    with open(CACHE, "wb") as f:
        pickle.dump(rows, f)
    print(f"[tune] cached {len(rows)} items to {CACHE} in {time.time() - t0:.0f}s")
    return 0


UNKNOWN_CONFIDENT_LOGP = -11.0  # keep in sync with likhi.engine.core


def _score(word: str, ft: dict, uni: float, w: dict[str, float]) -> float:
    """Mirror of LikhiEngine.score over cached feature dicts (keep the two in sync)."""
    from likhi.engine.core import looks_like_acronym

    xl = ft["xlit_logp"]
    acronym = (not ft["in_lexicon"]) and looks_like_acronym(word)
    if not ft["in_lexicon"] and xl == xl and not acronym:
        confidence = 1.0 - min(1.0, max(0.0, -xl / 3.0))
        uni = uni + (UNKNOWN_CONFIDENT_LOGP - uni) * confidence
    s = w["unigram"] * uni
    if ft["rom_exact"]:
        s += w["rom_exact"] + w["rom_exact_log"] * math.log(1 + ft["rom_exact"])
    if ft["rom_prefix"] and not ft["rom_exact"]:
        s += w["rom_prefix"] * math.log(1 + ft["rom_prefix"])
    if ft["key_fine"]:
        s += w["key_fine"]
    elif ft["key_coarse"]:
        s += w["key_coarse"]
    elif ft["key_fine_prefix"] or ft["key_coarse_prefix"]:
        s += w["key_prefix"]
    xl = ft["xlit_logp"]
    if xl == xl:
        s += w["xlit_logp"] * max(-40.0, xl)
    else:
        s += w["xlit_logp"] * -15.0
    gap = ft.get("gap", 0)
    if gap and not ft["rom_exact"]:
        s += w["gap"] * gap
    if ft["xlit_rank"] == 1:
        s += w["xlit_top1"]
    elif 1 < ft["xlit_rank"] <= 3:
        s += w["xlit_top3"]
    if ft["avro"]:
        s += w["avro"]
    if not ft["in_lexicon"]:
        scale = 1.0 if (xl != xl or acronym) else min(1.0, max(0.0, -xl / 3.0))
        s += w["oov"] * scale
    return s


def _evaluate(rows: list[dict], w: dict[str, float], metric: str) -> dict[str, float]:
    per_set: dict[str, list[float]] = {}
    for row in rows:
        golds = set(row["golds"])
        ranked = sorted(row["cands"].items(), key=lambda kv: -_score(kv[0], kv[1][0], kv[1][1], w))
        rank = next((i for i, (word, _) in enumerate(ranked, 1) if match_key(word) in golds), None)
        if metric == "top1":
            v = 1.0 if rank == 1 else 0.0
        elif metric == "top3":
            v = 1.0 if rank is not None and rank <= 3 else 0.0
        else:  # mrr
            v = 1.0 / rank if rank else 0.0
        per_set.setdefault(row["set"], []).append(v)
    out = {k: 100.0 * sum(v) / len(v) for k, v in per_set.items()}
    out["macro"] = sum(out.values()) / len(out)
    return out


GRID = {
    "unigram": [0.5, 0.75, 1.0, 1.25, 1.5],
    "rom_exact": [0.0, 1.0, 2.0, 3.0, 4.0, 6.0],
    "rom_exact_log": [0.0, 0.4, 0.8, 1.2, 1.6],
    "rom_prefix": [-0.5, 0.0, 0.3, 0.6],
    "key_fine": [0.0, 0.6, 1.2, 2.0, 3.0],
    "key_coarse": [0.0, 0.3, 0.6, 1.2, 2.0],
    "key_prefix": [-3.0, -2.0, -1.0, 0.0],
    "gap": [-1.5, -1.0, -0.6, -0.4, -0.2, 0.0],
    "xlit_logp": [0.1, 0.2, 0.35, 0.5, 0.75, 1.0, 1.5],
    "xlit_top1": [0.0, 1.0, 1.5, 2.5, 4.0],
    "xlit_top3": [0.0, 0.5, 1.0, 2.0],
    "avro": [0.0, 0.5, 1.0, 2.0],
    "oov": [-6.0, -4.0, -3.0, -2.0, -1.0, 0.0],
}


def cmd_search(args: argparse.Namespace) -> int:
    with open(CACHE, "rb") as f:
        rows = pickle.load(f)
    w = dict(DEFAULT_WEIGHTS)
    if args.start and Path(args.start).exists():
        w.update(json.loads(Path(args.start).read_text(encoding="utf-8")))
    best = _evaluate(rows, w, args.metric)
    print(f"[tune] start {args.metric}: {json.dumps({k: round(v, 2) for k, v in best.items()})}")
    for rnd in range(args.rounds):
        improved = False
        for name, values in GRID.items():
            cur = w[name]
            cand_best = (best["macro"], cur)
            for v in values:
                if v == cur:
                    continue
                w[name] = v
                r = _evaluate(rows, w, args.metric)
                if r["macro"] > cand_best[0] + 1e-9:
                    cand_best = (r["macro"], v)
            w[name] = cand_best[1]
            if cand_best[1] != cur:
                improved = True
                best = _evaluate(rows, w, args.metric)
                print(
                    f"[tune] round {rnd + 1}: {name}={cand_best[1]} -> macro {best['macro']:.2f}",
                    flush=True,
                )
        if not improved:
            break
    print(f"[tune] final {args.metric}: {json.dumps({k: round(v, 2) for k, v in best.items()})}")
    print("[tune] weights:", json.dumps(w))
    WEIGHTS_OUT.parent.mkdir(parents=True, exist_ok=True)
    WEIGHTS_OUT.write_text(json.dumps(w, indent=1), encoding="utf-8")
    print(f"[tune] saved to {WEIGHTS_OUT}")
    return 0


def main(argv: list[str] | None = None) -> int:
    from likhi.engine.threads import limit_blas_threads

    limit_blas_threads()
    ap = argparse.ArgumentParser(
        prog="likhi-tune", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("cache")
    c.add_argument("--dataset", action="append", required=True, help="name[:sample_size]")
    c.add_argument("--model-scored", type=int, default=40)
    c.add_argument(
        "--augment",
        default="",
        help="comma-separated typing habits, e.g. drop_vowels,a_for_o,double",
    )
    c.set_defaults(fn=cmd_cache)
    s = sub.add_parser("search")
    s.add_argument("--metric", default="top1", choices=["top1", "top3", "mrr"])
    s.add_argument("--rounds", type=int, default=4)
    s.add_argument("--start", help="weights json to start from")
    s.set_defaults(fn=cmd_search)
    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
