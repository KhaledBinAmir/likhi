//! Tune the ranker weights on dev sets by coordinate ascent.
//! A port of `src/likhi/eval/tune.py`.
//!
//!     likhi-tune cache  --dataset dakshina-dev:1500 --dataset banglatlit-val-words
//!     likhi-tune search --metric top1
//!
//! Features for every dev item are computed once -- the expensive part, tries plus model scoring of
//! the top-N candidates under a weight-independent pre-ranking -- and cached. The search then only
//! re-scores cached features, so hundreds of weight settings cost seconds instead of hours.
//!
//! **The cache must not depend on the weights being searched.** That is what
//! `CandidateOptions::static_prescore` is for: it picks which candidates get a model score using a
//! ranking the tuner cannot influence. Without it the cache would encode the weights it was built
//! with and every comparison would be against a moving target.
//!
//! One deliberate difference from the Python. There, `_score` is a hand-copied second
//! implementation of the ranker, kept beside a comment asking whoever edits one to remember the
//! other. Here the cached features are fed to `core::score_features`, the same function the engine
//! ranks with, so the tuner cannot drift from what it is tuning.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use likhi_engine::core::{score_features, CandidateOptions, Engine, EngineOptions, Feats, Weights};
use likhi_engine::evalkit::datasets::load_wordset;
use likhi_engine::textnorm::match_key;

/// What the search needs about one candidate. The unigram prior is cached with it so the search
/// never touches the lexicon.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedCandidate {
    word: String,
    uni: f64,
    rom_exact: u32,
    rom_prefix: u32,
    key_fine: bool,
    key_coarse: bool,
    key_fine_prefix: bool,
    key_coarse_prefix: bool,
    gap: u32,
    xlit_rank: usize,
    /// `None` is the Python's NaN: "the model did not score this". JSON has no NaN, and mapping it
    /// to 0.0 would turn "unscored" into "scored, perfectly" -- the single most valuable feature
    /// there is.
    xlit_logp: Option<f64>,
    avro: bool,
    in_lexicon: bool,
}

