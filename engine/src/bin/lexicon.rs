//! Build the lexicon tables the engine loads at runtime.
//! A port of `src/likhi/data/build_lexicon.py` and `build_bigrams.py`.
//!
//!     cargo run --release --features build --bin likhi-lexicon -- --raw data/raw --out models/rust/lexicon
//!
//! Inputs, all under `data/raw` (fetched by `scripts/fetch_datasets.py`):
//!
//! * Dakshina native-script Wikipedia sentences -> formal-register unigram counts
//! * FrequencyWords bn (OpenSubtitles)          -> conversational unigram counts
//! * BanglaTLit Bengali side, train rows only   -> chat-register unigram counts
//! * Dakshina lexicon, Aksharantar and BanglaTLit train pairs -> attested romanizations
//!
//! Outputs `.lkx` tables directly, which is the point: the Python builder wrote `marisa` tries and
//! a conversion step turned those into `.lkx`, so `marisa_trie` was a build dependency of a product
//! that no longer contains any Python.
//!
//! **One behaviour changes, deliberately.** A prefix scan over the romanization table yields
//! candidates in an order that decides how exact score ties break, and under marisa that order was
//! a LOUDS traversal -- an artifact of how the trie happened to be laid out. Here it is attestation
//! count, highest first. Measured with `scripts/compare_lexicons.py` and the evaluation harness:
//! the order had no measurable effect on dakshina-dev accuracy, so this is a change from an
//! accident to a rule, not a change in behaviour anyone can see.
//!
//! **`weights.json` is not built here and is carried forward from `--weights`.** It is produced by
//! `likhi-tune` and versioned beside the lexicon, and its absence is silent and expensive: the
//! engine falls back to untuned defaults and loses about seven points of top-1. The first
//! measurement of this builder's output was exactly that failure, misread for an hour as an
//! ordering effect. The builder now refuses to finish quietly without it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use likhi_engine::lexicon::write::write_table;
use likhi_engine::romankey::{key_from_bangla, Level};
use likhi_engine::textnorm::{canonical, has_bengali, match_key, normalize_roman};

/// Source bits recorded alongside each romanization, matching the Python's constants.
const SRC_DAKSHINA: i8 = 1;
const SRC_AKSHARANTAR: i8 = 2;
const SRC_BANGLATLIT: i8 = 4;

/// Completions are precomputed for typed prefixes up to these lengths; see `build_prefix_tables`.
const SHORT_ROMAN_PREFIX: usize = 3;
const SHORT_KEY_PREFIX: usize = 2;
const PREFIX_TOP: usize = 24;

const I32_MAX: u64 = i32::MAX as u64;

fn log(msg: &str) {
    println!("[lexicon] {msg}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

// --------------------------------------------------------------------------------- tokenising

/// True for the Bengali block and the two joiners, which is the Python's character class
/// `[U+0980-U+09FF, U+200C, U+200D]`.
fn is_bengali_token_char(c: char) -> bool {
    matches!(c, '\u{0980}'..='\u{09FF}' | '\u{200C}' | '\u{200D}')
}

/// True when a token is only digits and marks, which the Python drops with
/// `^[U+09E6-U+09EF, U+0981-U+0983, U+09BC, U+09BE-U+09CD, U+09D7, U+200C, U+200D]+$`.
fn is_all_digits_or_marks(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            matches!(c,
                '\u{09E6}'..='\u{09EF}'   // Bengali digits
                | '\u{0981}'..='\u{0983}' // candrabindu, anusvara, visarga
                | '\u{09BC}'              // nukta
                | '\u{09BE}'..='\u{09CD}' // vowel signs through virama
                | '\u{09D7}'              // au length mark
                | '\u{200C}' | '\u{200D}')
        })
}

