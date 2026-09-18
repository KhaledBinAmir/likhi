//! The lexicon tables: sorted string keys with fixed-size records, memory-mapped.
//!
//! This replaces `marisa_trie`, which has no maintained Rust reader. It can, because nothing in the
//! engine needs a trie's full power -- every use is one of exactly three things:
//!
//! * fetch the record for an exact key,
//! * walk every entry whose key starts with a prefix,
//! * walk the whole table once (to total the unigram counts at startup).
//!
//! A sorted array does all three, and a prefix scan over one is sequential reads rather than
//! pointer chasing. The file format is written by `scripts/build_rust_data.py`; see `write_lkx`
//! there for the layout, which this module must mirror exactly.
//!
//! Keys are front-coded in blocks of 16: the first key of each block is stored whole and the rest
//! as (shared prefix length, suffix). So a lookup binary-searches the block heads and then walks at
//! most 15 entries, reconstructing keys as it goes.
//!
//! Every read here is bounds-checked against the mapped length. The file comes from our own build
//! step, but it lands on a user's disk where it can be truncated by a failed copy or a full volume,
//! and the engine must refuse to load a damaged table rather than index into someone else's memory.

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

const MAGIC: &[u8; 4] = b"LKX3";
const VERSION: u32 = 3;
/// Header flag: a `count`-long u32 array follows the records, giving each entry's position in
/// marisa's own enumeration. See `prefix_iter_source_order`.
const FLAG_RANKS: u32 = 1;
/// Must match `BLOCK_LEN` in the builder. Read from the file rather than assumed; this is only the
/// value we expect, used to reject a file written by a mismatched builder.
const EXPECT_BLOCK_LEN: u32 = 16;

#[derive(Debug)]
pub enum LexError {
    Io(std::io::Error),
    Corrupt(String),
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::Io(e) => write!(f, "{e}"),
            LexError::Corrupt(m) => write!(f, "damaged table: {m}"),
        }
    }
}

impl std::error::Error for LexError {}

impl From<std::io::Error> for LexError {
    fn from(e: std::io::Error) -> Self {
        LexError::Io(e)
    }
}

fn corrupt<T>(msg: impl Into<String>) -> Result<T, LexError> {
    Err(LexError::Corrupt(msg.into()))
}

/// One table: sorted keys, one fixed-size record each.
pub struct Table {
    map: Mmap,
    count: usize,
    record_size: usize,
    block_len: usize,
    /// Byte offsets of each block within the key blob.
    block_offs: Vec<u32>,
    keys_start: usize,
    keys_len: usize,
    records_start: usize,
    /// Byte offset of the rank array, when the file carries one.
    ranks_start: Option<usize>,
}