impl CachedCandidate {
    fn to_feats(&self) -> Feats {
        Feats {
            rom_exact: self.rom_exact,
            rom_prefix: self.rom_prefix,
            key_fine: self.key_fine,
            key_coarse: self.key_coarse,
            key_fine_prefix: self.key_fine_prefix,
            key_coarse_prefix: self.key_coarse_prefix,
            gap: self.gap,
            xlit_rank: self.xlit_rank,
            xlit_logp: self.xlit_logp.unwrap_or(f64::NAN),
            avro: self.avro,
            in_lexicon: self.in_lexicon,
            ..Default::default()
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedItem {
    set: String,
    roman: String,
    /// Already `match_key`ed, so the search compares without normalising 9,000 words per round.
    golds: Vec<String>,
    weight: f64,
    cands: Vec<CachedCandidate>,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn cache_path(repo: &Path) -> PathBuf {
    repo.join("data").join("cache").join("tune_features.jsonl")
}

fn log(msg: &str) {
    println!("[tune] {msg}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

// ------------------------------------------------------------------------------------- caching

fn cmd_cache(datasets: &[String], model_scored: usize, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo();
    let dir = std::env::var_os("LIKHI_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("models").join("rust"));
    let engine = Arc::new(Engine::open(
        &dir,
        EngineOptions { personal: None, ..Default::default() },
    )?);

    let mut work: Vec<(String, String, Vec<String>, f64)> = Vec::new();
    for spec in datasets {
        // name[:sample]
        let (name, sample) = match spec.split_once(':') {
            Some((n, s)) => (n, s.parse::<usize>().ok()),
            None => (spec.as_str(), None),
        };
        let ws = load_wordset(&repo, name)?;
        let mut items = ws.items;
        if let Some(n) = sample {
            // Deterministic subsampling: every nth item. The Python shuffles with a fixed seed,
            // which is reproducible in Python but not portable to another RNG; a stride gives the
            // same property -- a stable, spread-out subset -- without pretending to match it.
            let stride = items.len().div_ceil(n.max(1));
            items = items.into_iter().step_by(stride.max(1)).take(n).collect();
        }
        log(&format!("{}: {} items", ws.name, items.len()));
        for it in items {
            work.push((
                ws.name.clone(),
                it.roman,
                it.golds.iter().map(|g| match_key(g)).collect(),
                it.weight,
            ));
        }
    }
    if work.is_empty() {
        return Err("no items to cache".into());
    }

    let started = Instant::now();
    let work = Arc::new(work);
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let total = work.len();

    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let engine = Arc::clone(&engine);
        let work = Arc::clone(&work);
        let next = Arc::clone(&next);
        let done = Arc::clone(&done);
        handles.push(std::thread::spawn(move || {
            let opts = CandidateOptions {
                static_prescore: true,
                model_scored: Some(model_scored),
            };
            let mut out: Vec<(usize, CachedItem)> = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= work.len() {
                    return out;
                }
                let (set, roman, golds, weight) = &work[i];
                let table = engine
                    .candidates_with(roman, true, &opts, None)
                    .expect("no abort flag");
                let cands = table
                    .iter()
                    .map(|(word, ft)| CachedCandidate {
                        word: word.to_string(),
                        uni: engine.unigram_logp(word),
                        rom_exact: ft.rom_exact,
                        rom_prefix: ft.rom_prefix,
                        key_fine: ft.key_fine,
                        key_coarse: ft.key_coarse,
                        key_fine_prefix: ft.key_fine_prefix,
                        key_coarse_prefix: ft.key_coarse_prefix,
                        gap: ft.gap,
                        xlit_rank: ft.xlit_rank,
                        xlit_logp: if ft.xlit_logp.is_nan() { None } else { Some(ft.xlit_logp) },
                        avro: ft.avro,
                        in_lexicon: ft.in_lexicon,
                    })
                    .collect();
                out.push((
                    i,
                    CachedItem {
                        set: set.clone(),
                        roman: roman.clone(),
                        golds: golds.clone(),
                        weight: *weight,
                        cands,
                    },
                ));
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(500) {
                    println!("[tune]   {n}/{total}");
                }
            }
        }));
    }

    let mut rows: Vec<(usize, CachedItem)> = Vec::with_capacity(total);
    for h in handles {
        rows.extend(h.join().map_err(|_| "a worker thread panicked")?);
    }
    rows.sort_by_key(|(i, _)| *i);

    let path = cache_path(&repo);
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    let mut out = String::new();
    for (_, row) in &rows {
        out.push_str(&serde_json::to_string(row)?);
        out.push('\n');
    }
    std::fs::write(&path, out)?;
    log(&format!(
        "cached {} items to {} in {:.0}s",
        rows.len(),
        path.display(),
        started.elapsed().as_secs_f64()
    ));
    Ok(())
}

// -------------------------------------------------------------------------------------- search

fn load_cache(path: &Path) -> Result<Vec<CachedItem>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: {e}; run `likhi-tune cache` first", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.trim().is_empty() {
            out.push(serde_json::from_str(line)?);
        }
    }
    Ok(out)
}

/// Per-set score for one weight setting, plus the macro average across sets.
///
/// Macro, not micro: the dev sets differ in size by more than an order of magnitude, and a micro
/// average would tune almost entirely for whichever is biggest.
fn evaluate(rows: &[CachedItem], w: &Weights, metric: &str) -> (HashMap<String, f64>, f64) {
    let mut per_set: HashMap<String, (f64, usize)> = HashMap::new();
    let mut scratch: Vec<(f64, usize)> = Vec::new();
    for row in rows {
        scratch.clear();
        for (i, c) in row.cands.iter().enumerate() {
            scratch.push((score_features(&c.word, &c.to_feats(), c.uni, w), i));
        }
        // Descending score; ties keep candidate order, as Python's stable `sorted` does.
        scratch.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        let rank = scratch
            .iter()
            .position(|(_, i)| row.golds.iter().any(|g| g == &match_key(&row.cands[*i].word)))
            .map(|p| p + 1);
        let v = match metric {
            "top1" => f64::from(rank == Some(1)),
            "top3" => f64::from(rank.is_some_and(|r| r <= 3)),
            _ => rank.map(|r| 1.0 / r as f64).unwrap_or(0.0),
        };
        let e = per_set.entry(row.set.clone()).or_insert((0.0, 0));
        e.0 += v;
        e.1 += 1;
    }
    let out: HashMap<String, f64> = per_set
        .iter()
        .map(|(k, (sum, n))| (k.clone(), 100.0 * sum / *n as f64))
        .collect();
    let macro_avg = out.values().sum::<f64>() / out.len().max(1) as f64;
    (out, macro_avg)
}

/// The search grid, copied from the Python. Only these weights are tuned; the personal and bigram
/// terms are not, because the dev sets have no personal history and no sentence context.
const GRID: &[(&str, &[f64])] = &[
    ("unigram", &[0.5, 0.75, 1.0, 1.25, 1.5]),
    ("rom_exact", &[0.0, 1.0, 2.0, 3.0, 4.0, 6.0]),
    ("rom_exact_log", &[0.0, 0.4, 0.8, 1.2, 1.6]),
    ("rom_prefix", &[-0.5, 0.0, 0.3, 0.6]),
    ("key_fine", &[0.0, 0.6, 1.2, 2.0, 3.0]),
    ("key_coarse", &[0.0, 0.3, 0.6, 1.2, 2.0]),
    ("key_prefix", &[-3.0, -2.0, -1.0, 0.0]),
    ("gap", &[-1.5, -1.0, -0.6, -0.4, -0.2, 0.0]),
    ("xlit_logp", &[0.1, 0.2, 0.35, 0.5, 0.75, 1.0, 1.5]),
    ("xlit_top1", &[0.0, 1.0, 1.5, 2.5, 4.0]),
    ("xlit_top3", &[0.0, 0.5, 1.0, 2.0]),
    ("avro", &[0.0, 0.5, 1.0, 2.0]),
    ("oov", &[-6.0, -4.0, -3.0, -2.0, -1.0, 0.0]),
];

fn show(per_set: &HashMap<String, f64>, macro_avg: f64) -> String {
    let mut keys: Vec<&String> = per_set.keys().collect();
    keys.sort();
    let parts: Vec<String> = keys
        .iter()
        .map(|k| format!("{k}: {:.2}", per_set[*k]))
        .collect();
    format!("{{{}, macro: {macro_avg:.2}}}", parts.join(", "))
}

/// Below this many cached items, a search overfits badly enough that its weights are worse than
/// the ones it would replace. See `cmd_search` for why that is enforced rather than warned about.
const MIN_ITEMS_TO_OVERWRITE: usize = 2000;

fn cmd_search(
    metric: &str,
    rounds: usize,
    start: Option<&Path>,
    out: &Path,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo();
    let rows = load_cache(&cache_path(&repo))?;
    log(&format!("{} cached items", rows.len()));

    // Refuse to overwrite a real weights file from a small cache, rather than warning about it.
    // The Python tuner writes unconditionally, and a one-round search over 23 items replaced the
    // tuned weights during testing -- weights worth about seven points of top-1, gone silently.
    // Writing somewhere else with --out is always allowed; only clobbering the live file is gated.
    let is_live = out.exists();
    if is_live && rows.len() < MIN_ITEMS_TO_OVERWRITE && !force {
        return Err(format!(
            "{} cached items is too few to tune on, and {} already exists.\n       \
             Cache more (a few thousand across the dev sets), write elsewhere with --out, \
             or pass --force if you mean it.",
            rows.len(),
            out.display()
        )
        .into());
    }

    let mut w = match start {
        Some(p) if p.exists() => {
            let map: HashMap<String, f64> = serde_json::from_str(&std::fs::read_to_string(p)?)?;
            log(&format!("starting from {}", p.display()));
            Weights::from_map(map)
        }
        _ => Weights::defaults(),
    };

    let (per_set, mut best) = evaluate(&rows, &w, metric);
    log(&format!("start {metric}: {}", show(&per_set, best)));

    for round in 0..rounds {
        let mut improved = false;
        for (name, values) in GRID {
            let current = w.get(name);
            let mut best_value = current;
            let mut best_macro = best;
            for &v in *values {
                if v == current {
                    continue;
                }
                w.set(name, v);
                let (_, m) = evaluate(&rows, &w, metric);
                if m > best_macro + 1e-9 {
                    best_macro = m;
                    best_value = v;
                }
            }
            w.set(name, best_value);
            if best_value != current {
                improved = true;
                let (per_set, m) = evaluate(&rows, &w, metric);
                best = m;
                log(&format!(
                    "round {}: {name}={best_value} -> macro {:.2}   {}",
                    round + 1,
                    m,
                    show(&per_set, m)
                ));
            }
        }
        if !improved {
            log(&format!("round {} changed nothing; stopping", round + 1));
            break;
        }
    }

    let (per_set, macro_avg) = evaluate(&rows, &w, metric);
    log(&format!("final {metric}: {}", show(&per_set, macro_avg)));

    let map = w.as_map();
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let mut json = serde_json::Map::new();
    for k in keys {
        json.insert(k.clone(), serde_json::json!(map[k]));
    }
    std::fs::create_dir_all(out.parent().expect("has a parent"))?;
    std::fs::write(out, serde_json::to_string_pretty(&json)?)?;
    log(&format!("saved to {}", out.display()));
    log("rebuild the lexicon (or copy this beside the tables) for the engine to pick it up");
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let usage = "usage:\n  \
        likhi-tune cache  --dataset <name[:sample]> [--dataset ...] [--model-scored 16] [--threads N]\n  \
        likhi-tune search [--metric top1|top3|mrr] [--rounds 4] [--start <weights.json>] [--out <weights.json>] [--force]";
    if argv.len() < 2 {
        eprintln!("{usage}");
        std::process::exit(2);
    }

    let repo = repo();
    let result = match argv[1].as_str() {
        "cache" => {
            let mut datasets = Vec::new();
            let mut model_scored = 16usize;
            let mut threads =
                std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            let mut i = 2;
            while i < argv.len() {
                let next = argv.get(i + 1).cloned();
                match argv[i].as_str() {
                    "--dataset" => {
                        datasets.push(next.expect("--dataset needs a name"));
                        i += 2;
                    }
                    "--model-scored" => {
                        model_scored = next.and_then(|v| v.parse().ok()).unwrap_or(16);
                        i += 2;
                    }
                    "--threads" => {
                        threads = next.and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
                        i += 2;
                    }
                    other => {
                        eprintln!("unknown argument {other}\n{usage}");
                        std::process::exit(2);
                    }
                }
            }
            if datasets.is_empty() {
                eprintln!("cache needs at least one --dataset\n{usage}");
                std::process::exit(2);
            }
            cmd_cache(&datasets, model_scored, threads)
        }
        "search" => {
            let mut metric = "top1".to_string();
            let mut rounds = 4usize;
            let mut start: Option<PathBuf> = Some(repo.join("models/rust/lexicon/weights.json"));
            let mut out = repo.join("models/rust/lexicon/weights.json");
            let mut force = false;
            let mut i = 2;
            while i < argv.len() {
                let next = argv.get(i + 1).cloned();
                match argv[i].as_str() {
                    "--metric" => {
                        metric = next.expect("--metric needs a name");
                        i += 2;
                    }
                    "--rounds" => {
                        rounds = next.and_then(|v| v.parse().ok()).unwrap_or(4);
                        i += 2;
                    }
                    "--start" => {
                        start = next.map(PathBuf::from);
                        i += 2;
                    }
                    "--from-defaults" => {
                        start = None;
                        i += 1;
                    }
                    "--out" => {
                        out = PathBuf::from(next.expect("--out needs a path"));
                        i += 2;
                    }
                    "--force" => {
                        force = true;
                        i += 1;
                    }
                    other => {
                        eprintln!("unknown argument {other}\n{usage}");
                        std::process::exit(2);
                    }
                }
            }
            cmd_search(&metric, rounds, start.as_deref(), &out, force)
        }
        other => {
            eprintln!("unknown command {other}\n{usage}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("[tune] failed: {e}");
        std::process::exit(1);
    }
}
