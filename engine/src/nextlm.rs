//! The optional neural next-word model: a small word-level LSTM that reads the whole sentence.
//!
//! Counting what follows a word stops helping after about two words of context (measured with
//! `likhi-nextword --context`: 9.6% right from one word, 11.4% from two, 11.4% from three), because
//! longer word sequences are too rare to count. A recurrent model generalises instead of counting:
//! it has seen "অফিসে প্রচুর কাজ" and can use that for "অফিসে অনেক কাজ". Trained offline (the
//! trainer and exporter live outside this repository's build) and downloaded only by people who
//! ask for it, so the base installation stays as small as it is.
//!
//! File format, LKM1 (little-endian): b"LKM1", u32 version = 1, u32 header length, a JSON header
//! `{"meta": {...}, "arrays": [{name, dtype, shape, offset, bytes}]}`, zero padding to 64 bytes,
//! then the data block. The large matrices are 8-bit, one scale per row -- the embedding (which is
//! also the output layer) and the LSTM weights -- and the rest f32. Mapped, like `weights.rs`, so
//! the pages actually touched are what is resident.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use memmap2::Mmap;
use serde::Deserialize;

use crate::core::never_suggested;
use crate::romankey::{key_from_bangla, Level};
use crate::textnorm::{has_bengali, to_output, tokens};

const MAGIC: &[u8; 4] = b"LKM1";

#[derive(Debug, Clone, Deserialize)]
pub struct LmMeta {
    pub vocab: usize,
    pub emb: usize,
    pub hidden: usize,
    pub layers: usize,
    pub bos: usize,
    pub unk: usize,
}

#[derive(Debug, Deserialize)]
struct ArrayIndex {
    name: String,
    dtype: String,
    shape: Vec<usize>,
    offset: usize,
    bytes: usize,
}

#[derive(Debug, Deserialize)]
struct Header {
    meta: LmMeta,
    arrays: Vec<ArrayIndex>,
}

#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    rows: usize,
    cols: usize,
}

/// One 8-bit matrix: rows of `i8` and a scale for each.
#[derive(Debug, Clone, Copy)]
struct Q8 {
    q: Span,
    scale: Span,
}

#[derive(Debug)]
pub struct LmError(pub String);

impl std::fmt::Display for LmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "next-word model: {}", self.0)
    }
}

impl std::error::Error for LmError {}

/// What the LSTM carries from one word to the next: (h, c) for every layer.
#[derive(Debug, Clone)]
pub struct LmState {
    h: Vec<Vec<f32>>,
    c: Vec<Vec<f32>>,
    /// The projected output after the last word read, ready to score the next one.
    out: Vec<f32>,
}

pub struct NextLm {
    map: Mmap,
    pub meta: LmMeta,
    embed: Q8,
    w_ih: Vec<Q8>,
    w_hh: Vec<Q8>,
    bias: Vec<Span>,
    proj_w: Span,
    proj_b: Span,
    pub vocab: Vec<String>,
    ids: HashMap<String, usize>,
}

