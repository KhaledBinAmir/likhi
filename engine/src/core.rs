//! Candidate generation from four channels plus a log-linear ranker.
//! A port of `src/likhi/engine/core.py`.
//!
//! Channels:
//! 1. attested romanizations -- roman -> word counts learned from Dakshina / Aksharantar / BanglaTLit
//! 2. phonetic keys -- fine and coarse consonant skeletons, which is what rescues "amr", "tmi", "korci"
//! 3. the transliteration model -- beam search for unseen words, and a score for every candidate
//! 4. the rule literal -- Avro Phonetic's reading, so any spelling stays typeable
//!
//! Ranking is a weighted sum of features. Personalization and bigram context fold into `score`.
//!
//! **Insertion order is part of the behaviour.** The Python builds candidates in a dict and then
//! sorts with `sorted(...)`, which is stable, so candidates with equal scores come out in the order
//! they were first added. That is why `Feats` lives in an insertion-ordered table here rather than a
//! `HashMap`: swapping in a hash map would compile, pass casual inspection, and quietly reorder tied
//! suggestions.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::avro::Avro;
use crate::lexicon::{rec_u32, rec_u32_i8, rec_u32x3, Table};
use crate::personal::PersonalStore;
use crate::romankey::{key_from_bangla, key_from_roman, Level};
use crate::textnorm::{canonical, normalize_roman, to_output};
use crate::xlit::Xlit;

/// The log-prior given to an unknown word the model is sure about.
const UNKNOWN_CONFIDENT_LOGP: f64 = -11.0;

/// How well attested a spelling must be before the fast path stops trusting candidates that agree
/// with nothing about the typed string. Chosen by measurement: at 5 the misaligned pairs that were
/// reaching the visible list disappear, top-1 is unchanged on every set, and top-5 moves by -0.02
/// on Dakshina and -0.35 on the chat set.
const FAST_TRUST_ROM_EXACT: u32 = 5;

/// Bengali spellings of English letter names, longest-disambiguating first.
///
/// The transliteration model reads vowel-less shorthand such as "amr" or "tmi" as an acronym
/// (এএমআর, টিএমআই) with high confidence, and those readings must not enjoy the confident-unknown-word
/// relief, or they beat আমার / তুমি.
///
/// Order matters and is copied verbatim: matching is first-wins, so এইচ must be tried before এ, or
/// "এইচ" would segment as এ + ইচ and fail.
const LETTER_NAMES: [&str; 26] = [
    "ডব্লিউ", "এইচ", "কিউ", "এক্স", "ওয়াই", "জেড", "এফ", "এম", "এন", "এল", "এস", "আর", "বি", "সি",
    "ডি", "ই", "জি", "জে", "কে", "পি", "টি", "ইউ", "ভি", "ও", "আই", "এ",
];

/// True when the word can be segmented into two or more English letter names.
pub fn looks_like_acronym(word: &str) -> bool {
    let mut i = 0usize;
    let mut parts = 0usize;
    while i < word.len() {
        let mut hit = false;
        for name in LETTER_NAMES {
            if word[i..].starts_with(name) {
                i += name.len();
                parts += 1;
                hit = true;
                break;
            }
        }
        if !hit {
            return false;
        }
    }
    parts >= 2
}

#[derive(Debug, Clone, Default)]
pub struct Feats {
    pub rom_exact: u32,
    pub rom_prefix: u32,
    pub key_fine: bool,
    pub key_coarse: bool,
    pub key_fine_prefix: bool,
    pub key_coarse_prefix: bool,
    pub gap: u32,
    pub xlit_rank: usize,
    /// NaN until the model scores it, exactly as the Python uses NaN as "not scored".
    pub xlit_logp: f64,
    pub avro: bool,
    pub lex_score: i64,
    pub in_lexicon: bool,
    pub is_latin: bool,
    pub personal_sel: f64,
    pub personal_share: f64,
    pub personal_word: f64,
    pub sources: HashSet<&'static str>,
}

impl Feats {
    fn new(lex_score: i64, in_lexicon: bool) -> Feats {
        Feats {
            lex_score,
            in_lexicon,
            xlit_logp: f64::NAN,
            ..Default::default()
        }
    }

