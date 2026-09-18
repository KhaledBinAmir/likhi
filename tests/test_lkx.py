"""The `.lkx` reader must return exactly what `marisa_trie` returned.

This is the hinge of the move off marisa. The lexicon builder is being rewritten in Rust and will
emit `.lkx` directly; before any of that can be trusted, reading the converted tables has to give
the same answers as reading the originals -- same records, same entries, and crucially the same
*order* from `items(prefix)`, because the engine inserts candidates in that order and then sorts
stably, so it decides how equal scores break.

Skipped unless both table sets are present:

    uv run python scripts/build_rust_data.py     # models/rust/lexicon/*.lkx from models/lexicon/*.marisa
"""

from __future__ import annotations

from pathlib import Path

import pytest

from likhi import lkx

REPO = Path(__file__).resolve().parents[1]
MARISA = REPO / "models" / "lexicon"
LKX = REPO / "models" / "rust" / "lexicon"

TABLES = [
    ("unigrams", "<III"),
    ("romans", "<Ib"),
    ("keys", "<I"),
    ("prefixes", "<I"),
    ("bigrams", "<I"),
    ("bigram_totals", "<I"),
]

pytestmark = pytest.mark.skipif(
    not (MARISA / "unigrams.marisa").exists() or not (LKX / "unigrams.lkx").exists(),
    reason="needs both models/lexicon/*.marisa and models/rust/lexicon/*.lkx",
)


def load_marisa(name: str, fmt: str):
    import marisa_trie

    trie = marisa_trie.RecordTrie(fmt)
    trie.load(str(MARISA / f"{name}.marisa"))
    return trie


@pytest.mark.parametrize(("name", "fmt"), TABLES)
def test_same_entries_and_records(name: str, fmt: str) -> None:
    if not (MARISA / f"{name}.marisa").exists():
        pytest.skip(f"{name} not built")
    trie = load_marisa(name, fmt)
    table = lkx.open_table(LKX, name, fmt)
    assert len(table) == sum(1 for _ in trie.items()), f"{name}: entry count differs"

    # Compare as multisets of (key, record): .items() order differs between the two by design
    # (marisa walks its trie, .lkx is sorted), and the per-prefix order is checked separately.
    from_marisa = sorted((k, tuple(r)) for k, r in trie.items())
    from_lkx = sorted((k, tuple(r)) for k, r in table.items())
    assert from_lkx == from_marisa, f"{name}: entries or records differ"


@pytest.mark.parametrize(("name", "fmt"), TABLES)
def test_get_agrees(name: str, fmt: str) -> None:
    if not (MARISA / f"{name}.marisa").exists():
        pytest.skip(f"{name} not built")
    trie = load_marisa(name, fmt)
    table = lkx.open_table(LKX, name, fmt)

    keys = [k for k, _ in trie.items()]
    step = max(1, len(keys) // 3000)
    for key in keys[::step]:
        assert table.get(key) == trie.get(key), f"{name}: get({key!r}) differs"
    # Misses. Deliberately not a bare prefix of a real key: `RecordTrie.get` raises
    # `struct.error: unpack requires a buffer of 4 bytes` on those, so there is no answer from
    # marisa to compare against. The .lkx reader returns None for them, which is what the caller
    # wants; it is checked directly below rather than against a function that throws.
    for key in ["", "zzzzz-not-a-key", "￿￿"]:
        assert table.get(key) == trie.get(key), f"{name}: get({key!r}) differs on a miss"
    # A prefix of a real key, and an extension of one: the two ways a binary search goes wrong.
    # Checked as an absolute rather than against marisa, for the reason above.
    first = keys[0]
    if len(first) > 1 and first[:-1] not in keys:
        assert table.get(first[:-1]) is None, f"{name}: a bare prefix must not match"
    assert table.get(first + "￿") is None, f"{name}: an extension must not match"


def test_prefix_order_is_preserved_for_romans() -> None:
    """The order that decides tie-breaking.

    `romans` is the table whose scan order reaches the ranker unsorted, so it is the one that
    carries marisa's enumeration order in its rank array. If this passes, a candidate list built
    from `.lkx` is identical to one built from the trie, ties included.
    """
    fmt = "<Ib"
    trie = load_marisa("romans", fmt)
    table = lkx.open_table(LKX, "romans", fmt)

    prefixes = ["amar\t", "screensaver\t", "a", "tumi\t", "kor", "bangla\t", "sonar\t", "b", "zz"]
    for p in prefixes:
        want = [(k, tuple(r)) for k, r in trie.items(p)]
        got = [(k, tuple(r)) for k, r in table.items(p)]
        assert got == want, f"romans.items({p!r}) came back in a different order"


def test_engine_gives_the_same_suggestions_from_either_format() -> None:
    """The end of the chain: the same engine, the same words, from either table set."""
    from likhi.engine.core import LikhiEngine

    xlit = REPO / "models" / "indicxlit-np"
    if not (xlit / "model.npz").exists():
        pytest.skip("transliteration model not built")

    a = LikhiEngine(lexicon_dir=MARISA, xlit_dir=xlit, personal=None)
    b = LikhiEngine(lexicon_dir=LKX, xlit_dir=xlit, personal=None)
    assert a.lexicon_format == "marisa"
    assert b.lexicon_format == "lkx"
    assert a._uni_total == b._uni_total, "the unigram normalizer differs"

    words = [
        "amar", "tumi", "kemon", "acho", "bhalo", "korchi", "khacche", "bangla", "sonar",
        "screensaver", "amr", "tmi", "korci", "koria", "rapid", "bajay", "maam", "myam",
    ]
    for w in words:
        assert b.suggest(w, (), 5) == a.suggest(w, (), 5), f"suggest({w!r}) differs"
        assert b.fast_suggest(w, (), 5) == a.fast_suggest(w, (), 5), f"fast_suggest({w!r}) differs"
