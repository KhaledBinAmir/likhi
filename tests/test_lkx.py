"""The `.lkx` format: round trip, and equivalence with the `marisa` tries it replaced.

Two properties, tested separately because they can fail for different reasons.

1. **Round trip.** Anything `write_lkx` writes, `lkx.Table` must read back exactly -- every key,
   every record, every prefix scan, in the right order. This is where the front-coding, the block
   boundaries and the binary search are exercised, on synthetic tables built to sit right on those
   boundaries. It needs no built lexicon and runs in milliseconds.

2. **Equivalence with marisa.** A trie converted to `.lkx` must answer identically to the trie.
   This is what justified moving the lexicon off `marisa_trie` at all. It converts a real table on
   the fly rather than reading a pre-converted one, because `models/rust/lexicon` is no longer a
   conversion of `models/lexicon` -- it is built directly by `engine/src/bin/lexicon.rs`, and the
   two legitimately differ by 2 romanizations and 83 prefix entries.
"""

from __future__ import annotations

import importlib.util
import struct
import sys
from pathlib import Path

import pytest

from likhi import lkx

REPO = Path(__file__).resolve().parents[1]
MARISA = REPO / "models" / "lexicon"


def _write_lkx():
    """`write_lkx` from scripts/build_rust_data.py, which is a script rather than a module."""
    path = REPO / "scripts" / "build_rust_data.py"
    spec = importlib.util.spec_from_file_location("likhi_build_rust_data", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module.write_lkx


# ------------------------------------------------------------------------------------ round trip


def build(tmp_path: Path, entries: list[tuple[str, int]], ranks: list[int] | None = None):
    """A one-u32-record table from (key, value) pairs."""
    path = tmp_path / "t.lkx"
    _write_lkx()(
        path,
        [(k, struct.pack("<I", v)) for k, v in entries],
        4,
        "test",
        ranks=ranks,
    )
    return lkx.Table(path, "<I")


def test_round_trip_across_block_boundaries(tmp_path: Path) -> None:
    # 16 entries per block, so 40 spans three blocks with a partial last one -- the case where a
    # walk that trusted a full block would read past the end.
    entries = [(f"key{i:04d}", i) for i in range(40)]
    t = build(tmp_path, entries)
    assert len(t) == 40
    assert [(k, r[0]) for k, r in t.items()] == entries
    for k, v in entries:
        assert t.get(k) == [(v,)], f"get({k!r})"


def test_round_trip_with_shared_prefixes(tmp_path: Path) -> None:
    # Front-coding stores only what differs from the previous key, so deep shared prefixes and a
    # key that is a strict prefix of the next are the interesting cases.
    entries = sorted(
        [("a", 1), ("ab", 2), ("abc", 3), ("abcd", 4), ("abce", 5), ("b", 6), ("", 7)]
    )
    t = build(tmp_path, entries)
    assert [(k, r[0]) for k, r in t.items()] == entries
    for k, v in entries:
        assert t.get(k) == [(v,)], f"get({k!r})"
    assert t.get("abcz") is None
    assert t.get("c") is None


def test_round_trip_with_non_ascii_keys(tmp_path: Path) -> None:
    # Keys are sorted by UTF-8 bytes; multi-byte characters must not be split by the front-coding.
    entries = sorted([("আমার", 1), ("আমি", 2), ("আম", 3), ("তুমি", 4), ("ক", 5)])
    t = build(tmp_path, entries)
    assert [(k, r[0]) for k, r in t.items()] == entries
    for k, v in entries:
        assert t.get(k) == [(v,)]


def test_prefix_scan_is_bounded_and_complete(tmp_path: Path) -> None:
    entries = sorted(
        [(f"{stem}\t{i}", i) for stem in ("amar", "amara", "ami", "b") for i in range(5)]
    )
    t = build(tmp_path, entries)
    for prefix in ("amar\t", "amar", "am", "a", "b", "", "zzz"):
        got = [k for k, _ in t.items(prefix)]
        want = sorted(k for k, _ in entries if k.startswith(prefix))
        assert got == want, f"items({prefix!r})"


def test_ranks_decide_prefix_scan_order(tmp_path: Path) -> None:
    # Without ranks the scan is in key order; with them it follows the rank, which is how the
    # engine's tie-breaking order survives being written to disk.
    entries = [("p\ta", 1), ("p\tb", 2), ("p\tc", 3)]
    assert [k for k, _ in build(tmp_path, entries).items("p\t")] == ["p\ta", "p\tb", "p\tc"]

    ranked = build(tmp_path / "r", entries, ranks=[2, 0, 1])
    assert [k for k, _ in ranked.items("p\t")] == ["p\tb", "p\tc", "p\ta"]


def test_duplicate_keys_are_preserved(tmp_path: Path) -> None:
    # marisa's RecordTrie allows several records per key and `.items()` yields each one, so the
    # format has to keep them; `get` returns the first.
    entries = [("dup", 1), ("dup", 2), ("other", 3)]
    t = build(tmp_path, entries)
    assert len(t) == 3
    assert sorted(r[0] for k, r in t.items() if k == "dup") == [1, 2]
    assert t.get("dup") is not None


# ------------------------------------------------------------------------------ against marisa


@pytest.mark.skipif(
    not (MARISA / "unigrams.marisa").exists(),
    reason="needs models/lexicon/unigrams.marisa (built by an older likhi-data)",
)
def test_converted_trie_answers_identically(tmp_path: Path) -> None:
    """Convert a real trie and check the reader agrees with it, key for key."""
    import marisa_trie

    trie = marisa_trie.RecordTrie("<III")
    trie.load(str(MARISA / "unigrams.marisa"))

    entries = [(k, struct.pack("<III", *r)) for k, r in trie.items()]
    path = tmp_path / "unigrams.lkx"
    _write_lkx()(path, entries, 12, "unigrams")
    table = lkx.Table(path, "<III")

    assert len(table) == len(entries)
    assert sorted((k, tuple(r)) for k, r in table.items()) == sorted(
        (k, tuple(r)) for k, r in trie.items()
    )

    keys = [k for k, _ in trie.items()]
    for key in keys[:: max(1, len(keys) // 2000)]:
        assert table.get(key) == trie.get(key), f"get({key!r})"
    for key in ("", "zzzzz-not-a-key", "￿￿"):
        assert table.get(key) == trie.get(key), f"get({key!r}) on a miss"
