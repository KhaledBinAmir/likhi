//! `likhi-nextword`: how good would next-word suggestions be, measured before any are built.
//!
//!     likhi-nextword [--k 5] [--csv data/raw/banglatlit/test.csv]
//!     likhi-nextword --audit [--min-share 0.2] [--list]
//!     likhi-nextword --letters [--max-letters 4] [--csv ...]
//!
//! `--letters` measures first-letter prediction: the next word's first few letters typed exactly as
//! the person romanized it, and the words that follow the previous word *and* fit those letters
//! placed into the ordinary candidate list. It reports how many keystrokes that would save and how
//! often it pushes the word the person wanted further down -- the trade the placement decides.
//!
//! `--audit` answers a different question: not how often the suggestion is right, but what it can
//! ever say. It walks every word in the table, keeps the ones whose top continuation clears the
//! threshold -- exactly the rule the engine applies before showing anything -- and lists what would
//! be shown, weighted by how often the word occurs. Precision on a test set cannot find a
//! suggestion that is embarrassing rather than wrong; this can.
//!
//! After a word is committed, a keyboard can offer the words that usually follow it. Likhi already
//! carries the table that would answer that -- the word-bigram table the ranker uses for context --
//! so the question is not whether it can be done but whether the answers would be good enough to
//! be worth the screen space and the keystrokes they claim.
//!
//! Measured on BanglaTLit's test split, which is chat register, the register pilots actually type
//! in, and which the bigram table was never built from: the builder reads BanglaTLit train rows only
//! and explicitly drops any that also appear in val or test. A measurement on sentences the table
//! had seen would report memory, not prediction.
//!
//! Tokenised exactly as the builder tokenises, and an unknown word breaks the chain exactly as it
//! does there, so every figure below describes the table as the engine would actually use it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use likhi_engine::core::NEVER_SUGGESTED_NEXT;
use likhi_engine::lexicon::{rec_u32, Table};
use likhi_engine::textnorm::{canonical, match_key};

const START: &str = "<s>";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

// The builder's tokenizer, reproduced rather than approximated. A different split would measure a
// different table from the one the engine loads.
fn is_bengali_token_char(c: char) -> bool {
    matches!(c, '\u{0980}'..='\u{09FF}' | '\u{200C}' | '\u{200D}')
}

fn is_all_digits_or_marks(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            matches!(c,
                '\u{09E6}'..='\u{09EF}'
                | '\u{0981}'..='\u{0983}'
                | '\u{09BC}'
                | '\u{09BE}'..='\u{09CD}'
                | '\u{09D7}'
                | '\u{200C}' | '\u{200D}')
        })
}

fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if is_bengali_token_char(c) {
            current.push(c);
        } else if !current.is_empty() {
            if !is_all_digits_or_marks(&current) {
                out.push(canonical(&current));
            }
            current.clear();
        }
    }
    out
}

/// What this person has typed after each word so far, learned as they go. Counts only; nothing
/// leaves the machine in the real thing, and nothing here is persisted.
#[derive(Default)]
struct Personal {
    after: HashMap<String, HashMap<String, u32>>,
}

impl Personal {
    fn learn(&mut self, prev: &str, word: &str) {
        *self
            .after
            .entry(prev.to_string())
            .or_default()
            .entry(word.to_string())
            .or_insert(0) += 1;
    }