impl Table {
    pub fn open(path: &Path) -> Result<Table, LexError> {
        let file = File::open(path)?;
        // SAFETY: the mapping is read-only and never handed out as a mutable slice. The usual
        // caveat applies -- another process truncating the file underneath us would fault -- and is
        // accepted here for the same reason every mapped-file reader accepts it: these are our own
        // data files in a directory the installer owns.
        let map = unsafe { Mmap::map(&file)? };

        const HEADER: usize = 4 + 4 * 6 + 8;
        if map.len() < HEADER {
            return corrupt(format!("{} bytes, shorter than a header", map.len()));
        }
        if &map[0..4] != MAGIC {
            return corrupt("wrong magic; not an .lkx table");
        }
        let u32_at = |off: usize| -> u32 {
            u32::from_le_bytes([map[off], map[off + 1], map[off + 2], map[off + 3]])
        };
        let version = u32_at(4);
        if version != VERSION {
            return corrupt(format!("version {version}, expected {VERSION}"));
        }
        let count = u32_at(8) as usize;
        let record_size = u32_at(12) as usize;
        let block_len = u32_at(16) as usize;
        let n_blocks = u32_at(20) as usize;
        let flags = u32_at(24);
        let keys_len = u64::from_le_bytes([
            map[28], map[29], map[30], map[31], map[32], map[33], map[34], map[35],
        ]) as usize;

        if block_len == 0 || block_len as u32 != EXPECT_BLOCK_LEN {
            return corrupt(format!("block length {block_len}, expected {EXPECT_BLOCK_LEN}"));
        }
        if record_size == 0 || record_size > 64 {
            return corrupt(format!("implausible record size {record_size}"));
        }
        if n_blocks != count.div_ceil(block_len) {
            return corrupt(format!("{n_blocks} blocks for {count} entries"));
        }

        let offs_start = HEADER;
        let offs_bytes = n_blocks
            .checked_mul(4)
            .ok_or_else(|| LexError::Corrupt("block table overflows".into()))?;
        let keys_start = offs_start
            .checked_add(offs_bytes)
            .ok_or_else(|| LexError::Corrupt("key blob offset overflows".into()))?;
        if map.len() < keys_start + keys_len {
            return corrupt("file ends inside the key blob");
        }

        let mut block_offs = Vec::with_capacity(n_blocks);
        for i in 0..n_blocks {
            let o = u32_at(offs_start + i * 4);
            if o as usize > keys_len {
                return corrupt(format!("block {i} starts past the key blob"));
            }
            block_offs.push(o);
        }
        // The builder emits these ascending; a descending pair means a corrupted or hostile file
        // and would break the binary search into an unpredictable walk.
        if block_offs.windows(2).any(|w| w[0] >= w[1]) && n_blocks > 1 {
            return corrupt("block offsets are not strictly ascending");
        }

        let records_start = (keys_start + keys_len).next_multiple_of(64);
        let records_bytes = count
            .checked_mul(record_size)
            .ok_or_else(|| LexError::Corrupt("record table overflows".into()))?;
        if map.len() < records_start + records_bytes {
            return corrupt(format!(
                "file is {} bytes; records need {}",
                map.len(),
                records_start + records_bytes
            ));
        }

        let ranks_start = if flags & FLAG_RANKS != 0 {
            let at = records_start + records_bytes;
            if map.len() < at + count * 4 {
                return corrupt("the header claims ranks but the file ends before them");
            }
            Some(at)
        } else {
            None
        };

        Ok(Table {
            map,
            count,
            record_size,
            block_len,
            block_offs,
            keys_start,
            keys_len,
            records_start,
            ranks_start,
        })
    }

    pub fn has_ranks(&self) -> bool {
        self.ranks_start.is_some()
    }