impl NextLm {
    /// Open `model.lkm` and its `vocab.txt` from `dir`.
    pub fn open(dir: &Path) -> Result<NextLm, LmError> {
        let err = |m: String| LmError(m);
        let file = File::open(dir.join("model.lkm")).map_err(|e| err(format!("model.lkm: {e}")))?;
        // SAFETY: read-only mapping, never written through, as in weights.rs.
        let map = unsafe { Mmap::map(&file) }.map_err(|e| err(format!("mapping: {e}")))?;
        if map.len() < 12 || &map[..4] != MAGIC {
            return Err(err("not an LKM1 file".into()));
        }
        let version = u32::from_le_bytes([map[4], map[5], map[6], map[7]]);
        if version != 1 {
            return Err(err(format!("version {version}, expected 1")));
        }
        let hlen = u32::from_le_bytes([map[8], map[9], map[10], map[11]]) as usize;
        let hend = 12usize.checked_add(hlen).filter(|e| *e <= map.len()).ok_or_else(|| err("header runs off the end".into()))?;
        let header: Header = serde_json::from_slice(&map[12..hend]).map_err(|e| err(format!("header: {e}")))?;
        let data = hend.next_multiple_of(64);
        if !(map.as_ptr() as usize + data).is_multiple_of(4) {
            return Err(err("data block is not 4-byte aligned".into()));
        }
        let mut spans: HashMap<String, (String, Span)> = HashMap::new();
        for a in &header.arrays {
            let (rows, cols) = match a.shape.as_slice() {
                [n] => (*n, 1),
                [r, c] => (*r, *c),
                _ => return Err(err(format!("{}: unexpected shape {:?}", a.name, a.shape))),
            };
            let width = if a.dtype == "i8" { 1 } else { 4 };
            if rows * cols * width != a.bytes || data + a.offset + a.bytes > map.len() || a.offset % 4 != 0 {
                return Err(err(format!("{}: size or offset does not fit the file", a.name)));
            }
            spans.insert(a.name.clone(), (a.dtype.clone(), Span { start: data + a.offset, rows, cols }));
        }
        let get = |name: &str, dtype: &str| -> Result<Span, LmError> {
            match spans.get(name) {
                Some((d, s)) if d == dtype => Ok(*s),
                Some((d, _)) => Err(err(format!("{name} is {d}, expected {dtype}"))),
                None => Err(err(format!("no array named {name}"))),
            }
        };
        let q8 = |name: &str| -> Result<Q8, LmError> {
            Ok(Q8 { q: get(&format!("{name}.q"), "i8")?, scale: get(&format!("{name}.scale"), "f32")? })
        };
        let m = header.meta.clone();
        let embed = q8("embed")?;
        let mut w_ih = Vec::new();
        let mut w_hh = Vec::new();
        let mut bias = Vec::new();
        for l in 0..m.layers {
            w_ih.push(q8(&format!("lstm.{l}.w_ih"))?);
            w_hh.push(q8(&format!("lstm.{l}.w_hh"))?);
            bias.push(get(&format!("lstm.{l}.bias"), "f32")?);
        }
        let proj_w = get("proj.w", "f32")?;
        let proj_b = get("proj.b", "f32")?;
        if embed.q.rows != m.vocab || embed.q.cols != m.emb || proj_w.rows != m.emb || proj_w.cols != m.hidden {
            return Err(err("array shapes do not match the header".into()));
        }
        let vocab: Vec<String> = std::fs::read_to_string(dir.join("vocab.txt"))
            .map_err(|e| err(format!("vocab.txt: {e}")))?
            .lines()
            .map(str::to_string)
            .collect();
        if vocab.len() != m.vocab {
            return Err(err(format!("vocab.txt has {} words, the model {}", vocab.len(), m.vocab)));
        }
        let ids = vocab.iter().enumerate().map(|(i, w)| (w.clone(), i)).collect();
        Ok(NextLm { map, meta: m, embed, w_ih, w_hh, bias, proj_w, proj_b, vocab, ids })
    }

    fn f32s(&self, s: Span) -> &[f32] {
        // SAFETY: bounds and 4-byte alignment were checked in `open`; f32 has no invalid patterns.
        unsafe { std::slice::from_raw_parts(self.map.as_ptr().add(s.start) as *const f32, s.rows * s.cols) }
    }

    fn i8s(&self, s: Span) -> &[i8] {
        // SAFETY: bounds were checked in `open`; any byte is a valid i8.
        unsafe { std::slice::from_raw_parts(self.map.as_ptr().add(s.start) as *const i8, s.rows * s.cols) }
    }

    /// The id of a word as the keyboard writes it, or the unknown-word id.
    pub fn id(&self, word: &str) -> usize {
        self.ids.get(word).copied().unwrap_or(self.meta.unk)
    }

