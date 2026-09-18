//! Sentence sets, for the WER evaluation. A port of the sentence half of
//! `src/likhi/eval/datasets.py`.

use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct SentenceItem {
    pub roman: String,
    pub gold: String,
    pub source: String,
}

fn open_lines(path: &Path) -> std::io::Result<Box<dyn BufRead>> {
    let file = std::fs::File::open(path)?;
    if path.extension().is_some_and(|e| e == "gz") {
        Ok(Box::new(BufReader::new(flate2::read::GzDecoder::new(file))))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

/// Dakshina romanized Wikipedia sentences: `native \t roman`.
///
/// When the per-split file is absent the combined file is halved, dev first, as the Python does.
fn dakshina_sentences(raw: &Path, split: &str) -> std::io::Result<Vec<SentenceItem>> {
    let dir = raw.join("dakshina/bn/romanized");
    let per_split = dir.join(format!("bn.romanized.rejoined.{split}.tsv"));
    let path = if per_split.exists() {
        per_split.clone()
    } else {
        dir.join("bn.romanized.rejoined.tsv")
    };
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in open_lines(&path)?.lines() {
        let line = line?;
        let parts: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
        if parts.len() >= 2 {
            rows.push((parts[0].to_string(), parts[1].to_string()));
        }
    }
    if path != per_split {
        let half = rows.len() / 2;
        rows = if split == "dev" {
            rows[..half].to_vec()
        } else {
            rows[half..].to_vec()
        };
    }
    Ok(rows
        .into_iter()
        .map(|(native, roman)| SentenceItem {
            roman,
            gold: crate::textnorm::canonical(&native),
            source: "dakshina".into(),
        })
        .collect())
}

fn banglatlit_sentences(raw: &Path, split: &str) -> Result<Vec<SentenceItem>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    let mut rdr = csv::Reader::from_path(raw.join(format!("banglatlit/{split}.csv")))?;
    for rec in rdr.deserialize::<std::collections::HashMap<String, String>>() {
        let row = rec?;
        let bn = row.get("text_bengali").map(|s| s.trim()).unwrap_or("");
        let rm = row.get("text_transliterated").map(|s| s.trim()).unwrap_or("");
        if !bn.is_empty() && !rm.is_empty() {
            out.push(SentenceItem {
                roman: rm.to_string(),
                gold: crate::textnorm::canonical(bn),
                source: "banglatlit".into(),
            });
        }
    }
    Ok(out)
}

pub fn load_sentences(repo: &Path, name: &str) -> Result<Vec<SentenceItem>, Box<dyn std::error::Error>> {
    let raw = repo.join("data/raw");
    if let Some(split) = name.strip_prefix("dakshina-") {
        return Ok(dakshina_sentences(&raw, split)?);
    }
    if let Some(split) = name.strip_prefix("banglatlit-") {
        return banglatlit_sentences(&raw, split);
    }
    Err(format!("unknown sentence set: {name}").into())
}
