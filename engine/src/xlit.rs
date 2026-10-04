//! The IndicXlit character transformer: a port of `src/likhi/engine/xlit_np.py`.
//!
//! A 6+6 layer pre-norm fairseq `transformer`, 256-dimensional, 4 heads, 11.8M parameters, decoding
//! one word at a time. It supplies two things to the ranker: a beam search over spellings the
//! lexicon may never have seen, and a teacher-forced score for candidates the other channels
//! produced.
//!
//! Faithful to fairseq: scaled embeddings, sinusoidal positions offset by padding_idx+1,
//! layernorm_embedding, the tanh GELU approximation the Python chose, final layer norms, and beam
//! search with length-normalized scores.
//!
//! **Where this can differ from the Python, and why that is acceptable.** NumPy's f32 matrix
//! products sum in a different order than the loops in `tensor.rs`, so activations differ in their
//! last bits and log-probabilities by perhaps 1e-5. That cannot be eliminated without
//! reimplementing a BLAS's exact blocking. It does not matter, because beam candidates are
//! separated by tenths of a nat, not by 1e-5 -- and `tests/goldens.rs` checks precisely that claim
//! by replaying several hundred real words and requiring the *same words in the same order*.

use std::collections::HashMap;
use std::path::Path;

use crate::tensor::{add_inplace, attention, gelu_inplace, layer_norm, log_softmax_inplace, matmul_bias, relu_inplace};
use crate::weights::{Arr, Meta, WeightError, Weights};

/// The Likhi model's source tokens: its two modes, the separator
/// between the sentence so far and the letters, and how many words of the sentence it reads.
const MODE_TRANSLITERATE: &str = "__t__";
const MODE_COMPLETE: &str = "__c__";
const CONTEXT_SEPARATOR: &str = "|";
pub const CONTEXT_WORDS: usize = 3;
/// The longest word completion mode will write, in characters: past the longest common words.
const COMPLETE_MAX_LEN: usize = 24;

#[derive(Clone, Copy)]
struct Attn {
    /// Fused [q|k|v] projection with the attention scaling already folded into the q columns.
    /// Empty for cross attention, which projects q separately and scales after the bias.
    wqkv: Arr,
    bqkv: Arr,
    wq: Arr,
    bq: Arr,
    wk: Arr,
    bk: Arr,
    wv: Arr,
    bv: Arr,
    wo: Arr,
    bo: Arr,
    ln_g: Arr,
    ln_b: Arr,
}

#[derive(Clone, Copy)]
struct Ffn {
    w1: Arr,
    b1: Arr,
    w2: Arr,
    b2: Arr,
    ln_g: Arr,
    ln_b: Arr,
}

#[derive(Clone, Copy)]
struct EncLayer {
    attn: Attn,
    ffn: Ffn,
}

#[derive(Clone, Copy)]
struct DecLayer {
    self_attn: Attn,
    cross: Attn,
    ffn: Ffn,
}

/// Encoder keys and values, already projected for every decoder layer's cross attention.
pub struct EncKv {
    /// Per decoder layer, `(src_len, dim)` row-major with heads interleaved.
    k: Vec<Vec<f32>>,
    v: Vec<Vec<f32>>,
    src_len: usize,
    /// The encoder's output at the context separator, for the word head; empty when the model has
    /// no word head or the source no separator.
    sep_state: Vec<f32>,
}

/// One beam's incremental self-attention cache, growing by `dim` per decoded step.
#[derive(Clone, Default)]
struct LayerCache {
    k: Vec<f32>,
    v: Vec<f32>,
}

pub struct Xlit {
    w: Weights,
    meta: Meta,
    src_vocab: Vec<String>,
    tgt_vocab: Vec<String>,
    src_index: HashMap<String, usize>,
    tgt_index: HashMap<String, usize>,
    pub pad: usize,
    pub eos: usize,
    pub unk: usize,
    src_eos: usize,
    src_unk: usize,
    enc_embed: Arr,
    dec_embed: Arr,
    out_proj_t: Arr,
    pos: Arr,
    enc_ln_emb: (Arr, Arr),
    dec_ln_emb: (Arr, Arr),
    enc_ln: (Arr, Arr),
    dec_ln: (Arr, Arr),
    banned: Arr,
    enc_layers: Vec<EncLayer>,
    dec_layers: Vec<DecLayer>,
    gelu: bool,
    /// The Likhi model's word head, `[dim, words]` and `[words]`, and its words (canonical): the
    /// next-word and completion distribution. Empty for a model without one, or one whose head is
    /// in `word_head.lkq` instead (`head_q8`).
    word_head: (Arr, Arr),
    head_q8: Option<HeadQ8>,
    /// A model that reads its context words whole (`Meta::word_context`): one vector a word,
    /// `[words, dim]`, which is also its word head, tied. Empty when it is in `head_q8` instead.
    word_embed: Arr,
    words: Vec<String>,
    word_index: HashMap<String, usize>,
}

/// The word head in 8 bits, `word_head.lkq`: a row of `i8` and a scale for each word, as the
/// next-word LSTM keeps its embedding -- a quarter of the size of the floats, scored with the
/// same integer dot product. Layout, little-endian:
///
/// ```text
/// magic b"LKQ1", version u32 = 1, words u32, dim u32, temperature f32 (0: none), (pad to 64)
/// scale f32[words], bias f32[words], q i8[words * dim]
/// ```
///
/// The temperature divides the scores: a model can be surer of its first guess than it has
/// reason to be, and the Tab guess is shown by its share, so the share must mean what it says.
/// Fitted on held-out text (`set_word_head_temperature`).
pub struct HeadQ8 {
    map: memmap2::Mmap,
    words: usize,
    dim: usize,
    temperature: f32,
}

const HEAD_MAGIC: &[u8; 4] = b"LKQ1";
const HEAD_DATA: usize = 64;