    /// True when something other than a lone attested pair vouches for this candidate.
    ///
    /// Any phonetic agreement counts, exact or as a prefix, as does Avro's rule reading and a
    /// romanization seen more than once. What fails this test is a candidate whose entire case is
    /// one row of aligned training data -- the part of that data most likely to be a misalignment.
    fn fast_supported(&self) -> bool {
        self.key_fine
            || self.key_coarse
            || self.key_fine_prefix
            || self.key_coarse_prefix
            || self.avro
            || self.rom_prefix > 0
            || self.rom_exact >= 2
    }
}

/// An insertion-ordered map from word to features. See the module note on why this is not a HashMap.
#[derive(Default)]
pub struct FeatTable {
    order: Vec<String>,
    index: HashMap<String, usize>,
    feats: Vec<Feats>,
}

impl FeatTable {
    fn slot(&mut self, word: &str, lex_score: i64, in_lexicon: bool) -> usize {
        if let Some(&i) = self.index.get(word) {
            return i;
        }
        let i = self.feats.len();
        self.order.push(word.to_string());
        self.index.insert(word.to_string(), i);
        self.feats.push(Feats::new(lex_score, in_lexicon));
        i
    }

    pub fn len(&self) -> usize {
        self.feats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.feats.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Feats)> {
        self.order.iter().map(|w| w.as_str()).zip(self.feats.iter())
    }

    pub fn get(&self, word: &str) -> Option<&Feats> {
        self.index.get(word).map(|&i| &self.feats[i])
    }
}

pub struct Weights(HashMap<String, f64>);

impl Weights {
    fn default_map() -> HashMap<String, f64> {
        [
            ("unigram", 1.0),
            ("rom_exact", 2.5),
            ("rom_exact_log", 0.8),
            ("rom_exact_fast", 2.5),
            ("rom_prefix", 0.3),
            ("key_fine", 1.2),
            ("key_coarse", 0.6),
            ("key_prefix", -1.0),
            ("gap", -0.4),
            ("xlit_logp", 0.5),
            ("xlit_top1", 1.5),
            ("xlit_top3", 0.7),
            ("avro", 0.8),
            ("oov", -3.0),
            ("personal_sel", 5.0),
            ("personal_word", 0.6),
            ("bigram", 0.7),
            ("latin_base", -6.0),
            ("latin", 0.0),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    fn get(&self, key: &str) -> f64 {
        // Every key is present by construction: the defaults define the full set and a tuned file
        // only overwrites. A miss is a programming error, and returning 0.0 would hide it as a
        // subtly worse ranking rather than a crash.
        *self.0.get(key).unwrap_or_else(|| panic!("no weight named {key}"))
    }
}

pub struct Engine {
    uni: Table,
    romans: Table,
    keys: Table,
    prefixes: Option<Table>,
    bigrams: Option<Table>,
    bigram_totals: Option<Table>,
    uni_total: f64,
    floor: f64,
    w: Weights,
    pub beam: usize,
    max_per_channel: usize,
    model_scored: usize,
    xlit: Option<Xlit>,
    avro: Option<Avro>,
    pub personal: Option<PersonalStore>,
}

pub struct EngineOptions {
    pub beam: usize,
    pub max_per_channel: usize,
    pub model_scored: usize,
    pub use_avro: bool,
    pub use_xlit: bool,
    pub personal: Option<PersonalStore>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            beam: 4,
            max_per_channel: 30,
            model_scored: 16,
            use_avro: true,
            use_xlit: true,
            personal: None,
        }
    }
}