    /// Blend what this person usually types next with what people in general type next.
    ///
    /// Probabilities, not raw counts, so the global table's millions do not drown a handful of
    /// personal observations: a word this person has typed after `prev` twice out of three times
    /// should outrank one the corpus saw a few thousand times out of a hundred thousand.
    /// `prev_surface` finds the global continuations; `prev_key` finds this person's. Everything is
    /// merged and returned as match keys, so a spelling variant is one candidate, not two.
    fn blend(
        &self,
        bigrams: &Table,
        totals: &Table,
        prev_surface: Option<&str>,
        prev_key: &str,
        k: usize,
        weight: f64,
    ) -> Vec<String> {
        let mut score: HashMap<String, f64> = HashMap::new();
        if let Some(prev) = prev_surface {
            if let Some(tot) = totals.get(prev) {
                let total = rec_u32(tot).max(1) as f64;
                for (key, rec) in bigrams.prefix_iter(&format!("{prev}\t")) {
                    if let Some((_, w)) = key.split_once('\t') {
                        *score.entry(match_key(w)).or_insert(0.0) += rec_u32(rec) as f64 / total;
                    }
                }
            }
        }
        if let Some(mine) = self.after.get(prev_key) {
            let total = mine.values().sum::<u32>().max(1) as f64;
            for (w, n) in mine {
                *score.entry(w.clone()).or_insert(0.0) += weight * (*n as f64) / total;
            }
        }
        let mut ranked: Vec<(f64, String)> = score.into_iter().map(|(w, s)| (s, w)).collect();
        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.cmp(&b.1)));
        ranked.into_iter().take(k).map(|(_, w)| w).collect()
    }
}

/// How sure the table is about its first suggestion after `prev`: that word's share of everything
/// seen after `prev`. The basis for showing a suggestion only when it is likely to be right.
fn top_share(bigrams: &Table, totals: &Table, prev: &str) -> f64 {
    let Some(tot) = totals.get(prev) else { return 0.0 };
    let total = rec_u32(tot).max(1) as f64;
    let best = bigrams
        .prefix_iter(&format!("{prev}\t"))
        .into_iter()
        .map(|(_, rec)| rec_u32(rec))
        .max()
        .unwrap_or(0);
    best as f64 / total
}

/// The words that follow `prev`, most frequent first.
fn continuations(bigrams: &Table, prev: &str, k: usize) -> Vec<String> {
    let prefix = format!("{prev}\t");
    let mut found: Vec<(u32, String)> = bigrams
        .prefix_iter(&prefix)
        .into_iter()
        .filter_map(|(key, rec)| {
            key.split_once('\t')
                .map(|(_, w)| (rec_u32(rec), w.to_string()))
        })
        .collect();
    // Count first, then the word, so the order is fully determined and two runs agree.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().take(k).map(|(_, w)| w).collect()
}

#[derive(Default)]
struct Tally {
    /// Word positions considered.
    positions: usize,
    /// Positions where the previous word had any bigram data, so a prediction was possible.
    covered: usize,
    /// hits[i]: the actual next word was at rank i+1.
    hits: Vec<usize>,
}

impl Tally {
    fn new(k: usize) -> Tally {
        Tally { hits: vec![0; k], ..Default::default() }
    }

    fn record(&mut self, rank: Option<usize>, covered: bool) {
        self.positions += 1;
        if covered {
            self.covered += 1;
        }
        if let Some(r) = rank {
            self.hits[r] += 1;
        }
    }

    fn within(&self, k: usize) -> usize {
        self.hits.iter().take(k).sum()
    }

    fn report(&self, title: &str, k: usize) {
        let pct = |n: usize, d: usize| 100.0 * n as f64 / d.max(1) as f64;
        println!("\n{title}");
        println!("  positions {:>6}   with data for the previous word {:>6} ({:.1}%)",
            self.positions, self.covered, pct(self.covered, self.positions));
        for n in [1, 3, k] {
            if n > k {
                continue;
            }
            println!(
                "  next word in top {n}: {:>5.1}% of all positions   {:>5.1}% where there was data",
                pct(self.within(n), self.positions),
                pct(self.within(n), self.covered)
            );
        }
    }
}