impl HeadQ8 {
    fn open(path: &Path, words: usize, dim: usize) -> Result<HeadQ8, XlitError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: read-only mapping, never written through, as in weights.rs.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        let bad = |m: &str| XlitError::Vocab(format!("{}: {m}", path.display()));
        if map.len() < 16 || &map[..4] != HEAD_MAGIC {
            return Err(bad("not an LKQ1 file"));
        }
        let u32_at = |i: usize| u32::from_le_bytes([map[i], map[i + 1], map[i + 2], map[i + 3]]) as usize;
        if u32_at(4) != 1 {
            return Err(bad("unknown version"));
        }
        if u32_at(8) != words || u32_at(12) != dim {
            return Err(bad("its shape does not match the model and word_vocabulary.json"));
        }
        if map.len() != HEAD_DATA + 8 * words + words * dim {
            return Err(bad("wrong length"));
        }
        let t = f32::from_le_bytes([map[16], map[17], map[18], map[19]]);
        let temperature = if t.is_finite() && t > 0.0 { t } else { 1.0 };
        Ok(HeadQ8 { map, words, dim, temperature })
    }

    fn f32s(&self, at: usize) -> &[f32] {
        // SAFETY: the length was checked in `open`, the mapping is page-aligned and HEAD_DATA is a
        // multiple of 4, and f32 has no invalid bit patterns.
        unsafe { std::slice::from_raw_parts(self.map.as_ptr().add(at) as *const f32, self.words) }
    }

    /// One word's vector, back in floats: a context word read whole.
    fn row(&self, j: usize) -> Vec<f32> {
        let scale = self.f32s(HEAD_DATA)[j];
        let at = HEAD_DATA + 8 * self.words + j * self.dim;
        self.map[at..at + self.dim].iter().map(|b| f32::from(*b as i8) * scale).collect()
    }

    fn scores(&self, x: &[f32]) -> Vec<f32> {
        let scale = self.f32s(HEAD_DATA);
        let bias = self.f32s(HEAD_DATA + 4 * self.words);
        let q_at = HEAD_DATA + 8 * self.words;
        // SAFETY: as above; any byte is a valid i8.
        let q = unsafe { std::slice::from_raw_parts(self.map.as_ptr().add(q_at) as *const i8, self.words * self.dim) };
        let (xq, xs) = crate::nextlm::quantize_i16(x);
        let t = self.temperature;
        (0..self.words)
            .map(|j| (crate::nextlm::dot_i8_i16(&q[j * self.dim..(j + 1) * self.dim], &xq) as f32 * xs * scale[j] + bias[j]) / t)
            .collect()
    }
}

/// Set the word head's temperature in `dir/word_head.lkq` (see `HeadQ8`); 1 removes it.
pub fn set_word_head_temperature(dir: &Path, temperature: f32) -> Result<(), XlitError> {
    use std::io::{Seek, SeekFrom, Write};
    let path = dir.join("word_head.lkq");
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let mut magic = [0u8; 4];
    std::io::Read::read_exact(&mut f, &mut magic)?;
    if &magic != HEAD_MAGIC {
        return Err(XlitError::Vocab(format!("{}: not an LKQ1 file", path.display())));
    }
    f.seek(SeekFrom::Start(16))?;
    f.write_all(&temperature.to_le_bytes())?;
    Ok(())
}

/// Turn an exported model's word head into `word_head.lkq` and drop the floats from `model.lkw`,
/// in place: 20,000 words of a 256-wide model are 20 MB as floats, 5 MB like this. Returns the
/// size of model.lkw before and after.
pub fn quantize_word_head(dir: &Path) -> Result<(u64, u64), XlitError> {
    let lkw = dir.join("model.lkw");
    let before = std::fs::metadata(&lkw)?.len();
    // A word-aware model's one table, a word a row; otherwise the head alone, a word a column.
    let tied = Weights::open(&lkw)?.names().any(|n| n == "word_embed");
    let (q, dim, words) = {
        let w = Weights::open(&lkw)?;
        let (hw, hb) = (w.arr(if tied { "word_embed" } else { "word_head_w" })?, w.arr("word_head_b")?);
        let (dim, words) = if tied { (hw.cols, hw.rows) } else { (hw.rows, hw.cols) };
        let (m, b) = (w.get(hw), w.get(hb));
        let at = |j: usize, i: usize| if tied { m[j * dim + i] } else { m[i * words + j] };
        let mut scale = vec![0.0f32; words];
        let mut q = vec![0i8; words * dim];
        for j in 0..words {
            let max = (0..dim).map(|i| at(j, i).abs()).fold(0.0f32, f32::max);
            scale[j] = if max > 0.0 { max / 127.0 } else { 1.0 };
            for i in 0..dim {
                q[j * dim + i] = (at(j, i) / scale[j]).round().clamp(-127.0, 127.0) as i8;
            }
        }
        let mut out = Vec::with_capacity(HEAD_DATA + 8 * words + words * dim);
        out.extend_from_slice(HEAD_MAGIC);
        for v in [1u32, words as u32, dim as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.resize(HEAD_DATA, 0);
        for v in scale.iter().chain(b) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend(q.iter().map(|v| *v as u8));
        (out, dim, words)
    };
    std::fs::write(dir.join("word_head.lkq"), q)?;
    crate::weights::rewrite_without(&lkw, &["word_head_w", "word_head_b", "word_embed"])?;
    let after = std::fs::metadata(&lkw)?.len();
    debug_assert!(dim > 0 && words > 0);
    Ok((before, after))
}

#[derive(Debug)]
pub enum XlitError {
    Weights(WeightError),
    Io(std::io::Error),
    Vocab(String),
}

impl std::fmt::Display for XlitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XlitError::Weights(e) => write!(f, "{e}"),
            XlitError::Io(e) => write!(f, "{e}"),
            XlitError::Vocab(m) => write!(f, "vocabulary: {m}"),
        }
    }
}