/// Bengali word tokens of a line, canonicalised. Mirrors `build_lexicon.tokens`.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_bengali_token_char(c) {
            current.push(c);
        } else if !current.is_empty() {
            if !is_all_digits_or_marks(&current) {
                out.push(canonical(&current));
            }
            current.clear();
        }
    }
    if !current.is_empty() && !is_all_digits_or_marks(&current) {
        out.push(canonical(&current));
    }
    out
}

/// Strip leading and trailing punctuation, keeping the Bengali block and the joiners as word
/// characters. Python's `\w` does not cover combining marks, so a naive strip would cut the final
/// vowel sign off a word such as U+0995 U+09BF U+09A8 U+09CD U+09A4 U+09C1.
fn strip_punct(tok: &str) -> &str {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || is_bengali_token_char(c);
    let start = tok.find(is_word).unwrap_or(tok.len());
    let end = tok.rfind(is_word).map(|i| i + tok[i..].chars().next().map_or(1, char::len_utf8));
    &tok[start..end.unwrap_or(start)]
}

fn is_latin_only(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic() || c == '\'')
}

// --------------------------------------------------------------------------------- counting

/// Counts by `match_key` but remembers the most frequent surface (output) form.
///
/// Surfaces carry the order they were first seen in, and ties are broken by it. That is not
/// tidiness: several spellings of a word share one `match_key` -- U+09F0 against U+09B0, a
/// trailing ZWNJ present or absent -- and they routinely tie on count, so whichever wins is
/// the spelling the user
/// is shown. `Counter.most_common(1)` in the Python resolves those ties by insertion order, and
/// breaking them lexicographically instead changed the chosen spelling of 117 words. The inputs are
/// read in a fixed order, so first-seen is deterministic and the build stays reproducible.
#[derive(Default)]
struct SurfaceCounter {
    counts: HashMap<String, u64>,
    surface: HashMap<String, HashMap<String, (u64, u32)>>,
    next: u32,
}

impl SurfaceCounter {
    fn add(&mut self, word: &str, n: u64) {
        let k = match_key(word);
        *self.counts.entry(k.clone()).or_insert(0) += n;
        let order = self.next;
        let slot = self
            .surface
            .entry(k)
            .or_default()
            .entry(word.to_string())
            .or_insert((0, order));
        if slot.1 == order {
            self.next += 1;
        }
        slot.0 += n;
    }

    fn best_surface(&self, k: &str) -> Option<String> {
        self.surface.get(k).and_then(best_of)
    }
}

/// Highest count wins; on a tie the one seen first, matching `Counter.most_common(1)`.
fn best_of(m: &HashMap<String, (u64, u32)>) -> Option<String> {
    m.iter()
        .min_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.1 .1.cmp(&b.1 .1)))
        .map(|(w, _)| w.clone())
}

// --------------------------------------------------------------------------------- readers

