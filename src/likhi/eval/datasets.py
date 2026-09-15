"""Loaders for the evaluation and training datasets.

All loaders return plain dataclasses so the harness never depends on pandas. Raw files live under
``data/raw`` (see ``scripts/fetch_datasets.py``); override with the ``LIKHI_DATA_RAW`` env var.

Two views of word-level data are offered:

* ``pairs``: one item per attested (roman, native) pair, unweighted. This matches how published
  top-1 numbers (Dakshina paper, IndicXlit) are computed and is the number to compare against.
* ``grouped``: one item per distinct roman string, with every attested native word accepted as
  gold and the attestation total as weight. This is closer to what a typist experiences.
"""

from __future__ import annotations

import csv
import gzip
import json
import os
import re
from collections import defaultdict
from collections.abc import Iterable, Iterator
from dataclasses import dataclass, field
from pathlib import Path

from likhi.engine.textnorm import canonical, has_bengali, normalize_roman


def raw_dir() -> Path:
    env = os.environ.get("LIKHI_DATA_RAW")
    if env:
        return Path(env)
    return Path(__file__).resolve().parents[3] / "data" / "raw"


@dataclass(frozen=True)
class WordItem:
    roman: str
    golds: tuple[str, ...]
    weight: float = 1.0
    source: str = ""


@dataclass(frozen=True)
class SentenceItem:
    roman: str
    gold: str
    source: str = ""


@dataclass
class WordSet:
    name: str
    items: list[WordItem] = field(default_factory=list)

    def __len__(self) -> int:
        return len(self.items)


def _open_text(path: Path):
    if path.suffix == ".gz":
        return gzip.open(path, "rt", encoding="utf-8")
    return open(path, encoding="utf-8")


def _group(pairs: Iterable[tuple[str, str, float, str]], name: str, grouped: bool) -> WordSet:
    """Build a WordSet from (roman, native, weight, source) tuples."""
    if not grouped:
        return WordSet(name, [WordItem(r, (n,), w, s) for r, n, w, s in pairs])
    golds: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    sources: dict[str, str] = {}
    for r, n, w, s in pairs:
        golds[r][n] += w
        sources.setdefault(r, s)
    items = [
        WordItem(r, tuple(sorted(g, key=lambda x: -g[x])), sum(g.values()), sources[r])
        for r, g in golds.items()
    ]
    return WordSet(name + "+grouped", items)


# --------------------------------------------------------------------------------------- Dakshina

def _dakshina_lexicon_rows(split: str) -> Iterator[tuple[str, str, float, str]]:
    path = raw_dir() / "dakshina" / "bn" / "lexicons" / f"bn.translit.sampled.{split}.tsv"
    with _open_text(path) as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 3:
                continue
            native, roman, count = parts[0], parts[1], float(parts[2])
            yield normalize_roman(roman), canonical(native), count, "dakshina"


def dakshina_lexicon(split: str = "test", *, grouped: bool = False) -> WordSet:
    """Dakshina bn romanization lexicon. splits: train / dev / test."""
    return _group(_dakshina_lexicon_rows(split), f"dakshina-{split}", grouped)


def dakshina_sentences(split: str = "test") -> list[SentenceItem]:
    """Dakshina bn romanized Wikipedia sentences (native \\t roman). dev = first half, test = second."""
    d = raw_dir() / "dakshina" / "bn" / "romanized"
    per_split = d / f"bn.romanized.rejoined.{split}.tsv"
    rows: list[tuple[str, str]] = []
    path = per_split if per_split.exists() else d / "bn.romanized.rejoined.tsv"
    with _open_text(path) as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= 2:
                rows.append((parts[0], parts[1]))
    if path != per_split:
        half = len(rows) // 2
        rows = rows[:half] if split == "dev" else rows[half:]
    return [SentenceItem(roman=r, gold=canonical(n), source="dakshina") for n, r in rows]


# ------------------------------------------------------------------------------------ Aksharantar