impl std::error::Error for XlitError {}

impl From<WeightError> for XlitError {
    fn from(e: WeightError) -> Self {
        XlitError::Weights(e)
    }
}

impl From<std::io::Error> for XlitError {
    fn from(e: std::io::Error) -> Self {
        XlitError::Io(e)
    }
}

fn load_vocab(path: &Path) -> Result<Vec<String>, XlitError> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| XlitError::Vocab(format!("{}: {e}", path.display())))
}

impl Xlit {
    pub fn open(dir: &Path) -> Result<Xlit, XlitError> {
        let w = Weights::open(&dir.join("model.lkw"))?;
        let meta = w.meta.clone();
        let src_vocab = load_vocab(&dir.join("source_vocabulary.json"))?;
        let tgt_vocab = load_vocab(&dir.join("target_vocabulary.json"))?;
        let src_index: HashMap<String, usize> =
            src_vocab.iter().enumerate().map(|(i, t)| (t.clone(), i)).collect();
        let tgt_index: HashMap<String, usize> =
            tgt_vocab.iter().enumerate().map(|(i, t)| (t.clone(), i)).collect();

        let words =
            if dir.join("word_vocabulary.json").exists() { load_vocab(&dir.join("word_vocabulary.json"))? } else { Vec::new() };
        let need = |m: &HashMap<String, usize>, t: &str| -> Result<usize, XlitError> {
            m.get(t).copied().ok_or_else(|| XlitError::Vocab(format!("no {t} token")))
        };

        let mut enc_layers = Vec::with_capacity(meta.encoder_layers);
        for i in 0..meta.encoder_layers {
            enc_layers.push(EncLayer {
                attn: Self::attn(&w, &format!("enc.{i}.attn"), true)?,
                ffn: Self::ffn(&w, &format!("enc.{i}.ffn"))?,
            });
        }
        let mut dec_layers = Vec::with_capacity(meta.decoder_layers);
        for i in 0..meta.decoder_layers {
            dec_layers.push(DecLayer {
                self_attn: Self::attn(&w, &format!("dec.{i}.self"), true)?,
                cross: Self::attn(&w, &format!("dec.{i}.cross"), false)?,
                ffn: Self::ffn(&w, &format!("dec.{i}.ffn"))?,
            });
        }

        Ok(Xlit {
            pad: need(&tgt_index, "<pad>")?,
            eos: need(&tgt_index, "</s>")?,
            unk: need(&tgt_index, "<unk>")?,
            src_eos: need(&src_index, "</s>")?,
            src_unk: need(&src_index, "<unk>")?,
            enc_embed: w.arr("enc_embed")?,
            dec_embed: w.arr("dec_embed")?,
            out_proj_t: w.arr("out_proj_t")?,
            pos: w.arr("pos")?,
            enc_ln_emb: (w.arr_opt("enc_ln_emb_g"), w.arr_opt("enc_ln_emb_b")),
            dec_ln_emb: (w.arr_opt("dec_ln_emb_g"), w.arr_opt("dec_ln_emb_b")),
            enc_ln: (w.arr_opt("enc_ln_g"), w.arr_opt("enc_ln_b")),
            dec_ln: (w.arr_opt("dec_ln_g"), w.arr_opt("dec_ln_b")),
            banned: w.arr("banned")?,
            gelu: meta.activation.starts_with("gelu"),
            word_head: (w.arr_opt("word_head_w"), w.arr_opt("word_head_b")),
            head_q8: match (dir.join("word_head.lkq").exists(), words.len()) {
                (true, n) if n > 0 => Some(HeadQ8::open(&dir.join("word_head.lkq"), n, meta.dim)?),
                _ => None,
            },
            word_embed: w.arr_opt("word_embed"),
            word_index: words.iter().enumerate().map(|(i, w)| (w.clone(), i)).collect(),
            words,
            enc_layers,
            dec_layers,
            src_vocab,
            tgt_vocab,
            src_index,
            tgt_index,
            meta,
            w,
        })
    }

    fn attn(w: &Weights, tag: &str, fused: bool) -> Result<Attn, WeightError> {
        Ok(if fused {
            Attn {
                wqkv: w.arr(&format!("{tag}.wqkv"))?,
                bqkv: w.arr(&format!("{tag}.bqkv"))?,
                wq: Arr::EMPTY,
                bq: Arr::EMPTY,
                wk: Arr::EMPTY,
                bk: Arr::EMPTY,
                wv: Arr::EMPTY,
                bv: Arr::EMPTY,
                wo: w.arr(&format!("{tag}.wo"))?,
                bo: w.arr(&format!("{tag}.bo"))?,
                ln_g: w.arr(&format!("{tag}.ln_g"))?,
                ln_b: w.arr(&format!("{tag}.ln_b"))?,
            }
        } else {
            Attn {
                wqkv: Arr::EMPTY,
                bqkv: Arr::EMPTY,
                wq: w.arr(&format!("{tag}.wq"))?,
                bq: w.arr(&format!("{tag}.bq"))?,
                wk: w.arr(&format!("{tag}.wk"))?,
                bk: w.arr(&format!("{tag}.bk"))?,
                wv: w.arr(&format!("{tag}.wv"))?,
                bv: w.arr(&format!("{tag}.bv"))?,
                wo: w.arr(&format!("{tag}.wo"))?,
                bo: w.arr(&format!("{tag}.bo"))?,
                ln_g: w.arr(&format!("{tag}.ln_g"))?,
                ln_b: w.arr(&format!("{tag}.ln_b"))?,
            }
        })
    }

