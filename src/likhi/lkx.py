"""Read the `.lkx` lexicon tables, from Python.

The Rust engine reads these; so does this, so that the two implementations can be compared on
exactly the same data. That matters during the move off `marisa_trie`: the lexicon builder is being
rewritten in Rust and will emit `.lkx` directly, and without a Python reader there would be no way
to measure whether the new tables change any suggestion -- the evaluation harness drives the Python
engine.

This is a drop-in for the part of `marisa_trie.RecordTrie` that `LikhiEngine` uses: `get`,
`items`, `items(prefix)` and `in`. Nothing else.

Format, written by `scripts/build_rust_data.py` and mirrored by `engine/src/lexicon.rs`:

    magic       4   b"LKX3"
    version     4   u32 = 3
    count       4   u32   entries
    record_sz   4   u32   bytes per record
    block_len   4   u32   entries per block
    n_blocks    4   u32
    flags       4   u32   bit 0: a rank array follows the records
    keys_len    8   u64
    block_offs  4*n_blocks u32   byte offset of each block within the key blob
    keys        keys_len bytes, front-coded in blocks; see below
    (pad to 64)
    records     count*record_sz
    ranks       count*4, when flag 0 is set

Keys are sorted by UTF-8 bytes and front-coded: the first key of each block is written whole, and
each key after it as (shared prefix length, suffix length, suffix) with both lengths as LEB128.

**Ranks.** `ranks[i]` is the position entry `i` had in the enumeration of whatever built the table.
`items(prefix)` returns entries in that order, not in key order, because the engine inserts
candidates in the order the table yields them and then sorts stably -- so this order decides how
equal scores break. See `write_lkx` in the builder for why that is reproduced rather than tidied up.
"""

from __future__ import annotations

import mmap
import struct
from pathlib import Path
from typing import Iterator

MAGIC = b"LKX3"
VERSION = 3
HEADER = 4 + 4 * 6 + 8
FLAG_RANKS = 1


class Table:
    """One `.lkx` table, memory-mapped."""

    def __init__(self, path: Path | str, fmt: str) -> None:
        self.path = Path(path)
        self.fmt = fmt
        self._struct = struct.Struct(fmt)
        self._file = open(self.path, "rb")
        self._map = mmap.mmap(self._file.fileno(), 0, access=mmap.ACCESS_READ)
        m = self._map

        if len(m) < HEADER or m[:4] != MAGIC:
            raise ValueError(f"{self.path}: not an .lkx table")
        version, count, record_size, block_len, n_blocks, flags = struct.unpack_from("<IIIIII", m, 4)
        if version != VERSION:
            raise ValueError(f"{self.path}: version {version}, expected {VERSION}")
        if record_size != self._struct.size:
            raise ValueError(
                f"{self.path}: records are {record_size} bytes, format {fmt!r} wants "
                f"{self._struct.size}"
            )
        (keys_len,) = struct.unpack_from("<Q", m, 28)

        self.count = count
        self.block_len = block_len
        self._block_offs = struct.unpack_from(f"<{n_blocks}I", m, HEADER)
        self._keys_start = HEADER + n_blocks * 4
        self._keys_end = self._keys_start + keys_len
        # The builder pads the header out to a 64-byte boundary before the records.
        self._records_start = -(-self._keys_end // 64) * 64
        self._record_size = record_size
        end = self._records_start + count * record_size
        if len(m) < end:
            raise ValueError(f"{self.path}: truncated; records need {end} bytes, file has {len(m)}")
        self._ranks_start = end if flags & FLAG_RANKS else None
        if self._ranks_start is not None and len(m) < end + count * 4:
            raise ValueError(f"{self.path}: header claims ranks but the file ends before them")

    # ------------------------------------------------------------------ keys

    def _varint(self, at: int) -> tuple[int, int]:
        value = 0
        shift = 0
        m = self._map
        while True:
            b = m[at]
            at += 1
            value |= (b & 0x7F) << shift
            if not b & 0x80:
                return value, at
            shift += 7

    def _walk_block(self, block: int) -> Iterator[tuple[int, bytes]]:
        """(entry index, key bytes) for one block, reconstructing the front-coded keys."""
        start = self._block_offs[block]
        end = (
            self._block_offs[block + 1]
            if block + 1 < len(self._block_offs)
            else self._keys_end - self._keys_start
        )
        at = self._keys_start + start
        stop = self._keys_start + end
        base = block * self.block_len
        prev = b""
        for slot in range(self.block_len):
            i = base + slot
            if i >= self.count or at >= stop:
                return
            if slot == 0:
                length, at = self._varint(at)
                key = self._map[at : at + length]
                at += length
            else:
                shared, at = self._varint(at)
                rest, at = self._varint(at)
                key = prev[:shared] + self._map[at : at + rest]
                at += rest
            prev = key
            yield i, key

    def _block_head(self, block: int) -> bytes:
        at = self._keys_start + self._block_offs[block]
        length, at = self._varint(at)
        return self._map[at : at + length]

    def _lower_bound(self, target: bytes) -> int:
        """Index of the first entry whose key is >= target."""
        lo, hi = 0, len(self._block_offs)
        while lo < hi:
            mid = (lo + hi) // 2
            if self._block_head(mid) < target:
                lo = mid + 1
            else:
                hi = mid
        if lo == 0:
            return 0
        block = lo - 1
        found = block * self.block_len
        for i, key in self._walk_block(block):
            if key >= target:
                return i
            found = i + 1
        return found

    # ------------------------------------------------------------------ records

    def _record(self, i: int) -> tuple:
        at = self._records_start + i * self._record_size
        return self._struct.unpack_from(self._map, at)

    def _rank(self, i: int) -> int:
        if self._ranks_start is None:
            return i
        (r,) = struct.unpack_from("<I", self._map, self._ranks_start + i * 4)
        return r

    # ------------------------------------------------------------------ the RecordTrie surface

    def get(self, key: str, default=None):
        """A list of records, as RecordTrie returns. `LikhiEngine` reads `[0]`."""
        target = key.encode("utf-8")
        i = self._lower_bound(target)
        if i >= self.count:
            return default
        for j, k in self._walk_block(i // self.block_len):
            if j == i:
                return [self._record(i)] if k == target else default
            if j > i:
                break
        return default

    def __contains__(self, key: str) -> bool:
        return self.get(key) is not None

    def items(self, prefix: str = "") -> list[tuple[str, tuple]]:
        """Entries whose key starts with `prefix`, in the source table's own order.

        Not key order: see the note on ranks in the module docstring.
        """
        p = prefix.encode("utf-8")
        start = self._lower_bound(p) if p else 0
        if start >= self.count:
            return []
        hits: list[tuple[int, str]] = []
        block = start // self.block_len
        while block < len(self._block_offs):
            stop = False
            for i, key in self._walk_block(block):
                if i < start:
                    continue
                if not key.startswith(p):
                    stop = True
                    break
                hits.append((i, key.decode("utf-8")))
            if stop:
                break
            block += 1
        hits.sort(key=lambda h: self._rank(h[0]))
        return [(k, self._record(i)) for i, k in hits]

    def __len__(self) -> int:
        return self.count

    def close(self) -> None:
        self._map.close()
        self._file.close()


def exists(directory: Path | str, name: str) -> bool:
    return (Path(directory) / f"{name}.lkx").exists()


def open_table(directory: Path | str, name: str, fmt: str) -> Table:
    return Table(Path(directory) / f"{name}.lkx", fmt)