/// The list as it would be shown: `base` is the engine's ordinary candidates, and up to `n` of
/// `predicted` go in at position `start`, moved up if they were already further down. Nothing
/// before `start` is touched -- position 0 above all, because that is what Space commits.
fn merged(base: &[String], predicted: &[String], start: usize, n: usize, k: usize) -> Vec<String> {
    let head: Vec<String> = base.iter().take(start).cloned().collect();
    let fits = |w: &String| !head.iter().any(|h| match_key(h) == match_key(w));
    let extra: Vec<String> = predicted.iter().filter(|w| fits(w)).take(n).cloned().collect();
    let mut out = head;
    out.extend(extra.iter().cloned());
    for w in base.iter().skip(start) {
        if !extra.iter().any(|e| match_key(e) == match_key(w)) {
            out.push(w.clone());
        }
    }
    out.truncate(k);
    out
}

/// See `--letters` in the module comment.
fn letters(csv_path: &Path, max_letters: usize) {
    let models = std::env::var_os("LIKHI_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("models/rust"));
    let engine = match likhi_engine::core::Engine::open(
        &models,
        likhi_engine::core::EngineOptions { personal: None, ..Default::default() },
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot open the engine from {}: {e}", models.display());
            std::process::exit(1);
        }
    };
    let mut rdr = match csv::Reader::from_path(csv_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot read {}: {e}", csv_path.display());
            std::process::exit(1);
        }
    };
    let headers = rdr.headers().cloned().unwrap_or_default();
    let col = |needle: &str| headers.iter().position(|h| h.to_ascii_lowercase().contains(needle));
    let (Some(bn_col), Some(rom_col)) = (col("bengali"), col("translit")) else {
        eprintln!("need a Bengali and a transliterated column (headers: {headers:?})");
        std::process::exit(1);
    };

    const K: usize = 5;
    // (label, insert at, how many)
    let rules: [(&str, usize, usize); 5] = [
        ("today (no prediction)", 0, 0),
        ("after 1st, up to 1", 1, 1),
        ("after 1st, up to 2", 1, 2),
        ("after 2nd, up to 1", 2, 1),
        ("after 2nd, up to 2", 2, 2),
    ];
    // Per rule: [letters][position] -> count; keystrokes needed; displaced at full length.
    let mut found = vec![vec![vec![0usize; K + 1]; max_letters + 1]; rules.len()];
    let mut keys_needed = vec![0usize; rules.len()];
    let mut displaced = vec![0usize; rules.len()];
    let mut keys_plain = 0usize;
    let (mut words, mut sentences, mut skipped) = (0usize, 0usize, 0usize);
    let mut offered = vec![0usize; max_letters + 1];
    let mut offered_right = vec![0usize; max_letters + 1];
    let mut tried = vec![0usize; max_letters + 1];

    for row in rdr.records().flatten() {
        let (Some(bn_text), Some(rom_text)) = (row.get(bn_col), row.get(rom_col)) else { continue };
        let bn = tokens(bn_text);
        let rom: Vec<String> = rom_text
            .split_whitespace()
            .map(|t| t.chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        // Word-for-word only: a sentence whose two sides do not split into the same number of
        // words cannot say which romanization belongs to which Bangla word.
        if bn.len() != rom.len() || bn.len() < 2 {
            skipped += 1;
            continue;
        }
        sentences += 1;
        for i in 1..bn.len() {
            let prev = &bn[i - 1];
            let want = match_key(&bn[i]);
            let r = &rom[i];
            let n = r.chars().count();
            if n < 2 {
                continue;
            }
            words += 1;
            keys_plain += n + 1;
            let context = vec![prev.clone()];
            let mut earliest: Vec<Option<(usize, usize)>> = vec![None; rules.len()];
            for len in 1..=max_letters.min(n) {
                let prefix: String = r.chars().take(len).collect();
                let (base, _) = engine.fast_suggest(&prefix, &context, K);
                let predicted = engine.next_completions(prev, &prefix, 2);
                tried[len] += 1;
                if !predicted.is_empty() {
                    offered[len] += 1;
                    if predicted.iter().any(|w| match_key(w) == want) {
                        offered_right[len] += 1;
                    }
                }
                let at_base = base.iter().position(|w| match_key(w) == want);
                for (ri, (_, start, count)) in rules.iter().enumerate() {
                    let shown = merged(&base, &predicted, *start, *count, K);
                    let at = shown.iter().position(|w| match_key(w) == want);
                    found[ri][len][at.unwrap_or(K)] += 1;
                    if len < n && earliest[ri].is_none() {
                        if let Some(p) = at {
                            earliest[ri] = Some((len, p));
                        }
                    }
                    // The whole word typed: is the word the person wanted now further away than
                    // the ordinary list had it?
                    if len == n {
                        let worse = match (at_base, at) {
                            (Some(b), Some(a)) => a > b,
                            (Some(_), None) => true,
                            _ => false,
                        };
                        if worse {
                            displaced[ri] += 1;
                        }
                    }
                }
            }
            for (ri, e) in earliest.iter().enumerate() {
                // Picked the first time it is on screen: Space when it is first (the space comes
                // with it), otherwise its number and then a space. Never picked: typed in full.
                keys_needed[ri] += match e {
                    Some((len, 0)) => len + 1,
                    Some((len, _)) => (len + 2).min(n + 1),
                    None => n + 1,
                };
            }
        }
    }

    println!(
        "first-letter prediction on {} ({} sentences word-for-word, {} skipped; {} words of 2+ letters)",
        csv_path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
        sentences,
        skipped,
        words
    );
    println!("\nhow often the prediction list has the word, by letters typed");
    for len in 1..=max_letters {
        println!(
            "  {len} letter(s): something offered {:>5.1}% of the time; the word among the 2 offered {:>5.1}% of those",
            100.0 * offered[len] as f64 / tried[len].max(1) as f64,
            100.0 * offered_right[len] as f64 / offered[len].max(1) as f64
        );
    }
    println!("\nkeystrokes, if every word is picked the moment it is on screen (an upper bound)");
    for (ri, (label, _, _)) in rules.iter().enumerate() {
        println!(
            "  {:<24} {:>5.1}% fewer keystrokes   full word pushed down {:>4} times",
            label,
            100.0 * (keys_plain - keys_needed[ri]) as f64 / keys_plain.max(1) as f64,
            displaced[ri]
        );
    }
    println!("\nwhere the word is, after 1 / 2 / 3 letters (position 1 is what Space takes)");
    for (ri, (label, _, _)) in rules.iter().enumerate() {
        let mut line = format!("  {label:<24}");
        for (len, row) in found[ri].iter().enumerate().skip(1).take(max_letters.min(3)) {
            let total: usize = row.iter().sum();
            let top1 = row[0];
            let top5: usize = row[..K].iter().sum();
            line.push_str(&format!(
                "   {len}: first {:>4.1}% on screen {:>4.1}%",
                100.0 * top1 as f64 / total.max(1) as f64,
                100.0 * top5 as f64 / total.max(1) as f64
            ));
        }
        println!("{line}");
    }
}