    fn ffn(w: &Weights, tag: &str) -> Result<Ffn, WeightError> {
        Ok(Ffn {
            w1: w.arr(&format!("{tag}.w1"))?,
            b1: w.arr(&format!("{tag}.b1"))?,
            w2: w.arr(&format!("{tag}.w2"))?,
            b2: w.arr(&format!("{tag}.b2"))?,
            ln_g: w.arr(&format!("{tag}.ln_g"))?,
            ln_b: w.arr(&format!("{tag}.ln_b"))?,
        })
    }

    pub fn vocab_size(&self) -> usize {
        self.tgt_vocab.len()
    }

    /// Source alphabet size. Only diagnostics use it, but a model whose source vocabulary does not
    /// match the one the weights were exported with produces silent nonsense, so it is reportable.
    pub fn source_vocab_size(&self) -> usize {
        self.src_vocab.len()
    }

    fn dim(&self) -> usize {
        self.meta.dim
    }

    fn activate(&self, x: &mut [f32]) {
        if self.gelu {
            gelu_inplace(x)
        } else {
            relu_inplace(x)
        }
    }

    /// `[__lang__] + characters + [</s>]`, unknown characters mapped to `<unk>`.
    pub fn encode_source(&self, roman: &str, lang: &str) -> Vec<usize> {
        let mut ids = Vec::with_capacity(roman.chars().count() + 2);
        ids.push(
            self.src_index
                .get(&format!("__{lang}__"))
                .copied()
                .unwrap_or(self.src_unk),
        );
        for ch in roman.chars() {
            let mut buf = [0u8; 4];
            let s: &str = ch.encode_utf8(&mut buf);
            ids.push(self.src_index.get(s).copied().unwrap_or(self.src_unk));
        }
        ids.push(self.src_eos);
        ids
    }

    /// Whether this model reads the sentence before the word. The Likhi model does; IndicXlit, which
    /// knows only the letters, does not. Told apart by the source vocabulary: only the Likhi model
    /// has the mode tokens.
    pub fn reads_context(&self) -> bool {
        self.src_index.contains_key(MODE_TRANSLITERATE)
    }

    /// The Likhi model's source, exactly as it was trained on: the mode --
    /// `complete` asks for the word these letters begin, otherwise the letters as typed -- then up
    /// to `CONTEXT_WORDS` words before, canonical Bangla separated by spaces, the separator, the
    /// letters, and `</s>`.
    pub fn encode_source_ctx(&self, complete: bool, context: &[String], roman: &str) -> Vec<usize> {
        let id = |s: &str| self.src_index.get(s).copied().unwrap_or(self.src_unk);
        let mut ids = vec![id(if complete { MODE_COMPLETE } else { MODE_TRANSLITERATE })];
        let start = context.len().saturating_sub(CONTEXT_WORDS);
        // The context words whole, right after the mode: ids past the character vocabulary, a word
        // the table lacks as its last entry, `<unk>`.
        if self.meta.word_context && !self.words.is_empty() {
            let unk = self.words.len() - 1;
            for w in &context[start..] {
                let wid = self.word_index.get(&crate::textnorm::canonical(w)).copied().unwrap_or(unk);
                ids.push(self.src_vocab.len() + wid);
            }
        }
        let mut buf = [0u8; 4];
        for ch in context[start..].join(" ").chars() {
            ids.push(id(ch.encode_utf8(&mut buf)));
        }
        ids.push(id(CONTEXT_SEPARATOR));
        for ch in roman.chars() {
            ids.push(id(ch.encode_utf8(&mut buf)));
        }
        ids.push(self.src_eos);
        ids
    }

    /// The word head's words, canonical, in the order `word_scores` scores them. Empty without one.
    pub fn words(&self) -> &[String] {
        &self.words
    }

    fn has_word_head(&self) -> bool {
        self.head_q8.is_some() || !self.word_head.0.is_empty() || !self.word_embed.is_empty()
    }

    /// The word head's raw scores for every word in `words`, from an encoding of the completion
    /// mode: higher is likelier, and a softmax over them is the next-word distribution. None for a
    /// model without a word head, or an encoding without a separator.
    pub fn word_scores(&self, enc: &EncKv) -> Option<Vec<f32>> {
        if !self.has_word_head() || enc.sep_state.is_empty() {
            return None;
        }
        if let Some(h) = &self.head_q8 {
            return Some(h.scores(&enc.sep_state));
        }
        if !self.word_embed.is_empty() {
            // Tied: each word's score is its own vector against the state, plus its bias.
            let bias = self.w.get(self.word_head.1);
            return Some(
                (0..self.word_embed.rows)
                    .map(|j| self.w.row(self.word_embed, j).iter().zip(&enc.sep_state).map(|(a, b)| a * b).sum::<f32>() + bias[j])
                    .collect(),
            );
        }
        let n = self.word_head.0.cols;
        let mut out = vec![0.0f32; n];
        matmul_bias(&enc.sep_state, 1, self.dim(), self.w.get(self.word_head.0), n, Some(self.w.get(self.word_head.1)), &mut out);
        Some(out)
    }

    /// `encode`, for the Likhi model: the encoder's view of the sentence so far and the letters.
    pub fn encode_ctx(&self, complete: bool, context: &[String], roman: &str) -> EncKv {
        self.encode_ids(&self.encode_source_ctx(complete, context, roman))
    }

    /// Optional layer norm: applied when the checkpoint carries one, skipped when it does not.
    fn maybe_ln(&self, ln: (Arr, Arr), x: &mut Vec<f32>, rows: usize, scratch: &mut Vec<f32>) {
        if ln.0.is_empty() {
            return;
        }
        scratch.clear();
        scratch.resize(x.len(), 0.0);
        layer_norm(x, rows, self.dim(), self.w.get(ln.0), self.w.get(ln.1), scratch);
        std::mem::swap(x, scratch);
    }

