"""Build a pruned word-bigram table for context-aware ranking.

Sources: Dakshina's native-script Wikipedia sentences (formal register, weight 1) and the
BanglaTLit chat sentences (weight 5, the register Likhi targets). Tokens are mapped to the same
surface forms as the lexicon so lookups agree. A sentence-start token ``<s>`` conditions the
first word.

Outputs (models/lexicon/):
* bigrams.marisa        RecordTrie "prev\\tword" -> (count,)
* bigram_totals.marisa  RecordTrie "prev" -> (total,)   (totals before pruning)
"""

from __future__ import annotations

import gzip
import json
import time
from collections import Counter, defaultdict
from pathlib import Path

from likhi.data.build_lexicon import banglatlit_train_rows, log, tokens
from likhi.engine.textnorm import match_key
from likhi.eval import datasets as ds

START = "<s>"


def _surface_map(lexicon_dir: Path) -> dict[str, str]:
    import marisa_trie

    uni = marisa_trie.RecordTrie("<III")
    uni.load(str(lexicon_dir / "unigrams.marisa"))
    return {match_key(w): w for w in uni.keys()}


def _sentences(raw: Path, wiki_lines: int | None):
    path = (
        raw / "dakshina" / "bn" / "native_script_wikipedia" / "bn.wiki-filt.train.text.shuf.txt.gz"
    )
    n = 0
    with gzip.open(path, "rt", encoding="utf-8") as f:
        for line in f:
            yield line, 1
            n += 1
            if wiki_lines and n >= wiki_lines:
                break
    for row in banglatlit_train_rows(raw):
        yield row["text_bengali"], 5


def build(
    lexicon_dir: Path,
    *,
    raw: Path | None = None,
    wiki_lines: int | None = None,
    min_count: int = 2,
    per_prev: int = 60,
) -> dict:
    import marisa_trie

    t0 = time.time()
    raw = raw or ds.raw_dir()
    surf = _surface_map(lexicon_dir)
    counts: dict[str, Counter[str]] = defaultdict(Counter)
    totals: Counter[str] = Counter()
    n_sent = 0
    for text, weight in _sentences(raw, wiki_lines):
        prev = START
        for tok in tokens(text):
            w = surf.get(match_key(tok))
            if w is None:
                prev = None  # unknown word breaks the chain
                continue
            if prev is not None:
                counts[prev][w] += weight
                totals[prev] += weight
            prev = w
        n_sent += 1
        if n_sent % 200000 == 0:
            log(f"bigrams: {n_sent:,} sentences, {sum(len(c) for c in counts.values()):,} pairs")
    items = []
    kept = 0
    for prev, c in counts.items():
        for w, n in c.most_common(per_prev):
            if n < min_count:
                break
            items.append((f"{prev}\t{w}", (min(n, 2**31 - 1),)))
            kept += 1
    trie = marisa_trie.RecordTrie("<I", items)
    trie.save(str(lexicon_dir / "bigrams.marisa"))
    tot = marisa_trie.RecordTrie("<I", [(p, (min(n, 2**31 - 1),)) for p, n in totals.items()])
    tot.save(str(lexicon_dir / "bigram_totals.marisa"))
    meta = {
        "built": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "sentences": n_sent,
        "bigram_types": sum(len(c) for c in counts.values()),
        "kept": kept,
        "prev_types": len(totals),
        "seconds": round(time.time() - t0, 1),
        "sizes_bytes": {
            p.name: p.stat().st_size for p in lexicon_dir.iterdir() if p.name.startswith("bigram")
        },
    }
    (lexicon_dir / "bigrams_meta.json").write_text(json.dumps(meta, indent=1), encoding="utf-8")
    log(
        f"bigrams done in {meta['seconds']}s: kept {kept:,} of {meta['bigram_types']:,}; {json.dumps(meta['sizes_bytes'])}"
    )
    return meta