impl Engine {
    /// `data_dir` is the `models/rust` tree: `lexicon/`, `indicxlit/`, `avro.json`.
    pub fn open(data_dir: &Path, opts: EngineOptions) -> Result<Engine, Box<dyn std::error::Error>> {
        let lex = data_dir.join("lexicon");
        let uni = Table::open(&lex.join("unigrams.lkx"))?;
        let romans = Table::open(&lex.join("romans.lkx"))?;
        let keys = Table::open(&lex.join("keys.lkx"))?;
        let optional = |name: &str| -> Option<Table> {
            let p = lex.join(name);
            if p.exists() {
                Table::open(&p).ok()
            } else {
                None
            }
        };
        let prefixes = optional("prefixes.lkx");
        let bigrams = optional("bigrams.lkx");
        let bigram_totals = if bigrams.is_some() {
            optional("bigram_totals.lkx")
        } else {
            None
        };

        // The unigram mixture's normalizer: one pass over the table at startup, as the Python does.
        let mut total: u64 = 0;
        for (_w, rec) in uni.iter_all() {
            let (a, b, c) = rec_u32x3(rec);
            total += a as u64 + 3 * b as u64 + 20 * c as u64;
        }
        let uni_total = total as f64 + 1.0;
        let floor = (0.5 / uni_total).ln();

        let mut weights = Weights::default_map();
        let tuned = lex.join("weights.json");
        if tuned.exists() {
            if let Ok(text) = std::fs::read_to_string(&tuned) {
                if let Ok(map) = serde_json::from_str::<HashMap<String, f64>>(&text) {
                    weights.extend(map);
                }
            }
        }

        let xlit = if opts.use_xlit {
            Some(Xlit::open(&data_dir.join("indicxlit"))?)
        } else {
            None
        };
        let avro = if opts.use_avro {
            Avro::open(&data_dir.join("avro.json")).ok()
        } else {
            None
        };

        Ok(Engine {
            uni,
            romans,
            keys,
            prefixes,
            bigrams,
            bigram_totals,
            uni_total,
            floor,
            w: Weights(weights),
            beam: opts.beam,
            max_per_channel: opts.max_per_channel,
            model_scored: opts.model_scored,
            xlit,
            avro,
            personal: opts.personal,
        })
    }

    pub fn unigram_logp(&self, word: &str) -> f64 {
        match self.uni.get(word) {
            None => self.floor,
            Some(rec) => {
                let (a, b, c) = rec_u32x3(rec);
                let s = a as f64 + 3.0 * b as f64 + 20.0 * c as f64;
                ((s + 0.5) / self.uni_total).ln()
            }
        }
    }

    fn lex_score(&self, word: &str) -> i64 {
        match self.uni.get(word) {
            None => 0,
            Some(rec) => {
                let (a, b, c) = rec_u32x3(rec);
                a as i64 + 3 * b as i64 + 20 * c as i64
            }
        }
    }

    /// `log P(word | prev) - log P(word)`: how much the previous word changes the odds.
    ///
    /// Stupid backoff with a 0.4 penalty. A previous word with no bigram data gives 0 for every
    /// candidate, so context never hurts when it is uninformative.
    pub fn context_adjust(&self, word: &str, context: &[String]) -> f64 {
        let (Some(bigrams), Some(totals)) = (&self.bigrams, &self.bigram_totals) else {
            return 0.0;
        };
        let Some(prev) = context.last() else {
            return 0.0;
        };
        let Some(tot) = totals.get(prev) else {
            return 0.0;
        };
        let total = rec_u32(tot) as f64;
        match bigrams.get(&format!("{prev}\t{word}")) {
            Some(rec) => (rec_u32(rec) as f64 / total).ln() - self.unigram_logp(word),
            None => 0.4f64.ln(),
        }
    }