    fn ffn_block(&self, f: &Ffn, x: &mut [f32], rows: usize) {
        let dim = self.dim();
        let hidden = f.w1.cols;
        let mut h = vec![0.0f32; rows * dim];
        if self.meta.pre_norm {
            layer_norm(x, rows, dim, self.w.get(f.ln_g), self.w.get(f.ln_b), &mut h);
        } else {
            h.copy_from_slice(x);
        }
        let mut inner = vec![0.0f32; rows * hidden];
        matmul_bias(&h, rows, dim, self.w.get(f.w1), hidden, Some(self.w.get(f.b1)), &mut inner);
        self.activate(&mut inner);
        let mut back = vec![0.0f32; rows * dim];
        matmul_bias(&inner, rows, hidden, self.w.get(f.w2), dim, Some(self.w.get(f.b2)), &mut back);
        add_inplace(x, &back);
        if !self.meta.pre_norm {
            let mut normed = vec![0.0f32; rows * dim];
            layer_norm(x, rows, dim, self.w.get(f.ln_g), self.w.get(f.ln_b), &mut normed);
            x.copy_from_slice(&normed);
        }
    }

    /// Run the encoder and pre-project its output for every decoder layer's cross attention.
    pub fn encode(&self, roman: &str, lang: &str) -> EncKv {
        let ids = self.encode_source(roman, lang);
        self.encode_ids(&ids)
    }

    fn encode_ids(&self, src_ids: &[usize]) -> EncKv {
        let dim = self.dim();
        let t = src_ids.len();
        let scale = self.meta.embed_scale;

        let mut x = vec![0.0f32; t * dim];
        let n_src = self.src_vocab.len();
        for (i, &id) in src_ids.iter().enumerate() {
            let emb: std::borrow::Cow<[f32]> = if id < n_src {
                self.w.row(self.enc_embed, id).into()
            } else {
                // A context word, whole (encode_source_ctx).
                match &self.head_q8 {
                    Some(h) => h.row(id - n_src).into(),
                    None => self.w.row(self.word_embed, id - n_src).into(),
                }
            };
            let pos = self.w.row(self.pos, self.meta.pos_offset + i);
            let row = &mut x[i * dim..(i + 1) * dim];
            for j in 0..dim {
                row[j] = scale * emb[j] + pos[j];
            }
        }
        let mut scratch = Vec::new();
        self.maybe_ln(self.enc_ln_emb, &mut x, t, &mut scratch);

        let mut qkv = vec![0.0f32; t * 3 * dim];
        let mut q = vec![0.0f32; t * dim];
        let mut k = vec![0.0f32; t * dim];
        let mut v = vec![0.0f32; t * dim];
        let mut attn_out = vec![0.0f32; t * dim];
        let mut projected = vec![0.0f32; t * dim];
        let mut normed = vec![0.0f32; t * dim];
        let mut sc = Vec::new();

        for layer in &self.enc_layers {
            let a = &layer.attn;
            let h: &[f32] = if self.meta.pre_norm {
                layer_norm(&x, t, dim, self.w.get(a.ln_g), self.w.get(a.ln_b), &mut normed);
                &normed
            } else {
                &x
            };
            matmul_bias(h, t, dim, self.w.get(a.wqkv), 3 * dim, Some(self.w.get(a.bqkv)), &mut qkv);
            for i in 0..t {
                let src = &qkv[i * 3 * dim..(i + 1) * 3 * dim];
                q[i * dim..(i + 1) * dim].copy_from_slice(&src[..dim]);
                k[i * dim..(i + 1) * dim].copy_from_slice(&src[dim..2 * dim]);
                v[i * dim..(i + 1) * dim].copy_from_slice(&src[2 * dim..]);
            }
            attention(&q, &k, &v, t, t, dim, self.meta.heads, false, &mut attn_out, &mut sc);
            matmul_bias(&attn_out, t, dim, self.w.get(a.wo), dim, Some(self.w.get(a.bo)), &mut projected);
            add_inplace(&mut x, &projected);
            if !self.meta.pre_norm {
                layer_norm(&x, t, dim, self.w.get(a.ln_g), self.w.get(a.ln_b), &mut normed);
                x.copy_from_slice(&normed);
            }
            self.ffn_block(&layer.ffn, &mut x, t);
        }
        self.maybe_ln(self.enc_ln, &mut x, t, &mut scratch);
        let sep_state = match (self.has_word_head(), self.src_index.get(CONTEXT_SEPARATOR)) {
            (true, Some(sep)) => match src_ids.iter().position(|id| id == sep) {
                Some(at) => x[at * dim..(at + 1) * dim].to_vec(),
                None => Vec::new(),
            },
            _ => Vec::new(),
        };

        let mut ek = Vec::with_capacity(self.dec_layers.len());
        let mut ev = Vec::with_capacity(self.dec_layers.len());
        for layer in &self.dec_layers {
            let c = &layer.cross;
            let mut kk = vec![0.0f32; t * dim];
            let mut vv = vec![0.0f32; t * dim];
            matmul_bias(&x, t, dim, self.w.get(c.wk), dim, Some(self.w.get(c.bk)), &mut kk);
            matmul_bias(&x, t, dim, self.w.get(c.wv), dim, Some(self.w.get(c.bv)), &mut vv);
            ek.push(kk);
            ev.push(vv);
        }
        EncKv { k: ek, v: ev, src_len: t, sep_state }
    }