    fn rank(&self, i: usize) -> u32 {
        match self.ranks_start {
            Some(at) => {
                let o = at + i * 4;
                u32::from_le_bytes([self.map[o], self.map[o + 1], self.map[o + 2], self.map[o + 3]])
            }
            None => i as u32,
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn keys(&self) -> &[u8] {
        &self.map[self.keys_start..self.keys_start + self.keys_len]
    }

    /// Record bytes for entry `i`. Panics only on an index past `count`, which is a caller bug.
    pub fn record(&self, i: usize) -> &[u8] {
        let at = self.records_start + i * self.record_size;
        &self.map[at..at + self.record_size]
    }

    /// Read a LEB128 varint, returning the value and the new position.
    ///
    /// Bounded at five bytes: the values encoded are string lengths, so anything longer is a
    /// damaged file, and an unbounded loop on corrupt input would read to the end of the blob.
    fn varint(blob: &[u8], mut at: usize) -> Option<(usize, usize)> {
        let mut value: usize = 0;
        for shift in 0..5 {
            let b = *blob.get(at)?;
            at += 1;
            value |= ((b & 0x7F) as usize) << (shift * 7);
            if b & 0x80 == 0 {
                return Some((value, at));
            }
        }
        None
    }

    /// The first (whole) key of a block, without allocating.
    fn block_head(&self, block: usize) -> Option<&[u8]> {
        let blob = self.keys();
        let at = *self.block_offs.get(block)? as usize;
        let (len, start) = Self::varint(blob, at)?;
        blob.get(start..start + len)
    }

    /// Walk a block from its head, calling `f` with (entry index, key bytes) until it returns false
    /// or the block ends. The key buffer is reused across entries, so `f` must not retain it.
    fn walk_block(&self, block: usize, buf: &mut Vec<u8>, mut f: impl FnMut(usize, &[u8]) -> bool) {
        let blob = self.keys();
        let Some(&start) = self.block_offs.get(block) else {
            return;
        };
        let end = self
            .block_offs
            .get(block + 1)
            .map(|&o| o as usize)
            .unwrap_or(self.keys_len);
        let mut at = start as usize;
        let base = block * self.block_len;

        buf.clear();
        for slot in 0..self.block_len {
            let i = base + slot;
            if i >= self.count || at >= end {
                return;
            }
            if slot == 0 {
                let Some((len, next)) = Self::varint(blob, at) else {
                    return;
                };
                let Some(bytes) = blob.get(next..next + len) else {
                    return;
                };
                buf.extend_from_slice(bytes);
                at = next + len;
            } else {
                let Some((shared, n1)) = Self::varint(blob, at) else {
                    return;
                };
                let Some((rest, n2)) = Self::varint(blob, n1) else {
                    return;
                };
                if shared > buf.len() {
                    return; // damaged: cannot share more than the previous key has
                }
                let Some(bytes) = blob.get(n2..n2 + rest) else {
                    return;
                };
                buf.truncate(shared);
                buf.extend_from_slice(bytes);
                at = n2 + rest;
            }
            if !f(i, buf) {
                return;
            }
        }
    }

    /// Index of the first entry whose key is >= `target`, i.e. `std::lower_bound`.
    fn lower_bound(&self, target: &[u8]) -> usize {
        // Find the last block whose head is <= target; the answer is inside it, or at the very
        // start if every head is already greater.
        let mut lo = 0usize;
        let mut hi = self.block_offs.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.block_head(mid) {
                Some(head) if head < target => lo = mid + 1,
                _ => hi = mid,
            }
        }
        // `lo` is the first block whose head is >= target. If that is block 0 the answer is 0;
        // otherwise the entry lies within the preceding block (or at `lo`'s head exactly).
        if lo == 0 {
            return 0;
        }
        let block = lo - 1;
        let mut found = block * self.block_len;
        let mut hit = None;
        let mut buf = Vec::new();
        self.walk_block(block, &mut buf, |i, key| {
            if key >= target {
                hit = Some(i);
                false
            } else {
                found = i + 1;
                true
            }
        });
        hit.unwrap_or(found)
    }

    /// The record for an exact key, or None. When a key appears more than once the first is
    /// returned, matching `marisa_trie`'s `.get(key)[0]` as core.py uses it.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        let target = key.as_bytes();
        let i = self.lower_bound(target);
        if i >= self.count {
            return None;
        }
        let mut matched = false;
        let mut buf = Vec::new();
        self.walk_block(i / self.block_len, &mut buf, |j, k| {
            if j < i {
                return true;
            }
            if j == i {
                matched = k == target;
            }
            false
        });
        if matched {
            Some(self.record(i))
        } else {
            None
        }
    }