fn open_lines(path: &Path) -> std::io::Result<Box<dyn BufRead>> {
    let file = std::fs::File::open(path)?;
    if path.extension().is_some_and(|e| e == "gz") {
        Ok(Box::new(BufReader::new(flate2::read::GzDecoder::new(file))))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

fn count_wiki(raw: &Path, limit: Option<usize>) -> std::io::Result<SurfaceCounter> {
    let path = raw
        .join("dakshina/bn/native_script_wikipedia/bn.wiki-filt.train.text.shuf.txt.gz");
    let mut sc = SurfaceCounter::default();
    let mut n = 0usize;
    for line in open_lines(&path)?.lines() {
        let line = line?;
        for tok in tokens(&line) {
            sc.add(&tok, 1);
        }
        n += 1;
        if limit.is_some_and(|l| n >= l) {
            break;
        }
    }
    let total: u64 = sc.counts.values().sum();
    log(&format!("wiki: {n} lines, {} types, {total} tokens", sc.counts.len()));
    Ok(sc)
}

fn count_subs(raw: &Path) -> std::io::Result<SurfaceCounter> {
    let mut sc = SurfaceCounter::default();
    for line in open_lines(&raw.join("frequencywords/bn_full.txt"))?.lines() {
        let line = line?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 2 {
            continue;
        }
        let Ok(count) = parts[1].parse::<u64>() else { continue };
        let toks = tokens(parts[0]);
        if toks.len() != 1 {
            continue;
        }
        sc.add(&toks[0], count);
    }
    log(&format!("subs: {} types", sc.counts.len()));
    Ok(sc)
}

struct ChatRow {
    bengali: String,
    roman: String,
}

/// Annotated train rows, excluding anything that also appears in val or test -- the train file
/// contains them.
fn banglatlit_train_rows(raw: &Path) -> Result<Vec<ChatRow>, Box<dyn std::error::Error>> {
    let mut held: std::collections::HashSet<String> = std::collections::HashSet::new();
    for split in ["val", "test"] {
        let mut rdr = csv::Reader::from_path(raw.join(format!("banglatlit/{split}.csv")))?;
        for rec in rdr.deserialize::<HashMap<String, String>>() {
            let row = rec?;
            if let Some(id) = row.get("id") {
                held.insert(id.clone());
            }
            if let Some(t) = row.get("text_transliterated") {
                held.insert(t.trim().to_string());
            }
        }
    }
    let mut rows = Vec::new();
    let mut rdr = csv::Reader::from_path(raw.join("banglatlit/train.csv"))?;
    for rec in rdr.deserialize::<HashMap<String, String>>() {
        let row = rec?;
        let bn = row.get("text_bengali").map(|s| s.trim()).unwrap_or("");
        let rm = row.get("text_transliterated").map(|s| s.trim()).unwrap_or("");
        let id = row.get("id").cloned().unwrap_or_default();
        if bn.is_empty() || rm.is_empty() || held.contains(&id) || held.contains(rm) {
            continue;
        }
        rows.push(ChatRow { bengali: bn.to_string(), roman: rm.to_string() });
    }
    Ok(rows)
}

fn count_chat(rows: &[ChatRow]) -> SurfaceCounter {
    let mut sc = SurfaceCounter::default();
    for row in rows {
        for tok in tokens(&row.bengali) {
            sc.add(&tok, 1);
        }
    }
    log(&format!("chat: {} sentences, {} types", rows.len(), sc.counts.len()));
    sc
}

// --------------------------------------------------------------------------------- romanizations

/// One romanization as the builder walks it: `(roman, word)` with `(count, source bits, first-seen)`.
type RomanEntry<'a> = (&'a (String, String), &'a (u64, i8, u32));

/// `(roman, word) -> (count, source bits, first-seen order)`.
///
/// The first-seen order is kept so surface-form tie-breaks match the Python's insertion order.
struct Romans {
    map: HashMap<(String, String), (u64, i8, u32)>,
    next: u32,
}

impl Romans {
    fn new() -> Romans {
        Romans { map: HashMap::new(), next: 0 }
    }

    fn add(&mut self, roman: &str, word: &str, n: u64, src: i8) {
        let r = normalize_roman(roman);
        let w = canonical(word);
        if r.is_empty() || w.is_empty() {
            return;
        }
        let next = self.next;
        let e = self.map.entry((r, w)).or_insert_with(|| {
            (0, 0, next)
        });
        if e.2 == next {
            self.next += 1;
        }
        e.0 += n;
        e.1 |= src;
    }
}

fn collect_romans(raw: &Path, chat: &[ChatRow]) -> Result<Romans, Box<dyn std::error::Error>> {
    let mut rom = Romans::new();

    // Dakshina lexicon: native \t roman \t count
    let path = raw.join("dakshina/bn/lexicons/bn.translit.sampled.train.tsv");
    let (mut ok, mut bad) = (0usize, 0usize);
    for line in open_lines(&path)?.lines() {
        let line = line?;
        let parts: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        // Reported rather than defaulted to zero: a count that fails to parse would silently
        // become an unattested pair, which is exactly the kind of quiet corruption that is
        // impossible to notice in a 95,000-row table.
        match parts[2].trim().parse::<f64>() {
            Ok(count) => {
                rom.add(parts[1], parts[0], count as u64, SRC_DAKSHINA);
                ok += 1;
            }
            Err(_) => bad += 1,
        }
    }
    if bad > 0 {
        log(&format!("WARNING dakshina: {bad} rows with an unreadable count"));
    }
    log(&format!("romans after dakshina ({ok} rows): {}", rom.map.len()));

    // Aksharantar: one JSON object per line.
    //
    // Deserialized into a struct with only the two fields that matter, not a map of strings: the
    // rows carry `"score": null`, which is not a string, so a `HashMap<String, String>` fails on
    // every line. Skipping failures quietly dropped all 1.1M rows and produced a lexicon that
    // looked plausible -- hence the count below, which is reported rather than swallowed.
    #[derive(serde::Deserialize)]
    struct AksharantarRow {
        #[serde(rename = "native word")]
        native: String,
        #[serde(rename = "english word")]
        english: String,
    }

    let path = raw.join("aksharantar/ben_train.json");
    if path.exists() {
        let (mut ok, mut bad) = (0usize, 0usize);
        for line in open_lines(&path)?.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<AksharantarRow>(line) {
                Ok(row) => {
                    rom.add(&row.english, &row.native, 1, SRC_AKSHARANTAR);
                    ok += 1;
                }
                Err(_) => bad += 1,
            }
        }
        if bad > 0 {
            log(&format!("WARNING aksharantar: {bad} unreadable lines of {}", ok + bad));
        }
        log(&format!("romans after aksharantar ({ok} rows): {}", rom.map.len()));
    } else {
        log("aksharantar absent; skipping");
    }

    // BanglaTLit: positional alignment of same-length sentences, the same recipe as the eval loader
    let mut pairs = 0usize;
    for row in chat {
        let r_toks: Vec<&str> = row.roman.split_whitespace().map(strip_punct).collect();
        let b_toks: Vec<&str> = row.bengali.split_whitespace().map(strip_punct).collect();
        if r_toks.len() != b_toks.len() {
            continue;
        }
        for (r, b) in r_toks.iter().zip(b_toks.iter()) {
            if !r.is_empty()
                && !b.is_empty()
                && is_latin_only(r)
                && has_bengali(b)
                && !b.chars().any(|c| c.is_ascii_alphabetic())
            {
                rom.add(r, b, 1, SRC_BANGLATLIT);
                pairs += 1;
            }
        }
    }
    log(&format!("romans after banglatlit ({pairs} aligned pairs): {}", rom.map.len()));
    Ok(rom)
}

// --------------------------------------------------------------------------------- output

fn rec_u32(v: u64) -> Vec<u8> {
    (v.min(I32_MAX) as u32).to_le_bytes().to_vec()
}

/// Top completions for very short inputs.
///
/// A one- or two-letter prefix matches tens of thousands of entries, and scanning them at every
/// keystroke is what made "ki" take 180 ms. For short roman prefixes and short phonetic-key
/// prefixes the best `PREFIX_TOP` words are picked once here and looked up directly at runtime.
fn build_prefix_tables<'a>(
    out: &Path,
    rom_items: &'a [(String, u64, i8)],
    key_items: &'a [(String, u64)],
) -> std::io::Result<()> {
    let mut lex: HashMap<&str, u64> = HashMap::new();
    for (key, score) in key_items {
        if let Some((_, word)) = key.split_once('\t') {
            lex.insert(word, *score);
        }
    }
    // Each word carries the order it was first added to this prefix. Only 24 survive the cut below
    // and scores tie constantly at these sizes, so the tie-break decides which completions a short
    // input offers at all. The Python's `sorted(...)[:24]` is stable, which means insertion order;
    // breaking ties lexicographically instead changed 9,571 of 124,267 entries.
    let mut best: HashMap<String, HashMap<&'a str, (u64, u32)>> = HashMap::new();
    let mut next: u32 = 0;

    /// Record `word` under prefix `p` with score `s`, keeping the highest score and the order it
    /// was first seen. A free function rather than a closure: the borrow checker cannot tie a
    /// closure's captured map to the `'a` of its argument.
    fn note<'a>(
        best: &mut HashMap<String, HashMap<&'a str, (u64, u32)>>,
        next: &mut u32,
        p: String,
        word: &'a str,
        s: u64,
    ) {
        let order = *next;
        let slot = best.entry(p).or_default().entry(word).or_insert((0, order));
        if slot.1 == order {
            *next += 1;
        }
        if s > slot.0 {
            slot.0 = s;
        }
    }

    for (key, count, _src) in rom_items {
        let Some((roman, word)) = key.split_once('\t') else { continue };
        // Attestation count first, corpus frequency only to break ties: a single noisy alignment of
        // a very frequent word must not outrank a well-attested completion.
        let s = count * 10_000 + std::cmp::min(lex.get(word).copied().unwrap_or(0), 9_999);
        let chars: Vec<char> = roman.chars().collect();
        for n in 1..=std::cmp::min(SHORT_ROMAN_PREFIX, chars.len()) {
            let p: String = format!("r:{}", chars[..n].iter().collect::<String>());
            note(&mut best, &mut next, p, word, s);
        }
    }
    for (key, score) in key_items {
        let Some((kk, word)) = key.split_once('\t') else { continue };
        let Some((level, pk)) = kk.split_once(':') else { continue };
        let chars: Vec<char> = pk.chars().collect();
        for n in 1..=std::cmp::min(SHORT_KEY_PREFIX, chars.len()) {
            let p = format!("k:{level}:{}", chars[..n].iter().collect::<String>());
            note(&mut best, &mut next, p, word, *score);
        }
    }
    let n_prefixes = best.len();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for (p, words) in &best {
        let mut ranked: Vec<(&&str, &(u64, u32))> = words.iter().collect();
        ranked.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.1 .1.cmp(&b.1 .1)));
        for (w, (s, _)) in ranked.into_iter().take(PREFIX_TOP) {
            entries.push((format!("{p}\t{w}"), rec_u32(*s)));
        }
    }
    log(&format!("prefix tables: {n_prefixes} short prefixes, {} entries", entries.len()));
    write_table(&out.join("prefixes.lkx"), entries, 4, None)
}

