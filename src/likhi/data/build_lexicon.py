"""Build the lexicon artifacts the engine loads at runtime.

Inputs (all under data/raw, fetched by scripts/fetch_datasets.py):
* Dakshina native-script Wikipedia sentences  -> formal-register unigram counts
* FrequencyWords bn (OpenSubtitles)            -> conversational unigram counts
* BanglaTLit Bengali side (train rows only)    -> chat-register unigram counts
* Dakshina lexicon train + Aksharantar train + BanglaTLit train word pairs
                                               -> attested romanizations (roman -> word counts)

Outputs (models/lexicon/):
* unigrams.marisa   RecordTrie word -> (wiki, subs, chat) counts
* romans.marisa     RecordTrie "roman\\tword" -> (count, source_bits)
* keys.marisa       RecordTrie "level:key\\tword" -> (score,) for fine and coarse phonetic keys
* meta.json         sizes and provenance
"""

from __future__ import annotations

import csv
import gzip
import json
import re
import time
from collections import Counter, defaultdict
from pathlib import Path

from likhi.engine.romankey import key_from_bangla
from likhi.engine.textnorm import canonical, match_key, normalize_roman
from likhi.eval import datasets as ds

RE_BANGLA_TOKEN = re.compile(r"[ঀ-৿‌‍]+")
RE_ALL_DIGITS_OR_MARKS = re.compile(r"^[০-৯ঁ-ঃ়া-্ৗ‌‍]+$")

SRC_DAKSHINA, SRC_AKSHARANTAR, SRC_BANGLATLIT = 1, 2, 4


def log(msg: str) -> None:
    print(f"[lexicon] {msg}", flush=True)


def tokens(text: str):
    for tok in RE_BANGLA_TOKEN.findall(text):
        if RE_ALL_DIGITS_OR_MARKS.match(tok):
            continue
        yield canonical(tok)


class SurfaceCounter:
    """Counts by match_key but remembers the most frequent surface (output) form."""

    def __init__(self) -> None:
        self.counts: Counter[str] = Counter()
        self.surface: dict[str, Counter[str]] = defaultdict(Counter)

    def add(self, word: str, n: int = 1) -> None:
        k = match_key(word)
        self.counts[k] += n
        self.surface[k][word] += n

    def best_surface(self, k: str) -> str:
        return self.surface[k].most_common(1)[0][0]


def count_wiki(raw: Path, limit_lines: int | None) -> SurfaceCounter:
    sc = SurfaceCounter()
    path = (
        raw / "dakshina" / "bn" / "native_script_wikipedia" / "bn.wiki-filt.train.text.shuf.txt.gz"
    )
    n = 0
    with gzip.open(path, "rt", encoding="utf-8") as f:
        for line in f:
            for tok in tokens(line):
                sc.add(tok)
            n += 1
            if limit_lines and n >= limit_lines:
                break
    log(f"wiki: {n:,} lines, {len(sc.counts):,} types, {sum(sc.counts.values()):,} tokens")
    return sc