    /// Every entry whose key starts with `prefix`, in key order, as (key, record).
    ///
    /// The key is copied because it is reconstructed from front-coding and the buffer is reused;
    /// callers keep these anyway (they split them on a tab).
    pub fn prefix_iter(&self, prefix: &str) -> Vec<(String, &[u8])> {
        let p = prefix.as_bytes();
        let start = self.lower_bound(p);
        if start >= self.count {
            return Vec::new();
        }
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut buf = Vec::new();
        let mut block = start / self.block_len;
        'outer: while block < self.block_offs.len() {
            let mut stop = false;
            self.walk_block(block, &mut buf, |i, key| {
                if i < start {
                    return true;
                }
                if !key.starts_with(p) {
                    stop = true;
                    return false;
                }
                // Keys are written from Python `str`, so they are valid UTF-8 by construction; a
                // damaged file is skipped rather than allowed to panic inside a keystroke.
                if let Ok(s) = std::str::from_utf8(key) {
                    out.push((s.to_string(), i));
                }
                true
            });
            if stop {
                break 'outer;
            }
            block += 1;
        }
        out.into_iter().map(|(s, i)| (s, self.record(i))).collect()
    }

    /// Every entry whose key starts with `prefix`, in the order the *source trie* yielded them.
    ///
    /// The engine inserts candidates into an ordered table in this order, and equal scores are then
    /// broken by insertion order, so for the tables where that reaches the ranker unsorted this is
    /// the order that must be used -- not key order. `marisa_trie` enumerates a LOUDS traversal,
    /// which is not lexicographic: for "screensaver" it yields স্ক্রিনসেভারের before its own prefix
    /// স্ক্রিনসেভার, and eleven percent of sampled prefixes differ from key order. The build step
    /// records each entry's position in that enumeration; see `write_lkx` in
    /// `scripts/build_rust_data.py`.
    ///
    /// On a table built without ranks this is identical to `prefix_iter`.
    pub fn prefix_iter_source_order(&self, prefix: &str) -> Vec<(String, &[u8])> {
        let p = prefix.as_bytes();
        let start = self.lower_bound(p);
        if start >= self.count {
            return Vec::new();
        }
        let mut hits: Vec<(String, usize)> = Vec::new();
        let mut buf = Vec::new();
        let mut block = start / self.block_len;
        while block < self.block_offs.len() {
            let mut stop = false;
            self.walk_block(block, &mut buf, |i, key| {
                if i < start {
                    return true;
                }
                if !key.starts_with(p) {
                    stop = true;
                    return false;
                }
                if let Ok(s) = std::str::from_utf8(key) {
                    hits.push((s.to_string(), i));
                }
                true
            });
            if stop {
                break;
            }
            block += 1;
        }
        hits.sort_by_key(|&(_, i)| self.rank(i));
        hits.into_iter().map(|(s, i)| (s, self.record(i))).collect()
    }

    /// Every entry in key order. Used once, to total the unigram counts at startup.
    pub fn iter_all(&self) -> Vec<(String, &[u8])> {
        let mut out: Vec<(String, usize)> = Vec::with_capacity(self.count);
        let mut buf = Vec::new();
        for block in 0..self.block_offs.len() {
            self.walk_block(block, &mut buf, |i, key| {
                if let Ok(s) = std::str::from_utf8(key) {
                    out.push((s.to_string(), i));
                }
                true
            });
        }
        out.into_iter().map(|(s, i)| (s, self.record(i))).collect()
    }
}

/// Record accessors. The layouts come from the `RecordTrie` format strings in core.py and are
/// checked against `record_size` when the table is opened, so a mismatched file fails at load
/// rather than returning silent nonsense.
pub fn rec_u32(r: &[u8]) -> u32 {
    u32::from_le_bytes([r[0], r[1], r[2], r[3]])
}

/// `"<III"` -- the wiki, subtitle and chat counts of a unigram.
pub fn rec_u32x3(r: &[u8]) -> (u32, u32, u32) {
    (
        u32::from_le_bytes([r[0], r[1], r[2], r[3]]),
        u32::from_le_bytes([r[4], r[5], r[6], r[7]]),
        u32::from_le_bytes([r[8], r[9], r[10], r[11]]),
    )
}

/// `"<Ib"` -- an attestation count and a signed source id.
pub fn rec_u32_i8(r: &[u8]) -> (u32, i8) {
    (u32::from_le_bytes([r[0], r[1], r[2], r[3]]), r[4] as i8)
}
