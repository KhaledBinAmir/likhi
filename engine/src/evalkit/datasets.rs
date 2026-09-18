//! Loaders for the evaluation word sets. A port of the word-set half of
//! `src/likhi/eval/datasets.py`.
//!
//! Two views of the same data, and the distinction matters when comparing numbers:
//!
//! * **pairs** -- one item per attested (roman, native) pair, unweighted. This is how published
//!   top-1 numbers are computed and the one to compare against papers.
//! * **grouped** (`name+grouped`) -- one item per distinct roman string, every attested native
//!   accepted as gold, weighted by attestation total. Closer to what a typist experiences.
//!
//! Item order is file order, and for grouped sets it is the order each roman was first seen. That
//! is not cosmetic: the sample-miss list is "the first N misses", so a different order reports
//! different examples, and a reader comparing two runs would think the engine had changed.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct WordItem {
    pub roman: String,
    pub golds: Vec<String>,
    pub weight: f64,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct WordSet {
    pub name: String,
    pub items: Vec<WordItem>,
}

/// One row as the loaders produce it, before grouping.
struct Row {
    roman: String,
    native: String,
    weight: f64,
    source: String,
}

fn open_lines(path: &Path) -> std::io::Result<Box<dyn BufRead>> {
    let file = std::fs::File::open(path)?;
    if path.extension().is_some_and(|e| e == "gz") {
        Ok(Box::new(BufReader::new(flate2::read::GzDecoder::new(file))))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

/// Build a `WordSet` from rows, mirroring the Python's `_group`.
///
/// Grouped: golds are ordered by descending accumulated weight, and Python's `sorted` is stable, so
/// equal weights keep the order the golds were first seen. Both orderings are reproduced with
/// explicit first-seen indices, because a `HashMap` has neither.
fn group(rows: Vec<Row>, name: &str, grouped: bool) -> WordSet {
    if !grouped {
        return WordSet {
            name: name.to_string(),
            items: rows
                .into_iter()
                .map(|r| WordItem {
                    roman: r.roman,
                    golds: vec![r.native],
                    weight: r.weight,
                    source: r.source,
                })
                .collect(),
        };
    }

    struct Group {
        order: usize,
        source: String,
        golds: HashMap<String, (f64, usize)>,
        next: usize,
    }
    let mut groups: HashMap<String, Group> = HashMap::new();
    let mut next_group = 0usize;
    for r in rows {
        let g = groups.entry(r.roman.clone()).or_insert_with(|| {
            let order = next_group;
            next_group += 1;
            Group { order, source: r.source.clone(), golds: HashMap::new(), next: 0 }
        });
        let seen = g.next;
        let slot = g.golds.entry(r.native).or_insert((0.0, seen));
        if slot.1 == seen {
            g.next += 1;
        }
        slot.0 += r.weight;
    }

    let mut ordered: Vec<(String, Group)> = groups.into_iter().collect();
    ordered.sort_by_key(|(_, g)| g.order);
    WordSet {
        name: format!("{name}+grouped"),
        items: ordered
            .into_iter()
            .map(|(roman, g)| {
                let mut golds: Vec<(&String, &(f64, usize))> = g.golds.iter().collect();
                // Descending weight; ties keep first-seen order, as a stable sort would.
                golds.sort_by(|a, b| {
                    b.1 .0
                        .partial_cmp(&a.1 .0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.1 .1.cmp(&b.1 .1))
                });
                let weight = g.golds.values().map(|(w, _)| *w).sum();
                WordItem {
                    roman,
                    golds: golds.into_iter().map(|(g, _)| g.clone()).collect(),
                    weight,
                    source: g.source,
                }
            })
            .collect(),
    }
}

fn strip_punct(tok: &str) -> &str {
    let is_word = |c: char| {
        c.is_alphanumeric()
            || c == '_'
            || matches!(c, '\u{0980}'..='\u{09FF}' | '\u{200C}' | '\u{200D}')
    };
    let start = tok.find(is_word).unwrap_or(tok.len());
    let end = tok
        .rfind(is_word)
        .map(|i| i + tok[i..].chars().next().map_or(1, char::len_utf8));
    &tok[start..end.unwrap_or(start)]
}

fn is_latin_only(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic() || c == '\'')
}

// ------------------------------------------------------------------------------------- Dakshina

fn dakshina_lexicon(raw: &Path, split: &str) -> std::io::Result<Vec<Row>> {
    let path = raw.join(format!("dakshina/bn/lexicons/bn.translit.sampled.{split}.tsv"));
    let mut out = Vec::new();
    for line in open_lines(&path)?.lines() {
        let line = line?;
        let parts: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let Ok(count) = parts[2].trim().parse::<f64>() else { continue };
        out.push(Row {
            roman: crate::textnorm::normalize_roman(parts[1]),
            native: crate::textnorm::canonical(parts[0]),
            weight: count,
            source: "dakshina".into(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------- Aksharantar

fn aksharantar(raw: &Path, split: &str, sources: Option<&[String]>) -> std::io::Result<Vec<Row>> {
    #[derive(serde::Deserialize)]
    struct AkRow {
        #[serde(rename = "native word")]
        native: String,
        #[serde(rename = "english word")]
        english: String,
        #[serde(default)]
        source: Option<String>,
    }
    let path = raw.join(format!("aksharantar/ben_{split}.json"));
    let mut out = Vec::new();
    let mut bad = 0usize;
    for line in open_lines(&path)?.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<AkRow>(line) {
            Ok(row) => {
                let source = row.source.unwrap_or_else(|| "aksharantar".into());
                if sources.is_some_and(|s| !s.iter().any(|f| f == &source)) {
                    continue;
                }
                out.push(Row {
                    roman: crate::textnorm::normalize_roman(&row.english),
                    native: crate::textnorm::canonical(&row.native),
                    weight: 1.0,
                    source,
                });
            }
            Err(_) => bad += 1,
        }
    }
    if bad > 0 {
        eprintln!("[eval] warning: {bad} unreadable aksharantar lines");
    }
    Ok(out)
}

// ----------------------------------------------------------------------------------- BanglaTLit

/// Chat-style word pairs aligned positionally from BanglaTLit sentences.
///
/// Only sentences whose two sides have the same token count are used. Noisy, but it is the only
/// public data in the Bangladeshi chat register.
fn banglatlit_word_pairs(raw: &Path, split: &str) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    let mut rdr = csv::Reader::from_path(raw.join(format!("banglatlit/{split}.csv")))?;
    for rec in rdr.deserialize::<HashMap<String, String>>() {
        let row = rec?;
        let bn = row.get("text_bengali").map(|s| s.trim()).unwrap_or("");
        let rm = row.get("text_transliterated").map(|s| s.trim()).unwrap_or("");
        if bn.is_empty() || rm.is_empty() {
            continue;
        }
        let gold = crate::textnorm::canonical(bn);
        let r_toks: Vec<&str> = rm.split_whitespace().map(strip_punct).collect();
        let b_toks: Vec<&str> = gold.split_whitespace().map(strip_punct).collect();
        if r_toks.len() != b_toks.len() {
            continue;
        }
        for (r, b) in r_toks.iter().zip(b_toks.iter()) {
            if r.is_empty() || b.is_empty() || !is_latin_only(r) || !crate::textnorm::has_bengali(b)
            {
                continue;
            }
            if crate::textnorm::has_bengali(r) || b.chars().any(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            out.push(Row {
                roman: crate::textnorm::normalize_roman(r),
                native: (*b).to_string(),
                weight: 1.0,
                source: "banglatlit".into(),
            });
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------------- feedback words

fn feedback_words(repo: &Path) -> std::io::Result<Vec<Row>> {
    let path = repo.join("data/feedback/words.jsonl");
    let mut out = Vec::new();
    if !path.exists() {
        return Ok(out);
    }
    for line in open_lines(&path)?.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(roman) = v.get("roman").and_then(|x| x.as_str()) else { continue };
        let golds: Vec<String> = match v.get("gold") {
            Some(serde_json::Value::Array(a)) => {
                a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()
            }
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            _ => continue,
        };
        for g in golds {
            out.push(Row {
                roman: crate::textnorm::normalize_roman(roman),
                native: crate::textnorm::canonical(&g),
                weight: 1.0,
                source: "feedback".into(),
            });
        }
    }
    Ok(out)
}

/// Resolve names like `dakshina-test`, `dakshina-dev+grouped`, `aksharantar-test:AK-Freq`,
/// `banglatlit-test-words`, `feedback-words`.
pub fn load_wordset(repo: &Path, name: &str) -> Result<WordSet, Box<dyn std::error::Error>> {
    let raw = repo.join("data/raw");
    let grouped = name.ends_with("+grouped");
    let base = name.trim_end_matches("+grouped");
    let (base, filter) = match base.split_once(':') {
        Some((b, f)) => (b, Some(f.split(',').map(str::to_string).collect::<Vec<_>>())),
        None => (base, None),
    };

    if let Some(split) = base.strip_prefix("dakshina-") {
        return Ok(group(dakshina_lexicon(&raw, split)?, &format!("dakshina-{split}"), grouped));
    }
    if let Some(split) = base.strip_prefix("aksharantar-") {
        let rows = aksharantar(&raw, split, filter.as_deref())?;
        return Ok(group(rows, &format!("aksharantar-{split}"), grouped));
    }
    if base.starts_with("banglatlit-") && base.ends_with("-words") {
        let split = base.split('-').nth(1).unwrap_or("test");
        // Always grouped, as the Python does: the alignment is noisy enough that every attested
        // spelling of a roman token deserves to count.
        let rows = banglatlit_word_pairs(&raw, split)?;
        return Ok(group(rows, &format!("banglatlit-{split}-words"), true));
    }
    if base == "feedback-words" {
        return Ok(group(feedback_words(repo)?, "feedback-words", grouped));
    }
    Err(format!("unknown word set: {name}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(roman: &str, native: &str, weight: f64) -> Row {
        Row {
            roman: roman.into(),
            native: native.into(),
            weight,
            source: "t".into(),
        }
    }

    #[test]
    fn ungrouped_keeps_one_item_per_pair_in_file_order() {
        let ws = group(
            vec![row("a", "X", 2.0), row("b", "Y", 1.0), row("a", "Z", 3.0)],
            "t",
            false,
        );
        assert_eq!(ws.name, "t");
        assert_eq!(ws.items.len(), 3);
        assert_eq!(ws.items[0].roman, "a");
        assert_eq!(ws.items[0].golds, vec!["X".to_string()]);
        assert_eq!(ws.items[2].golds, vec!["Z".to_string()]);
    }

    #[test]
    fn grouped_orders_golds_by_weight_and_items_by_first_appearance() {
        let ws = group(
            vec![
                row("a", "X", 2.0),
                row("b", "Y", 1.0),
                row("a", "Z", 3.0),
                row("a", "X", 5.0),
            ],
            "t",
            true,
        );
        assert_eq!(ws.name, "t+grouped");
        assert_eq!(ws.items.len(), 2);
        assert_eq!(ws.items[0].roman, "a", "first-seen roman comes first");
        // X accumulated 7, Z has 3.
        assert_eq!(ws.items[0].golds, vec!["X".to_string(), "Z".to_string()]);
        assert_eq!(ws.items[0].weight, 10.0);
        assert_eq!(ws.items[1].roman, "b");
    }

    #[test]
    fn grouped_breaks_equal_weights_by_first_seen() {
        let ws = group(vec![row("a", "first", 1.0), row("a", "second", 1.0)], "t", true);
        assert_eq!(ws.items[0].golds, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn punctuation_is_stripped_only_at_the_edges() {
        assert_eq!(strip_punct(",hello."), "hello");
        assert_eq!(strip_punct("a.b"), "a.b");
        assert_eq!(strip_punct(",,,"), "");
        // A Bengali word must keep its final vowel sign, which is not a "word character" to a
        // naive \W strip.
        assert_eq!(strip_punct("কিন্তু,"), "কিন্তু");
    }
}