    /// One decoder step for every live hypothesis at once. Appends to each cache and returns
    /// `(B, V)` log-probabilities.
    ///
    /// Batched across the beam for the same reason `decode_full_batch` is batched across
    /// candidates: the matrix products then touch each weight once per step rather than once per
    /// hypothesis. Unlike candidate scoring there is no padding to trade against it -- every
    /// hypothesis decodes exactly one token per step -- so this is free.
    fn decode_step_batch(
        &self,
        tokens: &[usize],
        step: usize,
        caches: &mut [Vec<LayerCache>],
        enc: &EncKv,
        logits: &mut Vec<f32>,
    ) {
        let dim = self.dim();
        let b = tokens.len();
        let scale = self.meta.embed_scale;

        let mut x = vec![0.0f32; b * dim];
        for (bi, &tok) in tokens.iter().enumerate() {
            let emb = self.w.row(self.dec_embed, tok);
            let pos = self.w.row(self.pos, self.meta.pos_offset + step);
            let row = &mut x[bi * dim..(bi + 1) * dim];
            for j in 0..dim {
                row[j] = scale * emb[j] + pos[j];
            }
        }
        let mut scratch = Vec::new();
        self.maybe_ln(self.dec_ln_emb, &mut x, b, &mut scratch);

        let mut qkv = vec![0.0f32; b * 3 * dim];
        let mut normed = vec![0.0f32; b * dim];
        let mut attn_out = vec![0.0f32; b * dim];
        let mut projected = vec![0.0f32; b * dim];
        let mut cq = vec![0.0f32; b * dim];
        let mut sc = Vec::new();

        for (i, layer) in self.dec_layers.iter().enumerate() {
            let a = &layer.self_attn;
            let h: &[f32] = if self.meta.pre_norm {
                layer_norm(&x, b, dim, self.w.get(a.ln_g), self.w.get(a.ln_b), &mut normed);
                &normed
            } else {
                &x
            };
            matmul_bias(h, b, dim, self.w.get(a.wqkv), 3 * dim, Some(self.w.get(a.bqkv)), &mut qkv);
            for bi in 0..b {
                let src = &qkv[bi * 3 * dim..(bi + 1) * 3 * dim];
                caches[bi][i].k.extend_from_slice(&src[dim..2 * dim]);
                caches[bi][i].v.extend_from_slice(&src[2 * dim..]);
                let tk = caches[bi][i].k.len() / dim;
                attention(
                    &src[..dim],
                    &caches[bi][i].k,
                    &caches[bi][i].v,
                    1,
                    tk,
                    dim,
                    self.meta.heads,
                    false,
                    &mut attn_out[bi * dim..(bi + 1) * dim],
                    &mut sc,
                );
            }
            matmul_bias(&attn_out, b, dim, self.w.get(a.wo), dim, Some(self.w.get(a.bo)), &mut projected);
            add_inplace(&mut x, &projected);
            if !self.meta.pre_norm {
                layer_norm(&x, b, dim, self.w.get(a.ln_g), self.w.get(a.ln_b), &mut normed);
                x.copy_from_slice(&normed);
            }

            let c = &layer.cross;
            let h: &[f32] = if self.meta.pre_norm {
                layer_norm(&x, b, dim, self.w.get(c.ln_g), self.w.get(c.ln_b), &mut normed);
                &normed
            } else {
                &x
            };
            matmul_bias(h, b, dim, self.w.get(c.wq), dim, Some(self.w.get(c.bq)), &mut cq);
            // Scaling applied after the bias, exactly as `(h @ c.wq + c.bq) * self.scaling` does.
            for value in cq.iter_mut() {
                *value *= self.meta.scaling;
            }
            for bi in 0..b {
                attention(
                    &cq[bi * dim..(bi + 1) * dim],
                    &enc.k[i],
                    &enc.v[i],
                    1,
                    enc.src_len,
                    dim,
                    self.meta.heads,
                    false,
                    &mut attn_out[bi * dim..(bi + 1) * dim],
                    &mut sc,
                );
            }
            matmul_bias(&attn_out, b, dim, self.w.get(c.wo), dim, Some(self.w.get(c.bo)), &mut projected);
            add_inplace(&mut x, &projected);
            if !self.meta.pre_norm {
                layer_norm(&x, b, dim, self.w.get(c.ln_g), self.w.get(c.ln_b), &mut normed);
                x.copy_from_slice(&normed);
            }

            self.ffn_block(&layer.ffn, &mut x, b);
        }
        self.maybe_ln(self.dec_ln, &mut x, b, &mut scratch);

        let v = self.vocab_size();
        logits.clear();
        logits.resize(b * v, 0.0);
        matmul_bias(&x, b, dim, self.w.get(self.out_proj_t), v, None, logits);
        for bi in 0..b {
            log_softmax_inplace(&mut logits[bi * v..(bi + 1) * v]);
        }
    }

    /// Teacher-forced pass over a batch of sequences: `(B*T, V)` log-probabilities, causally masked.
    ///
    /// Batched, and that is a performance decision rather than a stylistic one. Scoring sixteen
    /// candidates one at a time means streaming the 1 MB feed-forward weights through cache sixteen
    /// times for sixteen tiny products; batching turns those into one tall product that touches each
    /// weight once. Measured at roughly half the total latency of the full path.
    ///
    /// Rows are laid out item-major: item `b` occupies rows `b*T .. (b+1)*T`. Every matrix product
    /// therefore runs over all `B*T` rows at once, while attention -- which must not mix items --
    /// loops over `b` and works on that item's slice.
    fn decode_full_batch(&self, rows: &[Vec<usize>], t: usize, enc: &EncKv) -> Vec<f32> {
        let dim = self.dim();
        let b = rows.len();
        let scale = self.meta.embed_scale;

        let mut x = vec![0.0f32; b * t * dim];
        for (bi, seq) in rows.iter().enumerate() {
            for i in 0..t {
                let id = seq[i];
                let emb = self.w.row(self.dec_embed, id);
                let pos = self.w.row(self.pos, self.meta.pos_offset + i);
                let row = &mut x[(bi * t + i) * dim..(bi * t + i + 1) * dim];
                for j in 0..dim {
                    row[j] = scale * emb[j] + pos[j];
                }
            }
        }
        self.decode_full_body(x, b, t, enc)
    }

