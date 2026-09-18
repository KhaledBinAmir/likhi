//! Reader for the `.lkw` weight bundle written by `scripts/build_rust_data.py`.
//!
//! Layout:
//!
//!     magic     4    b"LKW1"
//!     version   4    u32 = 1
//!     index_len 4    u32
//!     index     index_len bytes of UTF-8 JSON: {"meta": {...}, "arrays": [{name, shape, offset, count}]}
//!     (pad to 64)
//!     data      f32, little-endian; an array's `offset` and `count` are in f32 elements
//!
//! The file is mapped rather than read. It is 45 MB and the engine is a background process that
//! spends most of its life idle: mapping means the pages the decoder actually touches are resident
//! and the rest can be evicted, and it means startup does no work proportional to the file size.
//!
//! Everything the Python does at load time -- transposes, the fused q/k/v projection with the
//! attention scaling folded into q, the sinusoidal position table, the banned-token mask -- was
//! already done by the builder in NumPy, so nothing here computes anything. That is deliberate: it
//! removes a whole class of porting bug, because those steps cannot be got subtly wrong twice.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use memmap2::Mmap;
use serde::Deserialize;

const MAGIC: &[u8; 4] = b"LKW1";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub dim: usize,
    pub heads: usize,
    pub head_dim: usize,
    pub encoder_layers: usize,
    pub decoder_layers: usize,
    pub pre_norm: bool,
    pub activation: String,
    pub embed_scale: f32,
    pub padding_idx: usize,
    pub pos_offset: usize,
    pub max_positions: usize,
    pub scaling: f32,
}

#[derive(Debug, Deserialize)]
struct ArrayIndex {
    name: String,
    shape: Vec<usize>,
    offset: usize,
    count: usize,
}

#[derive(Debug, Deserialize)]
struct Index {
    meta: Meta,
    arrays: Vec<ArrayIndex>,
}

/// Where one array lives in the blob. Offsets rather than slices, so the owning struct is not
/// self-referential: the model holds these and resolves them against `data()` on use.
#[derive(Debug, Clone, Copy)]
pub struct Arr {
    pub off: usize,
    pub rows: usize,
    pub cols: usize,
}

impl Arr {
    pub const EMPTY: Arr = Arr { off: 0, rows: 0, cols: 0 };

    pub fn len(&self) -> usize {
        if self.cols == 0 {
            self.rows
        } else {
            self.rows * self.cols
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub enum WeightError {
    Io(std::io::Error),
    Corrupt(String),
    Missing(String),
}

impl std::fmt::Display for WeightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeightError::Io(e) => write!(f, "{e}"),
            WeightError::Corrupt(m) => write!(f, "damaged weight file: {m}"),
            WeightError::Missing(n) => write!(f, "weight file has no array named {n}"),
        }
    }
}

impl std::error::Error for WeightError {}

impl From<std::io::Error> for WeightError {
    fn from(e: std::io::Error) -> Self {
        WeightError::Io(e)
    }
}

pub struct Weights {
    map: Mmap,
    data_start: usize,
    data_len: usize,
    index: HashMap<String, Arr>,
    pub meta: Meta,
}

impl Weights {
    pub fn open(path: &Path) -> Result<Weights, WeightError> {
        let file = File::open(path)?;
        // SAFETY: read-only mapping, never aliased mutably. See the note in lexicon.rs.
        let map = unsafe { Mmap::map(&file)? };

        if map.len() < 12 {
            return Err(WeightError::Corrupt("shorter than a header".into()));
        }
        if &map[0..4] != MAGIC {
            return Err(WeightError::Corrupt("wrong magic; not an .lkw bundle".into()));
        }
        let version = u32::from_le_bytes([map[4], map[5], map[6], map[7]]);
        if version != VERSION {
            return Err(WeightError::Corrupt(format!("version {version}, expected {VERSION}")));
        }
        let index_len = u32::from_le_bytes([map[8], map[9], map[10], map[11]]) as usize;
        let index_end = 12usize
            .checked_add(index_len)
            .ok_or_else(|| WeightError::Corrupt("index length overflows".into()))?;
        if map.len() < index_end {
            return Err(WeightError::Corrupt("file ends inside the index".into()));
        }
        let parsed: Index = serde_json::from_slice(&map[12..index_end])
            .map_err(|e| WeightError::Corrupt(format!("index is not valid JSON: {e}")))?;

        let data_start = index_end.next_multiple_of(64);
        if map.len() < data_start {
            return Err(WeightError::Corrupt("file ends before the data block".into()));
        }
        // The builder pads the header to 64 bytes and a mapping is page-aligned, so the data block
        // begins on a 4-byte boundary. Checked rather than assumed: reinterpreting misaligned bytes
        // as f32 is undefined behaviour, not merely slow.
        let base = map.as_ptr() as usize + data_start;
        if base % std::mem::align_of::<f32>() != 0 {
            return Err(WeightError::Corrupt("data block is not f32-aligned".into()));
        }
        let data_len = (map.len() - data_start) / 4;

        let mut index = HashMap::with_capacity(parsed.arrays.len());
        for a in &parsed.arrays {
            let (rows, cols) = match a.shape.len() {
                0 => (0, 0),
                1 => (a.shape[0], 0),
                2 => (a.shape[0], a.shape[1]),
                n => {
                    return Err(WeightError::Corrupt(format!(
                        "{}: {n} dimensions; only 1-D and 2-D arrays are used",
                        a.name
                    )))
                }
            };
            let arr = Arr { off: a.offset, rows, cols };
            if arr.len() != a.count {
                return Err(WeightError::Corrupt(format!(
                    "{}: shape {:?} holds {} values but count says {}",
                    a.name,
                    a.shape,
                    arr.len(),
                    a.count
                )));
            }
            if a.offset + a.count > data_len {
                return Err(WeightError::Corrupt(format!(
                    "{}: runs past the end of the data block",
                    a.name
                )));
            }
            index.insert(a.name.clone(), arr);
        }

        Ok(Weights { map, data_start, data_len, index, meta: parsed.meta })
    }

    fn data(&self) -> &[f32] {
        // SAFETY: alignment was checked in `open`, the range is inside the mapping, and f32 has no
        // invalid bit patterns, so every 4 bytes in range is a valid (possibly NaN) f32.
        unsafe {
            std::slice::from_raw_parts(
                self.map.as_ptr().add(self.data_start) as *const f32,
                self.data_len,
            )
        }
    }

    /// Look an array up by name. Absent is an error: every caller names an array the builder is
    /// required to emit, so a miss is a version mismatch and must be loud.
    pub fn arr(&self, name: &str) -> Result<Arr, WeightError> {
        self.index
            .get(name)
            .copied()
            .ok_or_else(|| WeightError::Missing(name.to_string()))
    }

    /// An array that the checkpoint may legitimately not contain (an optional layer norm). The
    /// builder writes those with shape `[0]`, so both "absent" and "present but empty" arrive here
    /// as an empty `Arr`.
    pub fn arr_opt(&self, name: &str) -> Arr {
        self.index.get(name).copied().unwrap_or(Arr::EMPTY)
    }

    pub fn get(&self, a: Arr) -> &[f32] {
        &self.data()[a.off..a.off + a.len()]
    }

    /// One row of a 2-D array: embedding lookups and position tables.
    pub fn row(&self, a: Arr, i: usize) -> &[f32] {
        debug_assert!(a.cols > 0, "row() on a 1-D array");
        debug_assert!(i < a.rows, "row {i} of {} rows", a.rows);
        let start = a.off + i * a.cols;
        &self.data()[start..start + a.cols]
    }
}