def count_subs(raw: Path) -> SurfaceCounter:
    sc = SurfaceCounter()
    with open(raw / "frequencywords" / "bn_full.txt", encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if len(parts) != 2:
                continue
            w, c = parts
            toks = list(tokens(w))
            if len(toks) != 1:
                continue
            sc.add(toks[0], int(c))
    log(f"subs: {len(sc.counts):,} types")
    return sc


def banglatlit_train_rows(raw: Path) -> list[dict]:
    """Annotated train rows, excluding anything that appears in val/test (the train file contains them)."""
    held = set()
    for split in ("val", "test"):
        with open(raw / "banglatlit" / f"{split}.csv", encoding="utf-8", newline="") as f:
            for row in csv.DictReader(f):
                held.add(row["id"])
                held.add((row["text_transliterated"] or "").strip())
    rows = []
    with open(raw / "banglatlit" / "train.csv", encoding="utf-8", newline="") as f:
        for row in csv.DictReader(f):
            bn = (row.get("text_bengali") or "").strip()
            rm = (row.get("text_transliterated") or "").strip()
            if not bn or not rm or row["id"] in held or rm in held:
                continue
            rows.append(row)
    return rows


def count_chat(rows: list[dict]) -> SurfaceCounter:
    sc = SurfaceCounter()
    for row in rows:
        for tok in tokens(row["text_bengali"]):
            sc.add(tok)
    log(f"chat: {len(rows):,} sentences, {len(sc.counts):,} types")
    return sc


def collect_romans(raw: Path, chat_rows: list[dict]) -> dict[tuple[str, str], list[int]]:
    """(roman, word) -> [count, source_bits]"""
    rom: dict[tuple[str, str], list[int]] = defaultdict(lambda: [0, 0])

    def add(roman: str, word: str, n: int, src: int) -> None:
        roman = normalize_roman(roman)
        word = canonical(word)
        if not roman or not word:
            return
        e = rom[(roman, word)]
        e[0] += n
        e[1] |= src

    for split in ("train",):
        for it in ds.dakshina_lexicon(split).items:
            add(it.roman, it.golds[0], int(it.weight), SRC_DAKSHINA)
    log(f"romans after dakshina: {len(rom):,}")
    for it in ds.aksharantar("train").items:
        add(it.roman, it.golds[0], 1, SRC_AKSHARANTAR)
    log(f"romans after aksharantar: {len(rom):,}")
    # BanglaTLit: positional alignment of same-length sentences (same recipe as the eval loader)
    from likhi.engine.textnorm import has_bengali
    from likhi.eval.datasets import _RE_LATIN_ONLY, _strip_punct  # noqa: PLC2701

    n_pairs = 0
    for row in chat_rows:
        r_toks = [_strip_punct(t) for t in row["text_transliterated"].split()]
        b_toks = [_strip_punct(t) for t in row["text_bengali"].split()]
        if len(r_toks) != len(b_toks):
            continue
        for r, b in zip(r_toks, b_toks, strict=True):
            if (
                r
                and b
                and _RE_LATIN_ONLY.match(r)
                and has_bengali(b)
                and not re.search(r"[A-Za-z]", b)
            ):
                add(r, b, 1, SRC_BANGLATLIT)
                n_pairs += 1
    log(f"romans after banglatlit ({n_pairs:,} aligned pairs): {len(rom):,}")
    return rom


SHORT_ROMAN_PREFIX = 3  # completions are precomputed for typed prefixes up to this length
SHORT_KEY_PREFIX = 2
PREFIX_TOP = 24


def build_prefix_tables(out: Path, rom_items: list, key_items: list) -> None:
    """Top completions for very short inputs.

    A one- or two-letter prefix matches tens of thousands of trie entries; scanning them at every
    keystroke is what made "ki" take 180 ms. So for short roman prefixes and short phonetic-key
    prefixes the best PREFIX_TOP words are picked once here and looked up in O(1) at runtime.
    Keys: "r:<prefix>\\t<word>" (romanizations, scored by attestation + lexicon score) and
    "k:<level>:<keyprefix>\\t<word>" (phonetic keys, scored by lexicon score).
    """
    import marisa_trie

    lex: dict[str, int] = {}
    for key, (score,) in key_items:
        lex[key.split("\t", 1)[1]] = score
    best: dict[str, dict[str, int]] = defaultdict(dict)
    for key, (count, _src) in rom_items:
        roman, word = key.split("\t", 1)
        for n in range(1, min(SHORT_ROMAN_PREFIX, len(roman)) + 1):
            p = "r:" + roman[:n]
            # attestation count first, corpus frequency only to break ties: a single noisy
            # alignment of a very frequent word must not outrank a well-attested completion
            s = count * 10_000 + min(lex.get(word, 0), 9_999)
            if s > best[p].get(word, -1):
                best[p][word] = s
    for key, (score,) in key_items:
        kk, word = key.split("\t", 1)
        level, pk = kk.split(":", 1)
        for n in range(1, min(SHORT_KEY_PREFIX, len(pk)) + 1):
            p = f"k:{level}:{pk[:n]}"
            if score > best[p].get(word, -1):
                best[p][word] = score
    items = []
    for p, words in best.items():
        for w, s in sorted(words.items(), key=lambda kv: -kv[1])[:PREFIX_TOP]:
            items.append((f"{p}\t{w}", (min(int(s), 2**31 - 1),)))
    marisa_trie.RecordTrie("<I", items).save(str(out / "prefixes.marisa"))
    log(f"prefix tables: {len(best):,} short prefixes, {len(items):,} entries")


def build(
    out: Path, *, raw: Path | None = None, wiki_lines: int | None = None, min_wiki: int = 2
) -> dict:
    import marisa_trie

    t0 = time.time()
    raw = raw or ds.raw_dir()
    out.mkdir(parents=True, exist_ok=True)

    wiki = count_wiki(raw, wiki_lines)
    subs = count_subs(raw)
    chat_rows = banglatlit_train_rows(raw)
    chat = count_chat(chat_rows)
    romans = collect_romans(raw, chat_rows)

    # Lexicon membership (unigrams + phonetic keys): corpus words above a small threshold plus the
    # human-romanized Dakshina words. Aksharantar's 1.1M mined words stay reachable through the
    # romanization index only: they are mostly named entities and would swamp the key index.
    keys: set[str] = set()
    keys |= {k for k, c in wiki.counts.items() if c >= min_wiki}
    keys |= set(subs.counts)
    keys |= set(chat.counts)
    rom_surface: dict[str, Counter[str]] = defaultdict(Counter)
    for (_r, w), (c, src) in romans.items():
        k = match_key(w)
        rom_surface[k][w] += c
        if src & (SRC_DAKSHINA | SRC_BANGLATLIT):
            keys.add(k)
    log(f"lexicon size: {len(keys):,} (words with romanizations: {len(rom_surface):,})")

    def surface(k: str) -> str:
        for sc in (chat, subs, wiki):
            if k in sc.counts:
                return sc.best_surface(k)
        best = rom_surface.get(k)
        return best.most_common(1)[0][0] if best else k

    surfaces = {k: surface(k) for k in keys}
    uni = marisa_trie.RecordTrie(
        "<III",
        [
            (surfaces[k], (wiki.counts.get(k, 0), subs.counts.get(k, 0), chat.counts.get(k, 0)))
            for k in keys
        ],
    )
    uni.save(str(out / "unigrams.marisa"))

    rom_items = []
    for (r, w), (c, src) in romans.items():
        k = match_key(w)
        rom_items.append((f"{r}\t{surfaces.get(k, w)}", (min(c, 2**31 - 1), src)))
    romtrie = marisa_trie.RecordTrie("<Ib", rom_items)
    romtrie.save(str(out / "romans.marisa"))

    key_items = []
    for k in keys:
        w = surfaces[k]
        score = wiki.counts.get(k, 0) + 3 * subs.counts.get(k, 0) + 20 * chat.counts.get(k, 0)
        for level in ("fine", "coarse"):
            pk = key_from_bangla(w, level)
            if pk:
                key_items.append((f"{level[0]}:{pk}\t{w}", (min(score, 2**31 - 1),)))
    keytrie = marisa_trie.RecordTrie("<I", key_items)
    keytrie.save(str(out / "keys.marisa"))

    build_prefix_tables(out, rom_items, key_items)

    meta = {
        "built": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "words": len(keys),
        "romanizations": len(rom_items),
        "phonetic_keys": len(key_items),
        "wiki_types": len(wiki.counts),
        "wiki_tokens": sum(wiki.counts.values()),
        "subs_types": len(subs.counts),
        "chat_types": len(chat.counts),
        "chat_sentences": len(chat_rows),
        "seconds": round(time.time() - t0, 1),
        "sizes_bytes": {p.name: p.stat().st_size for p in out.iterdir()},
    }
    (out / "meta.json").write_text(json.dumps(meta, indent=1), encoding="utf-8")
    log(f"done in {meta['seconds']}s: {json.dumps(meta['sizes_bytes'])}")
    return meta