    fn decode_full_body(&self, mut x: Vec<f32>, b: usize, t: usize, enc: &EncKv) -> Vec<f32> {
        let dim = self.dim();
        let n = b * t;
        let mut scratch = Vec::new();
        self.maybe_ln(self.dec_ln_emb, &mut x, n, &mut scratch);

        let mut qkv = vec![0.0f32; n * 3 * dim];
        let mut q = vec![0.0f32; n * dim];
        let mut k = vec![0.0f32; n * dim];
        let mut v = vec![0.0f32; n * dim];
        let mut normed = vec![0.0f32; n * dim];
        let mut attn_out = vec![0.0f32; n * dim];
        let mut projected = vec![0.0f32; n * dim];
        let mut cq = vec![0.0f32; n * dim];
        let mut sc = Vec::new();
        let item = t * dim;

        for (i, layer) in self.dec_layers.iter().enumerate() {
            let a = &layer.self_attn;
            let h: &[f32] = if self.meta.pre_norm {
                layer_norm(&x, n, dim, self.w.get(a.ln_g), self.w.get(a.ln_b), &mut normed);
                &normed
            } else {
                &x
            };
            matmul_bias(h, n, dim, self.w.get(a.wqkv), 3 * dim, Some(self.w.get(a.bqkv)), &mut qkv);
            for r in 0..n {
                let src = &qkv[r * 3 * dim..(r + 1) * 3 * dim];
                q[r * dim..(r + 1) * dim].copy_from_slice(&src[..dim]);
                k[r * dim..(r + 1) * dim].copy_from_slice(&src[dim..2 * dim]);
                v[r * dim..(r + 1) * dim].copy_from_slice(&src[2 * dim..]);
            }
            // Attention is per item: masking and the softmax must never mix two candidates.
            for bi in 0..b {
                let s = bi * item;
                attention(
                    &q[s..s + item],
                    &k[s..s + item],
                    &v[s..s + item],
                    t,
                    t,
                    dim,
                    self.meta.heads,
                    true,
                    &mut attn_out[s..s + item],
                    &mut sc,
                );
            }
            matmul_bias(&attn_out, n, dim, self.w.get(a.wo), dim, Some(self.w.get(a.bo)), &mut projected);
            add_inplace(&mut x, &projected);
            if !self.meta.pre_norm {
                layer_norm(&x, n, dim, self.w.get(a.ln_g), self.w.get(a.ln_b), &mut normed);
                x.copy_from_slice(&normed);
            }

            let c = &layer.cross;
            let h: &[f32] = if self.meta.pre_norm {
                layer_norm(&x, n, dim, self.w.get(c.ln_g), self.w.get(c.ln_b), &mut normed);
                &normed
            } else {
                &x
            };
            matmul_bias(h, n, dim, self.w.get(c.wq), dim, Some(self.w.get(c.bq)), &mut cq);
            for value in cq.iter_mut() {
                *value *= self.meta.scaling;
            }
            for bi in 0..b {
                let s = bi * item;
                attention(
                    &cq[s..s + item],
                    &enc.k[i],
                    &enc.v[i],
                    t,
                    enc.src_len,
                    dim,
                    self.meta.heads,
                    false,
                    &mut attn_out[s..s + item],
                    &mut sc,
                );
            }
            matmul_bias(&attn_out, n, dim, self.w.get(c.wo), dim, Some(self.w.get(c.bo)), &mut projected);
            add_inplace(&mut x, &projected);
            if !self.meta.pre_norm {
                layer_norm(&x, n, dim, self.w.get(c.ln_g), self.w.get(c.ln_b), &mut normed);
                x.copy_from_slice(&normed);
            }

            self.ffn_block(&layer.ffn, &mut x, n);
        }
        self.maybe_ln(self.dec_ln, &mut x, n, &mut scratch);

        let vsize = self.vocab_size();
        let mut logits = vec![0.0f32; n * vsize];
        matmul_bias(&x, n, dim, self.w.get(self.out_proj_t), vsize, None, &mut logits);
        for r in 0..n {
            log_softmax_inplace(&mut logits[r * vsize..(r + 1) * vsize]);
        }
        logits
    }

    /// `log P(word | roman)` summed over characters including `</s>`.
    ///
    /// A word containing a character outside the target vocabulary scores -inf, matching the
    /// Python's `ok` mask.
    ///
    /// All scorable words go through the decoder in one batch, padded to the longest. Padding sits
    /// at the end of each row and the causal mask only ever looks backwards, so a real position can
    /// never see a pad; the pad positions produce values that are simply not summed. This is the
    /// same construction the Python uses, for the same reason.
    pub fn score_candidates(&self, words: &[String], enc: &EncKv) -> Vec<f32> {
        let vsize = self.vocab_size();
        let mut out = vec![f32::NEG_INFINITY; words.len()];

        // Target ids per scorable word, with the slot it came from.
        let mut targets: Vec<(usize, Vec<usize>)> = Vec::with_capacity(words.len());
        for (slot, word) in words.iter().enumerate() {
            let mut ids = Vec::with_capacity(word.len() + 1);
            let mut ok = true;
            for ch in word.chars() {
                let mut buf = [0u8; 4];
                let s: &str = ch.encode_utf8(&mut buf);
                match self.tgt_index.get(s) {
                    Some(&i) => ids.push(i),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                ids.push(self.eos);
                targets.push((slot, ids));
            }
        }
        if targets.is_empty() {
            return out;
        }

        // Grouped by length, one batch per distinct length.
        //
        // Batching everything together to the longest word was measurably *worse* than scoring one
        // at a time: candidate lengths vary by a factor of three, so padding to the maximum added
        // about three quarters again as many rows, and that cost more than the cache reuse saved.
        // Grouping gets both -- every row in a batch is real work, and each weight matrix is still
        // streamed once per group instead of once per word.
        let mut by_len: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, (_slot, ids)) in targets.iter().enumerate() {
            by_len.entry(ids.len()).or_default().push(i);
        }

        for (t, members) in by_len {
            // Teacher forcing: input is </s> then the word, target is the word then </s>.
            let inputs: Vec<Vec<usize>> = members
                .iter()
                .map(|&i| {
                    let ids = &targets[i].1;
                    let mut row = Vec::with_capacity(t);
                    row.push(self.eos);
                    row.extend_from_slice(&ids[..ids.len() - 1]);
                    row
                })
                .collect();
            let lp = self.decode_full_batch(&inputs, t, enc);
            for (bi, &i) in members.iter().enumerate() {
                let (slot, ids) = &targets[i];
                let mut total = 0.0f32;
                for (step, &target) in ids.iter().enumerate() {
                    total += lp[(bi * t + step) * vsize + target];
                }
                out[*slot] = total;
            }
        }
        out
    }