struct Args {
    raw: PathBuf,
    out: PathBuf,
    /// The tuned ranker weights to place beside the tables. Not a build output; see the module
    /// note on why a lexicon directory without it is a quiet seven-point regression.
    weights: PathBuf,
    wiki_lines: Option<usize>,
    min_wiki: u64,
    bigram_min_count: u64,
    bigram_per_prev: usize,
}

fn default_weights() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|repo| repo.join("models").join("lexicon").join("weights.json"))
        .unwrap_or_else(|| PathBuf::from("models/lexicon/weights.json"))
}

fn build(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    // Checked first, not last: the tables are useless to the shipped engine without the tuned
    // weights, and finding that out after a minute of building is a minute wasted. An error rather
    // than a warning, because a warning in a build log is how the first run of this builder
    // produced a lexicon that measured seven points worse than the one it replaced.
    if !args.weights.exists() {
        return Err(format!(
            "no tuned weights at {}; pass --weights <path to weights.json> (produced by likhi-tune)",
            args.weights.display()
        )
        .into());
    }
    std::fs::create_dir_all(&args.out)?;

    let wiki = count_wiki(&args.raw, args.wiki_lines)?;
    let subs = count_subs(&args.raw)?;
    let chat_rows = banglatlit_train_rows(&args.raw)?;
    let chat = count_chat(&chat_rows);
    let romans = collect_romans(&args.raw, &chat_rows)?;

    // Lexicon membership: corpus words above a small threshold, plus the human-romanized Dakshina
    // words. Aksharantar's mined words stay reachable through the romanization index only -- they
    // are mostly named entities and would swamp the key index.
    let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    keys.extend(wiki.counts.iter().filter(|(_, &c)| c >= args.min_wiki).map(|(k, _)| k.clone()));
    keys.extend(subs.counts.keys().cloned());
    keys.extend(chat.counts.keys().cloned());

    // In first-seen order, not hash order. The Python walks its romans dict, whose order is
    // Dakshina then Aksharantar then BanglaTLit, and the surface tie-break below follows it; a
    // HashMap walk would pick a different spelling on every build.
    let mut ordered: Vec<RomanEntry<'_>> = romans.map.iter().collect();
    ordered.sort_by_key(|(_, v)| v.2);

    let mut rom_surface: HashMap<String, HashMap<String, (u64, u32)>> = HashMap::new();
    let mut rom_next: u32 = 0;
    for ((_r, w), (c, src, _ord)) in &ordered {
        let k = match_key(w);
        let order = rom_next;
        let slot = rom_surface
            .entry(k.clone())
            .or_default()
            .entry(w.clone())
            .or_insert((0, order));
        if slot.1 == order {
            rom_next += 1;
        }
        slot.0 += c;
        if src & (SRC_DAKSHINA | SRC_BANGLATLIT) != 0 {
            keys.insert(k);
        }
    }
    log(&format!(
        "lexicon size: {} (words with romanizations: {})",
        keys.len(),
        rom_surface.len()
    ));

    let surface = |k: &str| -> String {
        for sc in [&chat, &subs, &wiki] {
            if sc.counts.contains_key(k) {
                if let Some(s) = sc.best_surface(k) {
                    return s;
                }
            }
        }
        rom_surface.get(k).and_then(best_of).unwrap_or_else(|| k.to_string())
    };
    let surfaces: HashMap<&String, String> = keys.iter().map(|k| (k, surface(k))).collect();

    // unigrams: word -> (wiki, subs, chat)
    let mut uni_entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(keys.len());
    for k in &keys {
        let mut rec = Vec::with_capacity(12);
        for c in [
            wiki.counts.get(k).copied().unwrap_or(0),
            subs.counts.get(k).copied().unwrap_or(0),
            chat.counts.get(k).copied().unwrap_or(0),
        ] {
            rec.extend_from_slice(&(c.min(u32::MAX as u64) as u32).to_le_bytes());
        }
        uni_entries.push((surfaces[k].clone(), rec));
    }
    write_table(&args.out.join("unigrams.lkx"), uni_entries, 12, None)?;
    log(&format!("unigrams.lkx: {} entries", keys.len()));

    // romans: "roman\tword" -> (count, source).
    //
    // The rank -- the order a prefix scan yields these in -- is by attestation count, highest
    // first, then by word. It decides how the ranker breaks exact score ties and which candidates
    // are cheap enough to send to the model. Measured on dakshina-dev, two entirely different
    // orders (first-seen and count-descending) produced identical accuracy to two decimals, so the
    // order is not load-bearing; count order is used because it is deterministic and defensible,
    // where marisa's was an artifact of trie layout. See docs/DECISIONS.md.
    let mut by_rank: Vec<usize> = (0..ordered.len()).collect();
    by_rank.sort_by(|&a, &b| {
        let (ka, va) = ordered[a];
        let (kb, vb) = ordered[b];
        ka.0.as_bytes()
            .cmp(kb.0.as_bytes())
            .then_with(|| vb.0.cmp(&va.0))
            .then_with(|| ka.1.as_bytes().cmp(kb.1.as_bytes()))
    });
    let mut rank_of = vec![0u32; ordered.len()];
    for (rank, &i) in by_rank.iter().enumerate() {
        rank_of[i] = rank as u32;
    }

    let mut rom_items: Vec<(String, u64, i8)> = Vec::with_capacity(ordered.len());
    let mut rom_entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(ordered.len());
    let mut rom_ranks: Vec<u32> = Vec::with_capacity(ordered.len());
    for (i, ((r, w), (c, src, _))) in ordered.iter().enumerate() {
        let k = match_key(w);
        let word = surfaces.get(&k).cloned().unwrap_or_else(|| w.clone());
        let key = format!("{r}\t{word}");
        let mut rec = rec_u32(*c);
        rec.push(*src as u8);
        rom_entries.push((key.clone(), rec));
        rom_ranks.push(rank_of[i]);
        rom_items.push((key, (*c).min(I32_MAX), *src));
    }
    let n_rom = rom_entries.len();
    write_table(&args.out.join("romans.lkx"), rom_entries, 5, Some(rom_ranks))?;
    log(&format!("romans.lkx: {n_rom} entries"));

    // keys: "<level>:<key>\tword" -> (score)
    let mut key_items: Vec<(String, u64)> = Vec::new();
    for k in &keys {
        let w = &surfaces[k];
        let score = wiki.counts.get(k).copied().unwrap_or(0)
            + 3 * subs.counts.get(k).copied().unwrap_or(0)
            + 20 * chat.counts.get(k).copied().unwrap_or(0);
        for level in [Level::Fine, Level::Coarse] {
            let pk = key_from_bangla(w, level);
            if !pk.is_empty() {
                key_items.push((format!("{}:{pk}\t{w}", level.tag()), score.min(I32_MAX)));
            }
        }
    }
    let key_entries: Vec<(String, Vec<u8>)> =
        key_items.iter().map(|(k, s)| (k.clone(), rec_u32(*s))).collect();
    let n_keys = key_entries.len();
    write_table(&args.out.join("keys.lkx"), key_entries, 4, None)?;
    log(&format!("keys.lkx: {n_keys} entries"));

    build_prefix_tables(&args.out, &rom_items, &key_items)?;

    build_bigrams(args, &chat_rows, &surfaces, &keys)?;

    // The tuned weights, carried forward. Their presence was verified before any work began.
    std::fs::copy(&args.weights, args.out.join("weights.json"))?;
    log(&format!("weights.json: copied from {}", args.weights.display()));

    let meta = serde_json::json!({
        "built_by": "likhi-lexicon (Rust)",
        "words": keys.len(),
        "romanizations": n_rom,
        "phonetic_keys": n_keys,
        "wiki_types": wiki.counts.len(),
        "subs_types": subs.counts.len(),
        "chat_types": chat.counts.len(),
        "chat_sentences": chat_rows.len(),
        "seconds": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
    });
    std::fs::write(args.out.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;

    log(&format!("done in {:.1}s", started.elapsed().as_secs_f64()));
    Ok(())
}

/// A pruned word-bigram table for context-aware ranking.
///
/// Dakshina's Wikipedia sentences at weight 1 and the BanglaTLit chat sentences at weight 5 -- the
/// register Likhi targets. An unknown word breaks the chain rather than being skipped over, so a
/// bigram never spans a word the lexicon does not have.
fn build_bigrams(
    args: &Args,
    chat_rows: &[ChatRow],
    surfaces: &HashMap<&String, String>,
    keys: &std::collections::HashSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    const START: &str = "<s>";
    let surf: HashMap<&str, &str> =
        keys.iter().map(|k| (k.as_str(), surfaces[k].as_str())).collect();

    // Counts carry first-seen order, for the same reason as the prefix tables: only the top 60 per
    // previous word survive, ties at low counts are everywhere, and `Counter.most_common` resolves
    // them by insertion order.
    let mut counts: HashMap<String, HashMap<String, (u64, u32)>> = HashMap::new();
    let mut totals: HashMap<String, u64> = HashMap::new();
    let mut n_sent = 0usize;
    let mut next: u32 = 0;

    let consume = |text: &str,
                   weight: u64,
                   counts: &mut HashMap<String, HashMap<String, (u64, u32)>>,
                   totals: &mut HashMap<String, u64>,
                   next: &mut u32| {
        let mut prev: Option<String> = Some(START.to_string());
        for tok in tokens(text) {
            match surf.get(match_key(&tok).as_str()) {
                None => prev = None, // an unknown word breaks the chain
                Some(w) => {
                    if let Some(p) = &prev {
                        let order = *next;
                        let slot = counts
                            .entry(p.clone())
                            .or_default()
                            .entry((*w).to_string())
                            .or_insert((0, order));
                        if slot.1 == order {
                            *next += 1;
                        }
                        slot.0 += weight;
                        *totals.entry(p.clone()).or_insert(0) += weight;
                    }
                    prev = Some((*w).to_string());
                }
            }
        }
    };

    let path = args
        .raw
        .join("dakshina/bn/native_script_wikipedia/bn.wiki-filt.train.text.shuf.txt.gz");
    for line in open_lines(&path)?.lines() {
        let line = line?;
        consume(&line, 1, &mut counts, &mut totals, &mut next);
        n_sent += 1;
        if args.wiki_lines.is_some_and(|l| n_sent >= l) {
            break;
        }
        if n_sent.is_multiple_of(200_000) {
            let pairs: usize = counts.values().map(|c| c.len()).sum();
            log(&format!("bigrams: {n_sent} sentences, {pairs} pairs"));
        }
    }
    for row in chat_rows {
        consume(&row.bengali, 5, &mut counts, &mut totals, &mut next);
        n_sent += 1;
    }

    let bigram_types: usize = counts.values().map(|c| c.len()).sum();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for (prev, c) in &counts {
        let mut ranked: Vec<(&String, &(u64, u32))> = c.iter().collect();
        ranked.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.1 .1.cmp(&b.1 .1)));
        for (w, (n, _)) in ranked.into_iter().take(args.bigram_per_prev) {
            if *n < args.bigram_min_count {
                break;
            }
            entries.push((format!("{prev}\t{w}"), rec_u32(*n)));
        }
    }
    let kept = entries.len();
    write_table(&args.out.join("bigrams.lkx"), entries, 4, None)?;

    let total_entries: Vec<(String, Vec<u8>)> =
        totals.iter().map(|(p, n)| (p.clone(), rec_u32(*n))).collect();
    let prev_types = total_entries.len();
    write_table(&args.out.join("bigram_totals.lkx"), total_entries, 4, None)?;

    log(&format!(
        "bigrams: {n_sent} sentences, kept {kept} of {bigram_types} pairs, {prev_types} previous words"
    ));
    Ok(())
}

fn main() {
    let mut args = Args {
        raw: PathBuf::from("data/raw"),
        out: PathBuf::from("models/rust/lexicon"),
        weights: default_weights(),
        wiki_lines: None,
        min_wiki: 2,
        bigram_min_count: 2,
        bigram_per_prev: 60,
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--raw" => {
                args.raw = PathBuf::from(next.expect("--raw needs a path"));
                i += 2;
            }
            "--out" => {
                args.out = PathBuf::from(next.expect("--out needs a path"));
                i += 2;
            }
            "--weights" => {
                args.weights = PathBuf::from(next.expect("--weights needs a path"));
                i += 2;
            }
            "--wiki-lines" => {
                args.wiki_lines = next.and_then(|v| v.parse().ok());
                i += 2;
            }
            "--min-wiki" => {
                args.min_wiki = next.and_then(|v| v.parse().ok()).unwrap_or(2);
                i += 2;
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if let Err(e) = build(&args) {
        eprintln!("[lexicon] failed: {e}");
        std::process::exit(1);
    }
}

