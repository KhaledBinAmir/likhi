"""likhi-eval: measure a system on the standard word and sentence sets.

Examples:
    likhi-eval words --system avro --dataset dakshina-test --dataset aksharantar-test
    likhi-eval sentences --system avro --dataset banglatlit-test
    likhi-eval report
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path

from likhi.engine.textnorm import has_bengali, match_key, normalize_roman
from likhi.eval import datasets as ds
from likhi.eval.metrics import WordEval, percentile, wer
from likhi.eval.systems import load_system

RESULTS = Path(__file__).resolve().parents[3] / "results"


def _git_rev() -> str:
    try:
        return subprocess.check_output(["git", "rev-parse", "--short", "HEAD"], text=True).strip()
    except Exception:
        return "unknown"


def _save(result: dict) -> Path:
    RESULTS.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    path = RESULTS / f"{stamp}_{result['kind']}_{result['system']}_{result['dataset']}.json"
    path.write_text(json.dumps(result, ensure_ascii=False, indent=1), encoding="utf-8")
    return path


def eval_words(system_name: str, dataset: str, k: int, limit: int | None, errors: int) -> dict:
    system = load_system(system_name)
    wordset = ds.load_wordset(dataset)
    items = wordset.items[:limit] if limit else wordset.items
    ev = WordEval(ks=tuple(sorted({1, 3, 5, k})))
    lat: list[float] = []
    misses: list[dict] = []
    err_sources: Counter[str] = Counter()
    t0 = time.perf_counter()
    for it in items:
        s = time.perf_counter()
        cands = system.suggest(it.roman, k=k)
        lat.append((time.perf_counter() - s) * 1000)
        rank = ev.add(cands, it.golds, it.weight)
        if rank != 1:
            err_sources[it.source] += 1
            if len(misses) < errors:
                misses.append(
                    {"roman": it.roman, "gold": list(it.golds), "got": cands[:k], "rank": rank}
                )
    summary = ev.summary()
    result = {
        "kind": "words",
        "system": system_name,
        "dataset": wordset.name,
        "k": k,
        **summary,
        "latency_ms": {
            "p50": percentile(lat, 50),
            "p95": percentile(lat, 95),
            "p99": percentile(lat, 99),
            "mean": sum(lat) / len(lat) if lat else 0.0,
        },
        "elapsed_s": time.perf_counter() - t0,
        "git": _git_rev(),
        "when": datetime.now(UTC).isoformat(timespec="seconds"),
        "miss_by_source": dict(err_sources),
        "sample_misses": misses,
    }
    return result


_RE_HAS_LATIN = re.compile(r"[A-Za-z]")
# Same caveat as in datasets.py: never let \W eat Bengali combining marks.
_RE_EDGE_PUNCT = re.compile(r"^[^\wঀ-৿‌‍]+|[^\wঀ-৿‌‍]+$")


def _transliterate_sentence(system, roman: str) -> list[str]:
    out: list[str] = []
    ctx: list[str] = []
    for tok in roman.split():
        core = _RE_EDGE_PUNCT.sub("", tok)
        if not core or not _RE_HAS_LATIN.search(core):
            if core:
                out.append(core)
            continue
        cands = system.suggest(normalize_roman(core), context=tuple(ctx[-2:]), k=1)
        word = cands[0] if cands else core
        out.append(word)
        ctx.append(word)
    return out


def eval_sentences(system_name: str, dataset: str, limit: int | None, errors: int) -> dict:
    system = load_system(system_name)
    sents = ds.load_sentences(dataset)
    if limit:
        sents = sents[:limit]
    err_total = ref_total = 0
    bn_err = bn_ref = 0
    misses: list[dict] = []
    t0 = time.perf_counter()
    for s in sents:
        hyp = _transliterate_sentence(system, s.roman)
        ref = [t for t in (_RE_EDGE_PUNCT.sub("", x) for x in s.gold.split()) if t]
        e, n = wer(hyp, ref)
        err_total += e
        ref_total += n
        # Bengali-only view: ignore tokens the gold keeps in Latin (URLs, English), which no
        # transliterator should touch.
        ref_bn = [t for t in ref if has_bengali(t)]
        hyp_bn = [t for t in hyp if has_bengali(t)]
        e2, n2 = wer(hyp_bn, ref_bn)
        bn_err += e2
        bn_ref += n2
        if e and len(misses) < errors:
            misses.append({"roman": s.roman, "gold": s.gold, "hyp": " ".join(hyp)})
    result = {
        "kind": "sentences",
        "system": system_name,
        "dataset": dataset,
        "n": len(sents),
        "wer": 100.0 * err_total / max(1, ref_total),
        "wer_bengali_tokens": 100.0 * bn_err / max(1, bn_ref),
        "ref_tokens": ref_total,
        "elapsed_s": time.perf_counter() - t0,
        "git": _git_rev(),
        "when": datetime.now(UTC).isoformat(timespec="seconds"),
        "sample_misses": misses,
    }
    return result


def eval_replay(system_name: str, dataset: str, k: int, limit: int | None) -> dict:
    """Keystroke replay: type each word letter by letter and record when the gold first appears.

    Reports the share of keystrokes a typist could skip by committing as soon as the right word
    is top-1 (or within top-k), plus per-keystroke latency percentiles: the numbers that decide
    whether the keyboard *feels* like Gboard.
    """
    system = load_system(system_name)
    wordset = ds.load_wordset(dataset)
    items = wordset.items[:limit] if limit else wordset.items
    lat: list[float] = []
    saved_top1 = saved_topk = 0.0
    total_keys = 0
    never_top1 = never_topk = 0
    for it in items:
        roman = it.roman
        n = len(roman)
        if n == 0:
            continue
        total_keys += n
        first1 = firstk = None
        for i in range(1, n + 1):
            s = time.perf_counter()
            cands = system.suggest(roman[:i], k=k)
            lat.append((time.perf_counter() - s) * 1000)
            rank = None
            gold_keys = {match_key(g) for g in it.golds}
            for j, c in enumerate(cands, 1):
                if match_key(c) in gold_keys:
                    rank = j
                    break
            if rank == 1 and first1 is None:
                first1 = i
            if rank is not None and rank <= k and firstk is None:
                firstk = i
            if first1 is not None and firstk is not None:
                break
        if first1 is None:
            never_top1 += 1
        else:
            saved_top1 += n - first1
        if firstk is None:
            never_topk += 1
        else:
            saved_topk += n - firstk
    return {
        "kind": "replay",
        "system": system_name,
        "dataset": wordset.name,
        "k": k,
        "n": len(items),
        "keystrokes": total_keys,
        "saved_top1_pct": 100.0 * saved_top1 / max(1, total_keys),
        f"saved_top{k}_pct": 100.0 * saved_topk / max(1, total_keys),
        "never_top1_pct": 100.0 * never_top1 / max(1, len(items)),
        f"never_top{k}_pct": 100.0 * never_topk / max(1, len(items)),
        "latency_ms": {
            "p50": percentile(lat, 50),
            "p95": percentile(lat, 95),
            "p99": percentile(lat, 99),
            "mean": sum(lat) / len(lat) if lat else 0.0,
        },
        "git": _git_rev(),
        "when": datetime.now(UTC).isoformat(timespec="seconds"),
    }


def report() -> None:
    rows = []
    for p in sorted(RESULTS.glob("*.json")):
        r = json.loads(p.read_text(encoding="utf-8"))
        rows.append(r)
    if not rows:
        print("no results yet")
        return
    print(
        f"{'when':20} {'kind':9} {'system':10} {'dataset':28} {'n':>7} {'top1':>6} {'top3':>6} {'top5':>6} {'cer':>6} {'wer':>6} {'p95ms':>7}"
    )
    for r in rows:
        lat = r.get("latency_ms", {})
        print(
            f"{r['when'][:19]:20} {r['kind']:9} {r['system']:10} {r['dataset'][:28]:28} {r.get('n', 0):>7} "
            f"{r.get('top1', float('nan')):6.1f} {r.get('top3', float('nan')):6.1f} {r.get('top5', float('nan')):6.1f} "
            f"{r.get('cer', float('nan')):6.1f} {r.get('wer', float('nan')):6.1f} {lat.get('p95', float('nan')):7.2f}"
        )


def _print_result(r: dict) -> None:
    keys = [
        k
        for k in (
            "n",
            "top1",
            "top3",
            "top5",
            "mrr",
            "cer",
            "wer",
            "wer_bengali_tokens",
            "keystrokes",
            "saved_top1_pct",
            "saved_top5_pct",
            "never_top1_pct",
            "never_top5_pct",
        )
        if k in r
    ]
    parts = [f"{k}={r[k]:.2f}" if isinstance(r[k], float) else f"{k}={r[k]}" for k in keys]
    lat = r.get("latency_ms")
    if lat:
        parts.append(f"p50={lat['p50']:.2f}ms p95={lat['p95']:.2f}ms")
    print(f"[{r['system']} on {r['dataset']}] " + "  ".join(parts))
    for m in r.get("sample_misses", [])[:10]:
        print("   miss:", json.dumps(m, ensure_ascii=False))


def main(argv: list[str] | None = None) -> int:
    from likhi.engine.threads import limit_blas_threads

    limit_blas_threads()
    ap = argparse.ArgumentParser(
        prog="likhi-eval", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    w = sub.add_parser("words", help="word-level top-k accuracy, CER, MRR, latency")
    w.add_argument("--system", required=True)
    w.add_argument("--dataset", action="append", required=True)
    w.add_argument("--k", type=int, default=5)
    w.add_argument("--limit", type=int)
    w.add_argument(
        "--errors", type=int, default=25, help="how many misses to keep in the result file"
    )
    w.add_argument("--no-save", action="store_true")

    s = sub.add_parser("sentences", help="sentence-level WER with word-by-word transliteration")
    s.add_argument("--system", required=True)
    s.add_argument("--dataset", action="append", required=True)
    s.add_argument("--limit", type=int)
    s.add_argument("--errors", type=int, default=25)
    s.add_argument("--no-save", action="store_true")

    rp = sub.add_parser("replay", help="keystroke-by-keystroke replay: savings and per-key latency")
    rp.add_argument("--system", required=True)
    rp.add_argument("--dataset", action="append", required=True)
    rp.add_argument("--k", type=int, default=5)
    rp.add_argument("--limit", type=int)
    rp.add_argument("--no-save", action="store_true")

    sub.add_parser("report", help="table of all saved results")

    args = ap.parse_args(argv)
    if args.cmd == "report":
        report()
        return 0
    for dataset in args.dataset:
        if args.cmd == "words":
            r = eval_words(args.system, dataset, args.k, args.limit, args.errors)
        elif args.cmd == "replay":
            r = eval_replay(args.system, dataset, args.k, args.limit)
        else:
            r = eval_sentences(args.system, dataset, args.limit, args.errors)
        _print_result(r)
        if not args.no_save:
            print("   saved:", _save(r))
    return 0


if __name__ == "__main__":
    sys.exit(main())
