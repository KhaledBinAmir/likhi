"""Dump the Python engine's exact behaviour as golden vectors for the Rust port.

    python scripts/dump_goldens.py                  # -> tests/goldens/*.jsonl
    python scripts/dump_goldens.py --quick          # small sets, for a fast loop

The Rust rewrite has one hard requirement that no amount of code review can establish: it must
suggest the same words, in the same order, as the engine people are already using. These files are
how that is checked. Each line is one call: the inputs, and exactly what Python returned.

Two classes of golden, checked differently on the Rust side:

* string and integer functions (normalization, phonetic keys, Avro, lexicon counts) must match
  EXACTLY. There is no floating point in them and no excuse for a difference.
* transformer outputs (beam search, teacher-forced scores) are float32 matrix products, and a
  different summation order changes the last bits. Those are checked on *ranking* -- same words in
  the same order -- with scores compared to a tolerance, which is the property that actually
  matters to a typist.

Inputs are drawn from the Dakshina dev lexicon (real romanizations by real people), the pilot
feedback words (the cases that were reported as wrong), and a hand-written list of edge cases that
a port is likely to break: empty strings, punctuation, digits, mixed scripts, very long input.
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "src"))

OUT = REPO / "tests" / "goldens"

# Deliberately nasty: every one of these has broken a string function at some point, or is the kind
# of input that reaches the engine from a real keyboard rather than from a corpus.
EDGE_ROMAN = [
    "",
    " ",
    "a",
    "A",
    "AMAR",
    "aMaR",
    "amar ",
    " amar",
    "amar123",
    "123",
    "a'b",
    "ma'am",
    "maam",
    "mam",
    "myam",
    "don't",
    "hello-world",
    "e.mail",
    "x" * 64,
    "amar\tamar",
    "amar\namar",
    "café",
    "naïve",
    "ঢাকা",
    "amarআমার",
    "!!!",
    "___",
    "0",
    "007",
    "rapid",
    "bajay",
    "chiro",
    "chirodin",
    "koria",
    "shoytan",
    "khacche",
    "khaitecho",
    "bangla",
    "sonar",
    "amr",
    "tmi",
    "korci",
    "korchi",
]

# Bengali strings for the functions that consume them (canonical / to_output / key_from_bangla).
EDGE_BANGLA = [
    "",
    "আমার",
    "ড়",              # precomposed nukta
    "ড" + "়",     # decomposed nukta - must canonicalise to the same thing
    "ঢ়",
    "ঢ" + "়",
    "য়",
    "য" + "়",
    "ত্‍",    # legacy khanda-ta sequence
    "ৎ",
    "কঁা",              # candrabindu before the vowel sign - must be reordered
    "কাঁ",              # ... and this is what it must become
    "র‍্য",   # ZWJ ra-ya (র‍্য)
    "র্য",         # without the joiner
    "ৰ",                # Assamese ra
    "ৱ",                # Assamese wa
    "০১২৩৪৫৬৭৮৯",
    "খাচ্ছে",
    "খাইতেছো",
    "আমারআমার",
    "a আমার b",
]


def log(msg: str) -> None:
    print(f"[goldens] {msg}", flush=True)


def dakshina_romans(limit: int) -> list[str]:
    """Real romanizations from the Dakshina dev lexicon, deduplicated, in file order."""
    path = REPO / "data" / "raw" / "dakshina" / "bn" / "lexicons" / "bn.translit.sampled.dev.tsv"
    if not path.exists():
        log(f"missing {path}; skipping corpus inputs")
        return []
    seen: dict[str, None] = {}
    with path.open(encoding="utf-8", newline="") as fh:
        for row in csv.reader(fh, delimiter="\t"):
            # Dakshina lexicons are <native> <roman> [count]; guard against layout drift rather
            # than trusting the column index.
            if len(row) < 2:
                continue
            roman = row[1].strip()
            if roman and roman.isascii() and roman not in seen:
                seen[roman] = None
            if len(seen) >= limit:
                break
    return list(seen)


def feedback_romans() -> list[str]:
    path = REPO / "data" / "feedback" / "words.jsonl"
    if not path.exists():
        return []
    out: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        r = rec.get("roman") or rec.get("input") or ""
        if r:
            out.append(r)
    return out


def dakshina_words(limit: int) -> list[str]:
    path = REPO / "data" / "raw" / "dakshina" / "bn" / "lexicons" / "bn.translit.sampled.dev.tsv"
    if not path.exists():
        return []
    seen: dict[str, None] = {}
    with path.open(encoding="utf-8", newline="") as fh:
        for row in csv.reader(fh, delimiter="\t"):
            if not row:
                continue
            w = row[0].strip()
            if w and w not in seen:
                seen[w] = None
            if len(seen) >= limit:
                break
    return list(seen)


class Writer:
    """One JSONL file, written deterministically so goldens diff cleanly between runs."""

    def __init__(self, name: str) -> None:
        OUT.mkdir(parents=True, exist_ok=True)
        self.path = OUT / f"{name}.jsonl"
        self.fh = self.path.open("w", encoding="utf-8", newline="\n")
        self.n = 0

    def add(self, record: dict) -> None:
        # sort_keys so the field order never depends on dict insertion; ensure_ascii so the file is
        # pure ASCII and cannot be corrupted by a tool guessing the wrong code page (which has
        # already happened once in this repo, to LikhiApp.cs).
        self.fh.write(json.dumps(record, sort_keys=True, ensure_ascii=True) + "\n")
        self.n += 1

    def close(self) -> None:
        self.fh.close()
        log(f"{self.path.name}: {self.n} cases")


def dump_textnorm(romans: list[str], banglas: list[str]) -> None:
    from likhi.engine import textnorm as tn

    w = Writer("textnorm")
    for s in romans:
        w.add({"fn": "normalize_roman", "in": s, "out": tn.normalize_roman(s)})
    for s in banglas:
        w.add({"fn": "canonical", "in": s, "out": tn.canonical(s)})
        w.add({"fn": "match_key", "in": s, "out": tn.match_key(s)})
        w.add({"fn": "to_output", "in": s, "out": tn.to_output(s)})
        w.add({"fn": "has_bengali", "in": s, "out": tn.has_bengali(s)})
        w.add({"fn": "to_bangla_digits", "in": s, "out": tn.to_bangla_digits(s)})
        w.add({"fn": "to_western_digits", "in": s, "out": tn.to_western_digits(s)})
    w.close()


def dump_romankey(romans: list[str], banglas: list[str]) -> None:
    from likhi.engine.romankey import key_from_bangla, key_from_roman

    w = Writer("romankey")
    for level in ("fine", "coarse"):
        for s in romans:
            w.add({"fn": "key_from_roman", "in": s, "level": level, "out": key_from_roman(s, level)})
        for s in banglas:
            w.add(
                {"fn": "key_from_bangla", "in": s, "level": level, "out": key_from_bangla(s, level)}
            )
    w.close()


def dump_avro(romans: list[str]) -> None:
    try:
        import avro
    except ImportError:
        log("avro not importable; skipping avro goldens")
        return
    w = Writer("avro")
    for s in romans:
        try:
            out = avro.parse(s)
        except Exception as exc:  # the engine treats any failure as "no rule literal"
            w.add({"in": s, "error": type(exc).__name__})
            continue
        w.add({"in": s, "out": out})
    w.close()


def dump_lexicon(engine, words: list[str]) -> None:
    w = Writer("lexicon")
    w.add({"fn": "uni_total", "value": repr(engine._uni_total)})
    w.add({"fn": "floor", "value": repr(engine._floor)})
    for word in words:
        w.add(
            {
                "fn": "lookup",
                "word": word,
                "lex_score": engine._lex_score(word),
                "in_lexicon": word in engine.uni,
                "unigram_logp": repr(engine.unigram_logp(word)),
            }
        )
    w.close()


def dump_xlit(engine, romans: list[str], beam: int) -> None:
    xlit = engine.xlit
    if xlit is None:
        log("no transliteration model; skipping xlit goldens")
        return

    wb = Writer("xlit_beam")
    for r in romans:
        norm = r  # already normalised by the caller
        try:
            hyps = xlit.beam_search(norm, beam=beam, nbest=beam)
        except Exception as exc:
            wb.add({"roman": norm, "beam": beam, "error": type(exc).__name__})
            continue
        wb.add(
            {
                "roman": norm,
                "beam": beam,
                "hyps": [[word, repr(float(score))] for word, score in hyps],
            }
        )
    wb.close()

    # Teacher-forced scoring is the other half of the model's contribution to ranking, and it is
    # scored against words the beam did NOT produce, so it needs its own cases.
    ws = Writer("xlit_score")
    for r in romans:
        cands = [word for word, _ in xlit.beam_search(r, beam=beam, nbest=beam)]
        extra = [w for w in ("আমার", "আমি", "তুমি", "করছি", "খাচ্ছে") if w not in cands]
        words = (cands + extra)[:8]
        if not words:
            continue
        lps = xlit.score_candidates(r, words)
        ws.add({"roman": r, "words": words, "logps": [repr(float(x)) for x in lps]})
    ws.close()

    wt = Writer("xlit_tokenize")
    for r in romans:
        wt.add({"roman": r, "src_ids": xlit.encode_source(r)})
    wt.close()


def dump_suggest(engine, romans: list[str]) -> None:
    """The acceptance test: the whole pipeline, exactly as the shell calls it."""
    w = Writer("suggest")
    contexts: list[tuple[str, ...]] = [(), ("আমি",), ("তুমি", "কি")]
    for r in romans:
        for ctx in contexts:
            for k in (5,):
                w.add(
                    {
                        "roman": r,
                        "context": list(ctx),
                        "k": k,
                        "fast": True,
                        "out": engine.suggest(r, ctx, k, fast=True),
                    }
                )
                w.add(
                    {
                        "roman": r,
                        "context": list(ctx),
                        "k": k,
                        "fast": False,
                        "out": engine.suggest(r, ctx, k, fast=False),
                    }
                )
    w.close()

    wf = Writer("fast_suggest")
    for r in romans:
        for ctx in contexts:
            out, strong = engine.fast_suggest(r, ctx, 5)
            wf.add({"roman": r, "context": list(ctx), "out": out, "strong": strong})
    wf.close()


def dump_features(engine, romans: list[str]) -> None:
    """Per-candidate features and scores: when a suggest golden fails, this says which term moved."""
    w = Writer("features")
    for r in romans:
        feats = engine.candidates(r, use_model=True)
        rows = []
        for word, ft in sorted(feats.items()):
            rows.append(
                {
                    "word": word,
                    "rom_exact": ft.rom_exact,
                    "rom_prefix": ft.rom_prefix,
                    "key_fine": ft.key_fine,
                    "key_coarse": ft.key_coarse,
                    "key_fine_prefix": ft.key_fine_prefix,
                    "key_coarse_prefix": ft.key_coarse_prefix,
                    "gap": ft.gap,
                    "xlit_rank": ft.xlit_rank,
                    "xlit_logp": repr(float(ft.xlit_logp)),
                    "avro": ft.avro,
                    "lex_score": ft.lex_score,
                    "in_lexicon": ft.in_lexicon,
                    "is_latin": ft.is_latin,
                    "sources": sorted(ft.sources),
                    "score": repr(float(engine.score(word, ft, r))),
                }
            )
        w.add({"roman": r, "candidates": rows})
    w.close()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--quick", action="store_true", help="small sets for a fast iteration loop")
    ap.add_argument("--beam", type=int, default=4)
    args = ap.parse_args()

    from likhi.engine.core import LikhiEngine
    from likhi.engine.textnorm import normalize_roman

    n_cheap = 200 if args.quick else 4000
    n_model = 20 if args.quick else 400

    corpus = dakshina_romans(n_cheap)
    romans_cheap = EDGE_ROMAN + corpus + feedback_romans()
    banglas = EDGE_BANGLA + dakshina_words(n_cheap)

    log(f"{len(romans_cheap)} roman inputs, {len(banglas)} bangla inputs")

    dump_textnorm(romans_cheap, banglas)
    dump_romankey(romans_cheap, banglas)
    dump_avro(romans_cheap)

    log("loading the engine (this is the slow part)...")
    # personal=None: the golden set must not depend on whatever this machine has learned, or it
    # would be unreproducible anywhere else.
    engine = LikhiEngine(personal=None)

    dump_lexicon(engine, banglas)

    # The model path is ~80ms per word, so it gets a smaller, deliberately chosen set: the edge
    # cases and reported failures first, then corpus words to fill.
    seeds = [normalize_roman(r) for r in EDGE_ROMAN + feedback_romans()]
    seeds = [r for r in seeds if r]
    fill = [normalize_roman(r) for r in corpus]
    model_romans: list[str] = []
    for r in seeds + fill:
        if r and r not in model_romans:
            model_romans.append(r)
        if len(model_romans) >= n_model:
            break
    log(f"{len(model_romans)} inputs through the model path")

    dump_xlit(engine, model_romans, args.beam)
    dump_suggest(engine, model_romans)
    dump_features(engine, model_romans[: max(1, len(model_romans) // 4)])

    log("done")


if __name__ == "__main__":
    main()