    /// `out[r] += scale[r] * dot(q[r], x)` for every row: an 8-bit matrix times a vector.
    fn q8_matvec(&self, m: Q8, x: &[f32], out: &mut [f32]) {
        let q = self.i8s(m.q);
        let scale = self.f32s(m.scale);
        let cols = m.q.cols;
        let (xq, xs) = quantize_i16(x);
        for (r, o) in out.iter_mut().enumerate().take(m.q.rows) {
            *o += dot_i8_i16(&q[r * cols..(r + 1) * cols], &xq) as f32 * xs * scale[r];
        }
    }

    /// The state before any word: the start-of-sentence marker read.
    pub fn start(&self) -> LmState {
        let layers = self.meta.layers;
        let hsz = self.meta.hidden;
        let mut s = LmState { h: vec![vec![0.0; hsz]; layers], c: vec![vec![0.0; hsz]; layers], out: Vec::new() };
        self.step(&mut s, self.meta.bos);
        s
    }

    /// Read one more word.
    pub fn step(&self, s: &mut LmState, id: usize) {
        let hsz = self.meta.hidden;
        let e = self.embed;
        let row = &self.i8s(e.q)[id * e.q.cols..(id + 1) * e.q.cols];
        let sc = self.f32s(e.scale)[id];
        let mut x: Vec<f32> = row.iter().map(|v| f32::from(*v) * sc).collect();
        for l in 0..self.meta.layers {
            let mut gates = self.f32s(self.bias[l]).to_vec();
            self.q8_matvec(self.w_ih[l], &x, &mut gates);
            self.q8_matvec(self.w_hh[l], &s.h[l], &mut gates);
            // PyTorch's gate order: input, forget, cell, output.
            for j in 0..hsz {
                let i = sigmoid(gates[j]);
                let f = sigmoid(gates[hsz + j]);
                let g = gates[2 * hsz + j].tanh();
                let o = sigmoid(gates[3 * hsz + j]);
                s.c[l][j] = f * s.c[l][j] + i * g;
                s.h[l][j] = o * s.c[l][j].tanh();
            }
            x = s.h[l].clone();
        }
        // proj.w is stored as PyTorch keeps it, [out, in]: one output per row.
        let w = self.f32s(self.proj_w);
        let mut out = self.f32s(self.proj_b).to_vec();
        for (r, o) in out.iter_mut().enumerate() {
            *o += dot_f32(&w[r * hsz..(r + 1) * hsz], &x);
        }
        s.out = out;
    }

    /// Raw scores for the next word, for the ids asked about (all of them when `ids` is None).
    /// Higher is likelier; a softmax over the same set turns them into shares.
    pub fn scores(&self, s: &LmState, ids: Option<&[usize]>) -> Vec<(usize, f32)> {
        let e = self.embed;
        let q = self.i8s(e.q);
        let scale = self.f32s(e.scale);
        let cols = e.q.cols;
        let (xq, xs) = quantize_i16(&s.out);
        let score = |id: usize| -> f32 { dot_i8_i16(&q[id * cols..(id + 1) * cols], &xq) as f32 * xs * scale[id] };
        match ids {
            Some(ids) => ids.iter().map(|&i| (i, score(i))).collect(),
            None => (0..self.meta.vocab).map(|i| (i, score(i))).collect(),
        }
    }
}

/// The vector an 8-bit matrix is multiplied by, as 16-bit integers and one scale, so each row is an
/// integer dot product: exact in any order, which lets the compiler vectorize it freely, and far
/// cheaper than turning every weight into a float. The largest value is kept small enough that a
/// row of `x.len()` products of 127 cannot overflow an i32. At 16 bits the rounding is some two
/// hundred times finer than the 8-bit weights', so it adds nothing measurable to their error.
pub(crate) fn quantize_i16(x: &[f32]) -> (Vec<i16>, f32) {
    let limit = (i32::MAX as usize / (127 * x.len().max(1))).min(i16::MAX as usize) as f32;
    let max = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if max == 0.0 || !max.is_finite() {
        return (vec![0; x.len()], 0.0);
    }
    let scale = max / limit;
    (x.iter().map(|v| (v / scale).round() as i16).collect(), scale)
}