    /// Beam search. Returns `(word, length-normalized log probability)`, best first.
    ///
    /// Reproduces fairseq's rules as the Python does, including the two that are easy to miss and
    /// change the output when got wrong:
    ///
    /// * at step 0 the end-of-sequence token is forbidden, which is `min_len = 1`;
    /// * an EOS is only *finalized* when it ranks within the top `beam` of the 2*beam candidates
    ///   examined. Lower-ranked ones are discarded rather than recorded, otherwise weak short words
    ///   fill the finished list and stop the search before longer correct words complete.
    ///
    /// There is deliberately no other early stop. A live hypothesis' length-normalized score can
    /// still improve as it grows, so comparing it against finished ones is not a valid bound -- the
    /// Python notes that doing so dropped the model's own best answer for "khacche" at beam 4.
    pub fn beam_search(&self, roman: &str, beam: usize, nbest: usize, enc: &EncKv) -> Vec<(String, f32)> {
        if roman.is_empty() {
            return Vec::new();
        }
        self.beam_search_upto(std::cmp::min(60, 3 * roman.chars().count() + 5), beam, nbest, enc)
    }

    /// Beam search for the Likhi model's completion mode: the word the letters begin, or with no
    /// letters the next word. The letters say little about how long the word is, so the bound is
    /// the length of a long word rather than a multiple of what was typed.
    pub fn beam_search_complete(&self, beam: usize, nbest: usize, enc: &EncKv) -> Vec<(String, f32)> {
        self.beam_search_upto(COMPLETE_MAX_LEN, beam, nbest, enc)
    }

    fn beam_search_upto(&self, max_len: usize, beam: usize, nbest: usize, enc: &EncKv) -> Vec<(String, f32)> {
        let vsize = self.vocab_size();
        let banned = self.w.get(self.banned);
        let lenpen = 1.0f32;

        let mut tokens = vec![self.eos];
        let mut scores = vec![0.0f32];
        let mut seqs: Vec<Vec<usize>> = vec![Vec::new()];
        let mut caches: Vec<Vec<LayerCache>> = vec![vec![LayerCache::default(); self.dec_layers.len()]];
        let mut finished: Vec<(f32, Vec<usize>)> = Vec::new();

        let mut logits = Vec::new();
        for step in 0..max_len {
            // (hypothesis, token, score) for every continuation, then the best 2*beam of them.
            let mut cand: Vec<(usize, usize, f32)> = Vec::with_capacity(tokens.len() * 8);
            self.decode_step_batch(&tokens, step, &mut caches, enc, &mut logits);
            for b in 0..tokens.len() {
                for t in 0..vsize {
                    let mut lp = logits[b * vsize + t] + banned[t];
                    if step == 0 && t == self.eos {
                        lp = f32::NEG_INFINITY;
                    }
                    let sc = scores[b] + lp;
                    if sc.is_finite() {
                        cand.push((b, t, sc));
                    }
                }
            }
            let k = std::cmp::min(2 * beam, cand.len());
            // Descending by score. NumPy's argpartition/argsort pair is not stable, so an exact tie
            // has no defined order there either; ordering ties by (hypothesis, token) makes this
            // deterministic, which matters more than matching an order Python does not define.
            cand.sort_by(|a, b| {
                b.2.partial_cmp(&a.2)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.0.cmp(&b.0))
                    .then(a.1.cmp(&b.1))
            });
            cand.truncate(k);

            let mut new_tokens = Vec::new();
            let mut new_scores = Vec::new();
            let mut new_seqs = Vec::new();
            let mut new_src = Vec::new();
            for (j, &(b, t, sc)) in cand.iter().enumerate() {
                if t == self.eos {
                    if j < beam {
                        finished.push((sc / ((step + 1) as f32).powf(lenpen), seqs[b].clone()));
                    }
                    continue;
                }
                if new_tokens.len() < beam {
                    new_tokens.push(t);
                    new_scores.push(sc);
                    let mut s = seqs[b].clone();
                    s.push(t);
                    new_seqs.push(s);
                    new_src.push(b);
                }
            }
            if finished.len() >= beam || new_tokens.is_empty() {
                break;
            }
            let picked: Vec<Vec<LayerCache>> = new_src.iter().map(|&b| caches[b].clone()).collect();
            caches = picked;
            tokens = new_tokens;
            scores = new_scores;
            seqs = new_seqs;
        }

        if finished.is_empty() {
            // Ran out of length: fall back to the live hypotheses.
            finished = scores
                .iter()
                .zip(seqs.iter())
                .map(|(&s, q)| (s / (max_len as f32).powf(lenpen), q.clone()))
                .collect();
        }
        // Stable, like Python's list.sort, so equal scores keep the order they were finished in.
        finished.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut out = Vec::with_capacity(nbest);
        let mut seen = std::collections::HashSet::new();
        for (sc, q) in finished {
            let word: String = q
                .iter()
                .filter(|&&i| i != self.unk)
                .map(|&i| self.tgt_vocab[i].as_str())
                .collect();
            if !word.is_empty() && seen.insert(word.clone()) {
                out.push((word, sc));
            }
            if out.len() >= nbest {
                break;
            }
        }
        out
    }
}