    pub fn candidates(&mut self, roman: &str, use_model: bool) -> FeatTable {
        let r = normalize_roman(roman);
        let mut t = FeatTable::default();
        if r.is_empty() {
            return t;
        }

        // 1. attested romanizations: exact, then completions.
        //
        // Source order, not key order: these go straight into the candidate table and equal scores
        // are broken by insertion order, so the trie's own enumeration is what must be reproduced.
        for (key, rec) in self.romans.prefix_iter_source_order(&format!("{r}\t")) {
            let Some((_, word)) = key.split_once('\t') else { continue };
            let word = canonical(word);
            let (count, _src) = rec_u32_i8(rec);
            let i = t.slot(&word, self.lex_score(&word), self.uni.get(&word).is_some());
            t.feats[i].rom_exact += count;
            t.feats[i].sources.insert("rom");
        }

        // (score, word, count, gap) sorted descending, exactly like the Python tuple sort: score
        // first, then the word string, then count, then gap.
        let mut completions: Vec<(i64, String, u32, u32)> = Vec::new();
        match (&self.prefixes, r.chars().count() <= 3) {
            (Some(prefixes), true) => {
                // Short input: precomputed top completions instead of scanning thousands of entries.
                for (key, rec) in prefixes.prefix_iter(&format!("r:{r}\t")) {
                    let Some((_, word)) = key.split_once('\t') else { continue };
                    completions.push((rec_u32(rec) as i64, word.to_string(), 1, 2));
                }
            }
            _ => {
                for (key, rec) in self.romans.prefix_iter(&r) {
                    let Some((rom, word)) = key.split_once('\t') else { continue };
                    if rom == r {
                        continue;
                    }
                    let (count, _src) = rec_u32_i8(rec);
                    let score =
                        count as i64 * 10_000 + std::cmp::min(self.lex_score(word), 9_999);
                    let gap = (rom.chars().count() - r.chars().count()) as u32;
                    completions.push((score, word.to_string(), count, gap));
                }
            }
        }
        completions.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.as_bytes().cmp(a.1.as_bytes()))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| b.3.cmp(&a.3))
        });
        for (_s, word, count, gap) in completions.into_iter().take(self.max_per_channel) {
            let word = canonical(&word);
            let i = t.slot(&word, self.lex_score(&word), self.uni.get(&word).is_some());
            if t.feats[i].rom_exact == 0 {
                t.feats[i].rom_prefix += count;
                t.feats[i].gap = if t.feats[i].gap == 0 {
                    gap
                } else {
                    std::cmp::min(t.feats[i].gap, gap)
                };
            }
            t.feats[i].sources.insert("rom+");
        }

        // 2. phonetic keys.
        for (level, exact_src, prefix_src) in [
            (Level::Fine, "key-fine", "key-fine+"),
            (Level::Coarse, "key-coarse", "key-coarse+"),
        ] {
            let k = key_from_roman(&r, level);
            if k.is_empty() {
                continue;
            }
            let prefix = format!("{}:{}", level.tag(), k);
            let mut exact: Vec<(u32, String)> = Vec::new();
            let mut longer: Vec<(u32, String)> = Vec::new();
            match (&self.prefixes, k.chars().count() <= 2) {
                (Some(prefixes), true) => {
                    for (key, rec) in self.keys.prefix_iter(&format!("{prefix}\t")) {
                        if let Some((_, word)) = key.split_once('\t') {
                            exact.push((rec_u32(rec), word.to_string()));
                        }
                    }
                    for (key, rec) in prefixes.prefix_iter(&format!("k:{}:{}\t", level.tag(), k)) {
                        if let Some((_, word)) = key.split_once('\t') {
                            longer.push((rec_u32(rec), word.to_string()));
                        }
                    }
                }
                _ => {
                    for (key, rec) in self.keys.prefix_iter(&prefix) {
                        let Some((kk, word)) = key.split_once('\t') else { continue };
                        if kk == prefix {
                            exact.push((rec_u32(rec), word.to_string()));
                        } else {
                            longer.push((rec_u32(rec), word.to_string()));
                        }
                    }
                }
            }
            let desc = |a: &(u32, String), b: &(u32, String)| {
                b.0.cmp(&a.0).then_with(|| b.1.as_bytes().cmp(a.1.as_bytes()))
            };
            exact.sort_by(desc);
            longer.sort_by(desc);

            for (_s, word) in exact.into_iter().take(self.max_per_channel) {
                let word = canonical(&word);
                let i = t.slot(&word, self.lex_score(&word), self.uni.get(&word).is_some());
                match level {
                    Level::Fine => t.feats[i].key_fine = true,
                    Level::Coarse => t.feats[i].key_coarse = true,
                }
                t.feats[i].sources.insert(exact_src);
            }
            for (_s, word) in longer.into_iter().take(self.max_per_channel / 2) {
                let word = canonical(&word);
                let i = t.slot(&word, self.lex_score(&word), self.uni.get(&word).is_some());
                match level {
                    Level::Fine => t.feats[i].key_fine_prefix = true,
                    Level::Coarse => t.feats[i].key_coarse_prefix = true,
                }
                let already = match level {
                    Level::Fine => t.feats[i].key_fine,
                    Level::Coarse => t.feats[i].key_coarse,
                };
                if t.feats[i].rom_exact == 0 && !already {
                    let g = std::cmp::max(
                        1,
                        key_from_bangla(&word, level).chars().count() as i64
                            - k.chars().count() as i64,
                    ) as u32;
                    t.feats[i].gap = if t.feats[i].gap == 0 {
                        g
                    } else {
                        std::cmp::min(t.feats[i].gap, g)
                    };
                }
                t.feats[i].sources.insert(prefix_src);
            }
        }

        // 3. transliteration model. The encoder runs once and is shared with the scoring pass below.
        let enc = match (&self.xlit, use_model) {
            (Some(x), true) => {
                let enc = x.encode(&r, "bn");
                for (rank, (word, lp)) in
                    x.beam_search(&r, self.beam, self.beam, &enc).into_iter().enumerate()
                {
                    let word = canonical(&word);
                    let chars = word.chars().count();
                    let i = t.slot(&word, self.lex_score(&word), self.uni.get(&word).is_some());
                    if t.feats[i].xlit_rank == 0 {
                        t.feats[i].xlit_rank = rank + 1;
                    }
                    // The beam's scores are length-normalized (sum / (chars + EOS)); undo that to
                    // match the raw sums score_candidates produces.
                    t.feats[i].xlit_logp = lp as f64 * (chars + 1) as f64;
                    t.feats[i].sources.insert("xlit");
                }
                Some(enc)
            }
            _ => None,
        };

        // 4. rule literal.
        if let Some(a) = &self.avro {
            let lit = a.parse(roman);
            if !lit.is_empty() && lit != roman {
                let word = canonical(&lit);
                let i = t.slot(&word, self.lex_score(&word), self.uni.get(&word).is_some());
                t.feats[i].avro = true;
                t.feats[i].sources.insert("avro");
            }
        }

        // 5. personal history, including the raw Latin the user may have chosen before.
        if self.personal.is_some() {
            let sel = self.personal.as_mut().expect("checked").selections(&r, None);
            let total: f64 = sel.iter().map(|(_, c)| *c).sum();
            let total = if total == 0.0 { 1.0 } else { total };
            for (word, count) in &sel {
                let cw = canonical(word);
                let i = t.slot(&cw, self.lex_score(&cw), self.uni.get(&cw).is_some());
                if word == &r || word == roman {
                    t.feats[i].is_latin = true;
                }
                t.feats[i].personal_sel = *count;
                t.feats[i].personal_share = count / total;
                t.feats[i].sources.insert("personal");
            }
            let words: Vec<String> = t.order.clone();
            for (idx, word) in words.iter().enumerate() {
                if !t.feats[idx].is_latin {
                    let c = self.personal.as_mut().expect("checked").word_count(word, None);
                    t.feats[idx].personal_word = c;
                }
            }
        }

        // Model scores for the most promising candidates only: batched teacher forcing is the
        // expensive step, everything else was a table lookup.
        if let (Some(x), Some(enc), true) = (&self.xlit, &enc, !t.is_empty()) {
            let mut unscored: Vec<usize> =
                (0..t.feats.len()).filter(|&i| t.feats[i].xlit_logp.is_nan()).collect();
            // Stable, so equal scores keep insertion order, as Python's sorted does.
            let scores: Vec<f64> = unscored
                .iter()
                .map(|&i| self.score_inner(&t.order[i], &t.feats[i], &[]))
                .collect();
            let mut order: Vec<usize> = (0..unscored.len()).collect();
            order.sort_by(|&a, &b| {
                scores[b].partial_cmp(&scores[a]).unwrap_or(std::cmp::Ordering::Equal)
            });
            unscored = order.into_iter().map(|j| unscored[j]).collect();
            unscored.truncate(self.model_scored);

            let words: Vec<String> = unscored.iter().map(|&i| t.order[i].clone()).collect();
            if !words.is_empty() {
                let lps = x.score_candidates(&words, enc);
                for (&i, lp) in unscored.iter().zip(lps.iter()) {
                    t.feats[i].xlit_logp = *lp as f64;
                }
            }
        }

        t
    }

    fn score_inner(&self, word: &str, ft: &Feats, context: &[String]) -> f64 {
        let w = &self.w;
        if ft.is_latin {
            // Raw Latin only competes once the user has chosen it before, and then on personal
            // evidence rather than on Bangla corpus frequency.
            return w.get("latin_base")
                + w.get("latin")
                + personal_bonus(w, ft.personal_sel, ft.personal_share);
        }
        let mut uni = self.unigram_logp(word);
        let xl = ft.xlit_logp;
        let acronym = !ft.in_lexicon && looks_like_acronym(word);
        if !ft.in_lexicon && !xl.is_nan() && !acronym {
            // Unknown words sit at the unigram floor, a hidden second penalty. When the model is
            // confident the word is real, lift the prior towards that of a rare-but-real word.
            let confidence = 1.0 - (-xl / 3.0).clamp(0.0, 1.0);
            uni += (UNKNOWN_CONFIDENT_LOGP - uni) * confidence;
        }
        let mut s = w.get("unigram") * uni;
        if !context.is_empty() {
            s += w.get("bigram") * self.context_adjust(word, context);
        }
        if ft.rom_exact > 0 {
            s += w.get("rom_exact") + w.get("rom_exact_log") * (1.0 + ft.rom_exact as f64).ln();
        }
        if ft.rom_prefix > 0 && ft.rom_exact == 0 {
            s += w.get("rom_prefix") * (1.0 + ft.rom_prefix as f64).ln();
        }
        if ft.key_fine {
            s += w.get("key_fine");
        } else if ft.key_coarse {
            s += w.get("key_coarse");
        } else if ft.key_fine_prefix || ft.key_coarse_prefix {
            s += w.get("key_prefix");
        }
        if !xl.is_nan() {
            s += w.get("xlit_logp") * xl.max(-40.0);
        } else {
            // Not scored by the model: assume a poor fit so scored candidates can outrank it.
            s += w.get("xlit_logp") * -15.0;
        }
        if ft.gap > 0 && ft.rom_exact == 0 {
            s += w.get("gap") * ft.gap as f64;
        }
        if ft.xlit_rank == 1 {
            s += w.get("xlit_top1");
        } else if ft.xlit_rank > 1 && ft.xlit_rank <= 3 {
            s += w.get("xlit_top3");
        }
        if ft.avro {
            s += w.get("avro");
        }
        if !ft.in_lexicon {
            // The unknown-word penalty encodes "probably not a real word". A confident model is
            // evidence to the contrary: at log P > -3 the penalty fades, at log P ~ 0 it vanishes
            // (খাইতেছো for "khaitecho" is unknown to the lexicon but certain for the model).
            // Acronym readings of shorthand keep the full penalty.
            let scale = if xl.is_nan() || acronym {
                1.0
            } else {
                (-xl / 3.0).clamp(0.0, 1.0)
            };
            s += w.get("oov") * scale;
        }
        if ft.personal_sel != 0.0 {
            s += personal_bonus(w, ft.personal_sel, ft.personal_share);
        }
        if ft.personal_word != 0.0 {
            s += w.get("personal_word") * ft.personal_word.ln_1p();
        }
        s
    }

    pub fn score(&self, word: &str, ft: &Feats, context: &[String]) -> f64 {
        self.score_inner(word, ft, context)
    }

    /// Ranked candidates. `fast` skips the transliteration model.
    pub fn suggest(&mut self, roman: &str, context: &[String], k: usize, fast: bool) -> Vec<String> {
        let r = normalize_roman(roman);
        let prev: Vec<String> = context.last().map(|c| vec![canonical(c)]).unwrap_or_default();
        let t = self.candidates(&r, !fast);
        if t.is_empty() {
            return Vec::new();
        }
        let extra = if fast { self.w.get("rom_exact_fast") } else { 0.0 };
        let mut idx: Vec<usize> = (0..t.feats.len()).collect();
        let scored: Vec<f64> = idx
            .iter()
            .map(|&i| {
                self.score_inner(&t.order[i], &t.feats[i], &prev)
                    + extra * (1.0 + t.feats[i].rom_exact as f64).ln()
            })
            .collect();
        idx.sort_by(|&a, &b| {
            scored[b].partial_cmp(&scored[a]).unwrap_or(std::cmp::Ordering::Equal)
        });
        idx.into_iter().take(k).map(|i| to_output(&t.order[i])).collect()
    }

    /// Model-free ranking plus a confidence flag, from one pass over the table channels.
    ///
    /// The flag is true when some candidate is an attested spelling of the typed string (count >=
    /// 2). Phonetic-key matches alone are not enough: "khacche" key-matches কিছু and কাছে, which
    /// would be shown while the model's খাচ্ছে is still computing.
    ///
    /// Candidates that agree with nothing about the typed string are pushed behind those that do.
    /// The aligned romanization data has a tail of misaligned pairs, and one of those plus a high
    /// unigram count is enough to reach the visible list with no model to contradict it: কোন and
    /// হিসেবে for "bangla", খনির for "sonar", each attested exactly once and matching neither
    /// phonetic key. That list is selectable, so a user pressing 4 committed a word they never
    /// typed. Only applied when something is well attested for this exact spelling, because when
    /// the best evidence is a single occurrence that candidate may be all there is -- and demoted
    /// rather than removed, so they still fill slots nothing better is competing for.
    pub fn fast_suggest(
        &mut self,
        roman: &str,
        context: &[String],
        k: usize,
    ) -> (Vec<String>, bool) {
        let r = normalize_roman(roman);
        let prev: Vec<String> = context.last().map(|c| vec![canonical(c)]).unwrap_or_default();
        let t = self.candidates(&r, false);
        if t.is_empty() {
            return (Vec::new(), false);
        }
        let extra = self.w.get("rom_exact_fast");
        let demote = t.feats.iter().map(|f| f.rom_exact).max().unwrap_or(0) >= FAST_TRUST_ROM_EXACT;
        let base: Vec<f64> = (0..t.feats.len())
            .map(|i| {
                self.score_inner(&t.order[i], &t.feats[i], &prev)
                    + extra * (1.0 + t.feats[i].rom_exact as f64).ln()
            })
            .collect();
        let mut idx: Vec<usize> = (0..t.feats.len()).collect();
        idx.sort_by(|&a, &b| {
            let ka = u8::from(demote && !t.feats[a].fast_supported());
            let kb = u8::from(demote && !t.feats[b].fast_supported());
            ka.cmp(&kb)
                .then_with(|| base[b].partial_cmp(&base[a]).unwrap_or(std::cmp::Ordering::Equal))
        });
        let strong = t.feats.iter().any(|f| f.rom_exact >= 2);
        let out = idx.into_iter().take(k).map(|i| to_output(&t.order[i])).collect();
        (out, strong)
    }

    /// True when the table channels alone have solid evidence -- an attested spelling.
    pub fn has_strong_match(&mut self, roman: &str) -> bool {
        self.fast_suggest(roman, &[], 1).1
    }

    /// Record a commit so the personal model learns from it.
    pub fn learn(&mut self, roman: &str, chosen: &str) {
        if roman.is_empty() || chosen.is_empty() {
            return;
        }
        let r = normalize_roman(roman);
        if let Some(p) = self.personal.as_mut() {
            let _ = p.learn(&r, chosen, None);
        }
    }
}

/// Evidence from the user's own picks for this exact roman string.
///
/// Grows with the log of the pick count, scaled by how consistently this word was the choice.
/// Calibrated so one pick never flips a strongly established word, two picks flip a close call, and
/// about four consistent picks flip anything.
fn personal_bonus(w: &Weights, count: f64, share: f64) -> f64 {
    w.get("personal_sel") * count.ln_1p() * (0.5 + share)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acronym_segmentation_prefers_longer_letter_names() {
        // এইচ must be taken whole; if এ matched first this would fail to segment.
        assert!(looks_like_acronym("এএমআর"));
        assert!(looks_like_acronym("টিএমআই"));
        // A real word is not an acronym.
        assert!(!looks_like_acronym("আমার"));
        // A single letter name is not enough: two or more parts are required.
        assert!(!looks_like_acronym("এ"));
    }
}