def _aksharantar_rows(split: str) -> Iterator[tuple[str, str, float, str]]:
    path = raw_dir() / "aksharantar" / f"ben_{split}.json"
    with _open_text(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            yield (
                normalize_roman(obj["english word"]),
                canonical(obj["native word"]),
                1.0,
                obj.get("source", "aksharantar"),
            )


def aksharantar(split: str = "test", *, grouped: bool = False, sources: set[str] | None = None) -> WordSet:
    """Aksharantar bn. splits: train / valid / test. ``sources`` filters e.g. {"AK-Freq"}."""
    rows = _aksharantar_rows(split)
    if sources:
        rows = (r for r in rows if r[3] in sources)
    return _group(rows, f"aksharantar-{split}", grouped)


# ------------------------------------------------------------------------------------- BanglaTLit

_RE_TOKEN_PUNCT = re.compile(r"^[\W_]+|[\W_]+$", re.UNICODE)
_RE_LATIN_ONLY = re.compile(r"^[A-Za-z']+$")


def _strip_punct(tok: str) -> str:
    return _RE_TOKEN_PUNCT.sub("", tok)


def banglatlit_sentences(split: str = "test") -> list[SentenceItem]:
    """BanglaTLit annotated pairs. Files: data/raw/banglatlit/{train,val,test}.csv."""
    path = raw_dir() / "banglatlit" / f"{split}.csv"
    out: list[SentenceItem] = []
    with open(path, encoding="utf-8", newline="") as f:
        for row in csv.DictReader(f):
            bn = (row.get("text_bengali") or "").strip()
            rm = (row.get("text_transliterated") or "").strip()
            if bn and rm:
                out.append(SentenceItem(roman=rm, gold=canonical(bn), source="banglatlit"))
    return out


def banglatlit_word_pairs(split: str = "test", *, grouped: bool = True) -> WordSet:
    """Chat-style word pairs aligned from BanglaTLit sentences.

    Only sentences whose roman and Bengali sides have the same number of tokens are used, tokens
    are aligned positionally, and pairs are kept when the roman side is Latin letters and the
    Bengali side is Bengali script. This is noisy but large, and it is the only public data in the
    Bangladeshi chat register.
    """

    def rows() -> Iterator[tuple[str, str, float, str]]:
        for item in banglatlit_sentences(split):
            r_toks = [_strip_punct(t) for t in item.roman.split()]
            b_toks = [_strip_punct(t) for t in item.gold.split()]
            if len(r_toks) != len(b_toks):
                continue
            for r, b in zip(r_toks, b_toks, strict=True):
                if not r or not b or not _RE_LATIN_ONLY.match(r) or not has_bengali(b):
                    continue
                if has_bengali(r) or re.search(r"[A-Za-z]", b):
                    continue
                yield normalize_roman(r), b, 1.0, "banglatlit"

    return _group(rows(), f"banglatlit-{split}-words", grouped)


# ---------------------------------------------------------------------------------- Personal set

def personal(path: Path | None = None) -> tuple[list[SentenceItem], WordSet]:
    """Khaled's personal set: JSONL with {"roman": ..., "gold": [...], "tags": [...]}.

    Returns sentence items (first gold) and positionally aligned word pairs like BanglaTLit.
    """
    path = path or (raw_dir().parent / "personal" / "personal.jsonl")
    sents: list[SentenceItem] = []
    pairs: list[tuple[str, str, float, str]] = []
    if not path.exists():
        return sents, WordSet("personal-words")
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            obj = json.loads(line)
            golds = obj["gold"] if isinstance(obj["gold"], list) else [obj["gold"]]
            sents.append(SentenceItem(roman=obj["roman"], gold=canonical(golds[0]), source="personal"))
            r_toks = [_strip_punct(t) for t in obj["roman"].split()]
            for g in golds:
                b_toks = [_strip_punct(t) for t in g.split()]
                if len(r_toks) != len(b_toks):
                    continue
                for r, b in zip(r_toks, b_toks, strict=True):
                    if r and b and _RE_LATIN_ONLY.match(r) and has_bengali(b):
                        pairs.append((normalize_roman(r), canonical(b), 1.0, "personal"))
    return sents, _group(pairs, "personal-words", grouped=True)


# ------------------------------------------------------------------------------------- Registry

def load_wordset(name: str) -> WordSet:
    """Resolve names like 'dakshina-test', 'dakshina-dev+grouped', 'aksharantar-test',
    'aksharantar-test:AK-Freq', 'banglatlit-test-words', 'personal-words'."""
    grouped = name.endswith("+grouped")
    base = name.removesuffix("+grouped")
    src_filter: set[str] | None = None
    if ":" in base:
        base, flt = base.split(":", 1)
        src_filter = set(flt.split(","))
    if base.startswith("dakshina-"):
        return dakshina_lexicon(base.split("-", 1)[1], grouped=grouped)
    if base.startswith("aksharantar-"):
        return aksharantar(base.split("-", 1)[1], grouped=grouped, sources=src_filter)
    if base.startswith("banglatlit-") and base.endswith("-words"):
        return banglatlit_word_pairs(base.split("-")[1], grouped=True)
    if base == "personal-words":
        return personal()[1]
    raise KeyError(f"unknown word set: {name}")


def load_sentences(name: str) -> list[SentenceItem]:
    if name.startswith("dakshina-"):
        return dakshina_sentences(name.split("-", 1)[1])
    if name.startswith("banglatlit-"):
        return banglatlit_sentences(name.split("-", 1)[1])
    if name == "personal":
        return personal()[0]
    raise KeyError(f"unknown sentence set: {name}")