/// Everything the engine could show, and how often. See the module comment. `list` also prints
/// every pair, one per line, for searching.
fn audit(bigrams: &Table, totals: &Table, min_share: f64, list: bool) {
    // (word before, word shown, its share, how often the word before occurs)
    let mut shown: Vec<(String, String, f64, u32)> = Vec::new();
    let mut considered = 0usize;
    for (prev, rec) in totals.iter_all() {
        considered += 1;
        let total = rec_u32(rec).max(1);
        // The engine's own exclusions apply, so this lists what it would actually show.
        let best = bigrams
            .prefix_iter(&format!("{prev}\t"))
            .into_iter()
            .filter_map(|(key, rec)| key.split_once('\t').map(|(_, w)| (rec_u32(rec), w.to_string())))
            .filter(|(_, w)| !NEVER_SUGGESTED_NEXT.contains(&w.as_str()))
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        if let Some((count, word)) = best {
            let share = count as f64 / total as f64;
            if share >= min_share {
                shown.push((prev, word, share, total));
            }
        }
    }
    let occurrences: u64 = shown.iter().map(|s| u64::from(s.3)).sum();
    println!(
        "{} of {} words in the table lead to a suggestion at share >= {min_share}",
        shown.len(),
        considered
    );

    // What the strip says most, counted by how often it would say it.
    let mut by_word: HashMap<&str, (u64, usize)> = HashMap::new();
    for (_, w, _, n) in &shown {
        let e = by_word.entry(w.as_str()).or_insert((0, 0));
        e.0 += u64::from(*n);
        e.1 += 1;
    }
    let mut words: Vec<(&str, (u64, usize))> = by_word.into_iter().collect();
    words.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(b.0)));
    println!("\nthe words it would suggest most (share of all times a suggestion appears; after how many words)");
    for (w, (n, after)) in words.iter().take(40) {
        println!("  {:<20} {:>5.1}%  after {after} words", w, 100.0 * *n as f64 / occurrences.max(1) as f64);
    }

    // The contexts people are most likely to meet, with what they would be offered.
    shown.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
    println!("\nthe most common words that get a suggestion, and the suggestion");
    let limit = if list { shown.len() } else { 60 };
    for (prev, w, share, n) in shown.iter().take(limit) {
        println!("  {:<18} -> {:<16} share {:.2}  seen {n}", prev, w, share);
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let mut k = 5usize;
    let mut csv_path = repo().join("data/raw/banglatlit/test.csv");
    let mut audit_share: Option<f64> = None;
    let mut audit_list = false;
    let mut letters_max: Option<usize> = None;
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--k" => {
                k = argv.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(5).max(1);
                i += 2;
            }
            "--csv" => {
                if let Some(p) = argv.get(i + 1) {
                    csv_path = PathBuf::from(p);
                }
                i += 2;
            }
            "--audit" => {
                audit_share = Some(audit_share.unwrap_or(0.2));
                i += 1;
            }
            "--min-share" => {
                audit_share = argv.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--list" => {
                audit_list = true;
                audit_share = Some(audit_share.unwrap_or(0.2));
                i += 1;
            }
            "--letters" => {
                letters_max = Some(letters_max.unwrap_or(4));
                i += 1;
            }
            "--max-letters" => {
                letters_max = argv.get(i + 1).and_then(|v| v.parse().ok()).map(|n: usize| n.clamp(1, 8));
                i += 2;
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    if let Some(max_letters) = letters_max {
        letters(&csv_path, max_letters);
        return;
    }

    let lex = std::env::var_os("LIKHI_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("models/rust"))
        .join("lexicon");
    let bigrams = match Table::open(&lex.join("bigrams.lkx")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot open bigrams.lkx: {e:?}");
            std::process::exit(1);
        }
    };
    let totals = match Table::open(&lex.join("bigram_totals.lkx")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot open bigram_totals.lkx: {e:?}");
            std::process::exit(1);
        }
    };

    if let Some(min_share) = audit_share {
        audit(&bigrams, &totals, min_share, audit_list);
        return;
    }

    // Every previous word the table knows, keyed the way the builder keyed them. A test token is
    // found through its match key, so a nukta or joiner difference does not count as a miss.
    let surface: HashMap<String, String> = totals
        .iter_all()
        .into_iter()
        .map(|(w, _)| (match_key(&w), w))
        .collect();

    let mut rdr = match csv::Reader::from_path(&csv_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot read {}: {e}", csv_path.display());
            std::process::exit(1);
        }
    };
    let headers = rdr.headers().cloned().unwrap_or_default();
    // BanglaTLit names it `text_bengali`. Matched by substring, and a missing column is an error
    // rather than a guess: falling back to a fixed index picked the romanized column on the first
    // try, which has no Bengali tokens at all and would have measured an empty set as a result.
    let Some(bengali_col) = headers
        .iter()
        .position(|h| h.to_ascii_lowercase().contains("bengali"))
    else {
        eprintln!("no Bengali column in {} (headers: {:?})", csv_path.display(), headers);
        std::process::exit(1);
    };

    let mut first = Tally::new(k);
    let mut after = Tally::new(k);
    // The same positions, with a keyboard that learns from everything typed before them.
    let mut adapted = Tally::new(k);
    let mut personal = Personal::default();
    let mut sentences = 0usize;
    let mut examples: Vec<String> = Vec::new();
    // (top share, rank of the typed word among the offers) for every mid-sentence position that
    // had data, so precision can be read off at any confidence threshold afterwards.
    let mut gated: Vec<(f64, Option<usize>)> = Vec::new();

    for row in rdr.records().flatten() {
        let Some(text) = row.get(bengali_col) else { continue };
        let toks = tokens(text);
        if toks.is_empty() {
            continue;
        }
        sentences += 1;
        let mut prev: Option<String> = Some(START.to_string());
        let mut prev_key = START.to_string();
        for tok in &toks {
            let key = match_key(tok);
            let at_start = prev.as_deref() == Some(START);
            let tally = if at_start { &mut first } else { &mut after };
            match &prev {
                Some(p) => {
                    let covered = totals.get(p).is_some();
                    let options = if covered { continuations(&bigrams, p, k) } else { Vec::new() };
                    let rank = options.iter().position(|w| match_key(w) == key);
                    if examples.len() < 6 && p != START && rank.is_some() {
                        examples.push(format!("  after {p} -> offered {:?}, typed {tok}", options));
                    }
                    if covered && p != START {
                        gated.push((top_share(&bigrams, &totals, p), rank));
                    }
                    tally.record(rank, covered);
                }
                // The previous word was one the table does not know: nothing to predict from, the
                // same way the builder breaks the chain at an unknown word.
                None => tally.record(None, false),
            }
            if !at_start {
                // Predicted *before* learning this pair, so the keyboard never sees an answer ahead
                // of being asked for it. Learning afterwards is what a real keyboard would do.
                let options = personal.blend(&bigrams, &totals, prev.as_deref(), &prev_key, k, 3.0);
                let rank = options.iter().position(|w| *w == key);
                adapted.record(rank, !options.is_empty());
            }
            personal.learn(&prev_key, &key);
            prev = surface.get(&key).cloned();
            prev_key = key;
        }
    }

    println!("next-word prediction on {} ({} sentences, held out from the table)",
        csv_path.file_name().and_then(|n| n.to_str()).unwrap_or("?"), sentences);
    after.report("after a word, global table only (what the keyboard could offer on day one)", k);
    // Labelled for what it is. BanglaTLit is comments from thousands of different people, so this
    // learns from strangers, not from one typist, and says nothing about personal learning. What it
    // does show is that trusting learned counts too much can drown good general statistics.
    adapted.report(
        "after a word, learning from earlier sentences (MANY authors: not a test of personal learning)",
        k,
    );
    first.report("first word of a sentence", k);

    // Precision against how often a suggestion would appear, at several confidence thresholds. This
    // is the actual design decision: a strip that is usually wrong teaches people to ignore it.
    println!("\nshow suggestions only when the table is confident (mid-sentence positions)");
    println!("  {:>10} {:>12} {:>14} {:>14}", "threshold", "shown", "top-1 right", "top-3 right");
    let total = after.positions.max(1) as f64;
    for t in [0.0, 0.05, 0.1, 0.2, 0.3, 0.5] {
        let shown: Vec<&(f64, Option<usize>)> = gated.iter().filter(|(s, _)| *s >= t).collect();
        let n = shown.len().max(1) as f64;
        let top1 = shown.iter().filter(|(_, r)| *r == Some(0)).count() as f64;
        let top3 = shown.iter().filter(|(_, r)| r.is_some_and(|r| r < 3)).count() as f64;
        println!(
            "  {:>10.2} {:>11.1}% {:>13.1}% {:>13.1}%",
            t,
            100.0 * shown.len() as f64 / total,
            100.0 * top1 / n,
            100.0 * top3 / n
        );
    }
    if !examples.is_empty() {
        println!("\nsome correct predictions:");
        for e in examples {
            println!("{e}");
        }
    }
}