pub(crate) fn dot_i8_i16(a: &[i8], x: &[i16]) -> i32 {
    a.iter().zip(x).map(|(a, b)| i32::from(*a) * i32::from(*b)).sum()
}

/// Lanes summed independently. One running total makes every addition wait for the last, which
/// the compiler may not reorder for floats; eight let it keep a vector register busy.
const LANES: usize = 8;

fn dot_f32(a: &[f32], x: &[f32]) -> f32 {
    let mut acc = [0.0f32; LANES];
    let ((ac, ar), (xc, xr)) = (a.as_chunks::<LANES>(), x.as_chunks::<LANES>());
    let tail: f32 = ar.iter().zip(xr).map(|(a, b)| a * b).sum();
    for (a, x) in ac.iter().zip(xc) {
        for ((s, a), x) in acc.iter_mut().zip(a).zip(x) {
            *s += a * x;
        }
    }
    acc.iter().sum::<f32>() + tail
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Log-probabilities over a set of scores (a softmax restricted to that set).
pub fn log_softmax(scores: &mut [(usize, f32)]) {
    let max = scores.iter().map(|s| s.1).fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = scores.iter().map(|s| (s.1 - max).exp()).sum();
    let log_sum = sum.ln() + max;
    for s in scores.iter_mut() {
        s.1 -= log_sum;
    }
}

// ------------------------------------------------------------------------------ prediction

/// The longest context read, in words. The model was trained on windows of 48; a sentence of chat
/// rarely runs past a dozen.
const CONTEXT_KEPT: usize = 40;

/// What the model expects after one context: a probability for every word it may offer.
struct Distribution {
    /// The context, as the model's own tokens.
    context: Vec<String>,
    /// The LSTM's state after reading it, kept so the next word is one step rather than a whole
    /// sentence. None for the Likhi model, which reads its few words afresh each time.
    state: Option<LmState>,
    /// Indexed by word id, summing to one; zero for the words never offered.
    probs: Vec<f32>,
}

/// Every word's coarse phonetic key, and the ids sorted by it, so that the words some typed letters
/// could begin are one contiguous run, found by binary search.
struct KeyIndex {
    keys: Vec<String>,
    order: Vec<u32>,
}

impl KeyIndex {
    fn build(vocab: &[String]) -> KeyIndex {
        let keys: Vec<String> = vocab.iter().map(|w| key_from_bangla(w, Level::Coarse)).collect();
        let mut order: Vec<u32> = (0..keys.len() as u32).collect();
        order.sort_by(|a, b| keys[*a as usize].cmp(&keys[*b as usize]));
        KeyIndex { keys, order }
    }

    /// The ids whose key begins with `prefix`; all of them for an empty one.
    fn starting_with(&self, prefix: &str) -> &[u32] {
        let key = |id: &u32| self.keys[*id as usize].as_str();
        let start = self.order.partition_point(|id| key(id) < prefix);
        let len = self.order[start..].partition_point(|id| key(id).starts_with(prefix));
        &self.order[start..start + len]
    }
}

#[derive(Default)]
struct Slot {
    /// The context to work out next. A newer one replaces it: by then it is stale.
    queued: Option<Vec<String>>,
    ready: Option<Arc<Distribution>>,
    /// Built by the worker as it starts, so opening the model never holds up the engine.
    index: Option<Arc<KeyIndex>>,
}

/// The neural model behind next-word suggestions, run off the keystroke path.
///
/// A keystroke never waits for it. When a word is committed, `prepare` hands the new context to a
/// worker thread, which reads the one new word onto the state it kept and scores every word it
/// knows -- about 3 ms on a desktop, perhaps 15 on an old laptop, long before the next letter
/// arrives. The letters of the next word then only look the answer up. Asked about a context it
/// has not finished, it says so, and the caller falls back to the counted table for that key.
pub struct Predictor {
    source: Arc<Source>,
    shared: Arc<(Mutex<Slot>, Condvar)>,
}

/// Where the next-word distribution comes from: the separate next-word LSTM, or the word head of
/// the Likhi model, which is the same network that transliterates.
enum Source {
    Lstm(Box<NextLm>),
    Likhi(Arc<crate::xlit::Xlit>),
}

impl Source {
    fn vocab(&self) -> &[String] {
        match self {
            Source::Lstm(lm) => &lm.vocab,
            Source::Likhi(x) => x.words(),
        }
    }
}

impl Predictor {
    /// Open the next-word LSTM in `dir` and start its worker.
    pub fn open(dir: &Path) -> Result<Predictor, LmError> {
        Self::start(Source::Lstm(Box::new(NextLm::open(dir)?)))
    }

    /// Next words from the Likhi model's word head. An error for a model without one.
    pub fn from_likhi(model: Arc<crate::xlit::Xlit>) -> Result<Predictor, LmError> {
        if model.words().is_empty() {
            return Err(LmError("the Likhi model has no word head".into()));
        }
        Self::start(Source::Likhi(model))
    }

    fn start(source: Source) -> Result<Predictor, LmError> {
        let source = Arc::new(source);
        let shared = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
        let (for_worker, shared_worker) = (Arc::clone(&source), Arc::clone(&shared));
        std::thread::Builder::new()
            .name("likhi-nextlm".into())
            .spawn(move || predict_worker(&for_worker, &shared_worker))
            .map_err(|e| LmError(format!("worker thread: {e}")))?;
        Ok(Predictor { source, shared })
    }

    /// The model's tokens for a context given as the words were typed: the Bangla words only,
    /// canonical, at most the last `CONTEXT_KEPT`.
    fn tokens_of(context: &[String]) -> Vec<String> {
        let mut t = tokens(&context.join(" "));
        let over = t.len().saturating_sub(CONTEXT_KEPT);
        t.drain(..over);
        t
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.shared.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether an answer worked out for `had` serves a question about `asked`: the same words, or
    /// more of them before. A keyboard that sends only the last few words is then still answered
    /// from everything the engine was told when each was committed -- more of the real sentence,
    /// which only helps. Nothing before is not "any context", though: it is the start of a text.
    fn serves(had: &[String], asked: &[String]) -> bool {
        if asked.is_empty() {
            had.is_empty()
        } else {
            had.ends_with(asked)
        }
    }

    fn queue(&self, slot: &mut Slot, context: Vec<String>) {
        let served = |had: &Vec<String>| Self::serves(had, &context);
        if slot.ready.as_ref().is_some_and(|d| served(&d.context)) || slot.queued.as_ref().is_some_and(served) {
            return;
        }
        slot.queued = Some(context);
        self.shared.1.notify_one();
    }

    /// Start working out what follows `context`. Called when a word is committed, with the context
    /// it was typed in and the word itself, so the answer is ready before the next letter.
    pub fn prepare(&self, context: &[String]) {
        let t = Self::tokens_of(context);
        let mut slot = self.lock();
        self.queue(&mut slot, t);
    }

    /// The `k` likeliest words to follow `context` whose coarse phonetic key begins with
    /// `typed_key` -- any word, for an empty key -- each with its share of all the words that fit,
    /// likeliest first. None while the answer for this context is still being worked out; asking
    /// starts it.
    pub fn best(&self, context: &[String], typed_key: &str, k: usize) -> Option<Vec<(String, f32)>> {
        let t = Self::tokens_of(context);
        let (d, index) = {
            let mut slot = self.lock();
            match (&slot.ready, &slot.index) {
                (Some(d), Some(ix)) if Self::serves(&d.context, &t) => (Arc::clone(d), Arc::clone(ix)),
                _ => {
                    self.queue(&mut slot, t);
                    return None;
                }
            }
        };
        let fitting = index.starting_with(typed_key);
        let total: f32 = fitting.iter().map(|id| d.probs[*id as usize]).sum();
        let mut top: Vec<(f32, u32)> = Vec::with_capacity(k + 1);
        for &id in fitting {
            let p = d.probs[id as usize];
            if p > 0.0 && (top.len() < k || p > top[top.len() - 1].0) {
                let at = top.partition_point(|(q, _)| *q >= p);
                top.insert(at, (p, id));
                top.truncate(k);
            }
        }
        let vocab = self.source.vocab();
        Some(top.into_iter().map(|(p, id)| (to_output(&vocab[id as usize]), p / total)).collect())
    }
}

impl Predictor {
    /// `best`, waiting up to `wait` for the answer if it is being worked out. For the moment
    /// straight after a commit, whose learn request asked for this very context a millisecond ago:
    /// on a desktop the answer takes about three, so a short wait turns a fallback to the counted
    /// table into the model's answer. Never for a keystroke that types a letter.
    pub fn best_within(&self, context: &[String], typed_key: &str, k: usize, wait: std::time::Duration) -> Option<Vec<(String, f32)>> {
        let t = Self::tokens_of(context);
        let deadline = std::time::Instant::now() + wait;
        let mut slot = self.lock();
        loop {
            if slot.index.is_some() && slot.ready.as_ref().is_some_and(|d| Self::serves(&d.context, &t)) {
                drop(slot);
                return self.best(context, typed_key, k);
            }
            self.queue(&mut slot, t.clone());
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            slot = self.shared.1.wait_timeout(slot, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

fn predict_worker(source: &Source, shared: &(Mutex<Slot>, Condvar)) {
    let (lock, cv) = shared;
    let vocab = source.vocab();
    // Offered: real Bangla words that are not on the never-suggested list. The markers for padding,
    // unknown words and the sentence start have no Bangla in them.
    let allowed: Vec<bool> = vocab.iter().map(|w| has_bengali(w) && !never_suggested(w)).collect();
    let index = Arc::new(KeyIndex::build(vocab));
    lock.lock().unwrap_or_else(|e| e.into_inner()).index = Some(index);
    loop {
        let (want, before) = {
            let mut slot = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(q) = slot.queued.take() {
                    break (q, slot.ready.clone());
                }
                slot = cv.wait(slot).unwrap_or_else(|e| e.into_inner());
            }
        };
        let (scores, state): (Vec<f32>, Option<LmState>) = match source {
            Source::Lstm(lm) => {
                // Usually the context is the last one plus the word just committed: one step.
                let (mut state, read) = match before.as_ref().and_then(|d| d.state.clone().map(|s| (d, s))) {
                    Some((d, s)) if want.starts_with(&d.context) => (s, d.context.len()),
                    _ => (lm.start(), 0),
                };
                for w in &want[read..] {
                    lm.step(&mut state, lm.id(w));
                }
                (lm.scores(&state, None).into_iter().map(|s| s.1).collect(), Some(state))
            }
            // The Likhi model in completion mode with no letters: its word head's scores.
            Source::Likhi(x) => {
                let enc = x.encode_ctx(true, &want, "");
                (x.word_scores(&enc).unwrap_or_else(|| vec![f32::NEG_INFINITY; vocab.len()]), None)
            }
        };
        let max = scores
            .iter()
            .zip(&allowed)
            .filter(|(_, ok)| **ok)
            .map(|(s, _)| *s)
            .fold(f32::NEG_INFINITY, f32::max);
        let mut probs = vec![0.0f32; vocab.len()];
        let mut sum = 0.0f32;
        for (id, s) in scores.into_iter().enumerate() {
            if allowed[id] {
                probs[id] = (s - max).exp();
                sum += probs[id];
            }
        }
        if sum > 0.0 {
            for p in &mut probs {
                *p /= sum;
            }
        }
        let done = Arc::new(Distribution { context: want, state, probs });
        lock.lock().unwrap_or_else(|e| e.into_inner()).ready = Some(done);
        // For `best_within`, which may be waiting for exactly this answer.
        cv.notify_all();
    }
}