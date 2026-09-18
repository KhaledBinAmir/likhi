//! `likhi-eval words`: word-level top-k accuracy, MRR, CER and latency.
//! A port of the `words` command of `src/likhi/eval/cli.py`.
//!
//!     cargo run --release --features tools --bin likhi-eval -- \
//!         words --dataset dakshina-dev
//!
//! Why this exists in Rust: the Python harness drives the Python engine one word at a time, and a
//! run over dakshina-dev takes about twelve minutes. That is the loop every ranking change has to
//! go through, so it is the loop worth making fast. This runs the words across all cores.
//!
//! **Latency numbers are only comparable at `--threads 1`.** Measured under contention they say
//! more about the scheduler than the engine, so the thread count is recorded in the result file and
//! printed with the summary. Accuracy is unaffected by threading: each word is independent, and
//! results are re-assembled in dataset order before anything is accumulated.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use likhi_engine::core::{Engine, EngineOptions};
use likhi_engine::evalkit::datasets::{load_wordset, WordItem};
use likhi_engine::evalkit::metrics::{percentile, rank_of_gold, wer, WordEval};
use likhi_engine::evalkit::sentences::load_sentences;
use likhi_engine::textnorm::{has_bengali, normalize_roman};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn data_dir(repo: &Path) -> PathBuf {
    std::env::var_os("LIKHI_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("models").join("rust"))
}

struct Args {
    dataset: String,
    k: usize,
    limit: Option<usize>,
    errors: usize,
    threads: usize,
    save: bool,
}

/// One word's outcome, kept with its index so the accumulation is dataset-ordered regardless of
/// which thread finished first.
struct Outcome {
    index: usize,
    candidates: Vec<String>,
    millis: f64,
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo();
    let dir = data_dir(&repo);
    let wordset = load_wordset(&repo, &args.dataset)?;
    let items: Vec<WordItem> = match args.limit {
        Some(n) => wordset.items.into_iter().take(n).collect(),
        None => wordset.items,
    };
    if items.is_empty() {
        return Err(format!("{} is empty", args.dataset).into());
    }

    // personal: None -- a measurement must not depend on what this machine has been taught.
    let engine = Arc::new(Engine::open(
        &dir,
        EngineOptions { personal: None, ..Default::default() },
    )?);

    let started = Instant::now();
    let items = Arc::new(items);
    let next = Arc::new(AtomicUsize::new(0));
    let k = args.k;

    let mut handles = Vec::with_capacity(args.threads);
    for _ in 0..args.threads {
        let engine = Arc::clone(&engine);
        let items = Arc::clone(&items);
        let next = Arc::clone(&next);
        handles.push(std::thread::spawn(move || {
            let mut out: Vec<Outcome> = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    return out;
                }
                let t = Instant::now();
                let candidates = engine.suggest(&items[i].roman, &[], k, false);
                out.push(Outcome {
                    index: i,
                    candidates,
                    millis: t.elapsed().as_secs_f64() * 1000.0,
                });
            }
        }));
    }

    let mut outcomes: Vec<Outcome> = Vec::with_capacity(items.len());
    for h in handles {
        outcomes.extend(h.join().map_err(|_| "a worker thread panicked")?);
    }
    outcomes.sort_by_key(|o| o.index);
    let elapsed = started.elapsed().as_secs_f64();

    let mut ks = vec![1, 3, 5, args.k];
    ks.sort_unstable();
    ks.dedup();
    let mut ev = WordEval::new(ks);
    let mut latencies = Vec::with_capacity(outcomes.len());
    let mut misses = Vec::new();
    let mut miss_by_source: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for o in &outcomes {
        let item = &items[o.index];
        latencies.push(o.millis);
        let rank = ev.add(&o.candidates, &item.golds, item.weight);
        if rank != Some(1) {
            *miss_by_source.entry(item.source.clone()).or_insert(0) += 1;
            if misses.len() < args.errors {
                misses.push(serde_json::json!({
                    "roman": item.roman,
                    "gold": item.golds,
                    "got": o.candidates.iter().take(k).collect::<Vec<_>>(),
                    "rank": rank,
                }));
            }
        }
    }

    let summary = ev.summary();
    println!(
        "[likhi on {}] n={}  top1={:.2}  top3={:.2}  top5={:.2}  mrr={:.2}  cer={:.2}  \
         p50={:.2}ms p95={:.2}ms  threads={}  {:.1}s",
        wordset.name,
        summary.n,
        summary.top(1),
        summary.top(3),
        summary.top(5),
        summary.mrr,
        summary.cer,
        percentile(&latencies, 50.0),
        percentile(&latencies, 95.0),
        args.threads,
        elapsed,
    );
    if args.threads > 1 {
        println!("  (latency is measured under contention; use --threads 1 to compare it)");
    }

    if args.save {
        let mut result = serde_json::Map::new();
        result.insert("kind".into(), "words".into());
        result.insert("system".into(), "likhi-rust".into());
        result.insert("dataset".into(), wordset.name.clone().into());
        result.insert("k".into(), k.into());
        result.insert("n".into(), summary.n.into());
        for (kk, v) in &summary.topk {
            result.insert(format!("top{kk}"), (*v).into());
        }
        result.insert("mrr".into(), summary.mrr.into());
        result.insert("cer".into(), summary.cer.into());
        result.insert(
            "latency_ms".into(),
            serde_json::json!({
                "p50": percentile(&latencies, 50.0),
                "p95": percentile(&latencies, 95.0),
                "p99": percentile(&latencies, 99.0),
                "mean": latencies.iter().sum::<f64>() / latencies.len() as f64,
            }),
        );
        result.insert("threads".into(), args.threads.into());
        result.insert("elapsed_s".into(), elapsed.into());
        result.insert("miss_by_source".into(), serde_json::to_value(&miss_by_source)?);
        result.insert("sample_misses".into(), serde_json::Value::Array(misses));

        let dir = repo.join("results");
        std::fs::create_dir_all(&dir)?;
        // No timestamp: this binary has no clock it can use without pulling in a date library, and
        // the caller knows when it ran. A fixed name per (system, dataset) is overwritten, which is
        // what a gate wants -- the timestamped history is the Python harness's job.
        let path = dir.join(format!("latest_words_likhi-rust_{}.json", wordset.name));
        std::fs::write(&path, serde_json::to_string_pretty(&result)?)?;
        println!("   saved: {}", path.display());
    }
    Ok(())
}

/// Transliterate a sentence word by word, carrying the last two committed words as context.
///
/// Tokens with no Latin letters are passed through untouched: they are URLs, numbers and English
/// that no transliterator should be rewriting, and counting them as errors would measure the wrong
/// thing.
fn transliterate_sentence(engine: &Engine, roman: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut ctx: Vec<String> = Vec::new();
    for tok in roman.split_whitespace() {
        let core = strip_edge_punct(tok);
        if core.is_empty() {
            continue;
        }
        if !core.chars().any(|c| c.is_ascii_alphabetic()) {
            out.push(core.to_string());
            continue;
        }
        let context: Vec<String> = ctx.iter().rev().take(2).rev().cloned().collect();
        let cands = engine.suggest(&normalize_roman(core), &context, 1, false);
        let word = cands.first().cloned().unwrap_or_else(|| core.to_string());
        out.push(word.clone());
        ctx.push(word);
    }
    out
}

/// Strip punctuation at the edges only, treating the Bengali block and the joiners as word
/// characters -- a naive strip would eat a final vowel sign.
fn strip_edge_punct(tok: &str) -> &str {
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

fn run_sentences(dataset: &str, limit: Option<usize>, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo();
    let engine = Arc::new(Engine::open(
        &data_dir(&repo),
        EngineOptions { personal: None, ..Default::default() },
    )?);
    let mut sents = load_sentences(&repo, dataset)?;
    if let Some(n) = limit {
        sents.truncate(n);
    }
    if sents.is_empty() {
        return Err(format!("{dataset} is empty").into());
    }

    let started = Instant::now();
    let sents = Arc::new(sents);
    let next = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let engine = Arc::clone(&engine);
        let sents = Arc::clone(&sents);
        let next = Arc::clone(&next);
        handles.push(std::thread::spawn(move || {
            // (index, errors, ref tokens, bengali-only errors, bengali-only ref tokens)
            let mut out: Vec<(usize, usize, usize, usize, usize)> = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= sents.len() {
                    return out;
                }
                let hyp = transliterate_sentence(&engine, &sents[i].roman);
                let reference: Vec<String> = sents[i]
                    .gold
                    .split_whitespace()
                    .map(|t| strip_edge_punct(t).to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                let (e, n) = wer(&hyp, &reference);
                // Bengali-only view: ignore tokens the gold keeps in Latin.
                let hyp_bn: Vec<String> =
                    hyp.iter().filter(|t| has_bengali(t)).cloned().collect();
                let ref_bn: Vec<String> =
                    reference.iter().filter(|t| has_bengali(t)).cloned().collect();
                let (e2, n2) = wer(&hyp_bn, &ref_bn);
                out.push((i, e, n, e2, n2));
            }
        }));
    }
    let (mut err, mut refs, mut bn_err, mut bn_refs) = (0usize, 0usize, 0usize, 0usize);
    for h in handles {
        for (_, e, n, e2, n2) in h.join().map_err(|_| "a worker thread panicked")? {
            err += e;
            refs += n;
            bn_err += e2;
            bn_refs += n2;
        }
    }
    println!(
        "[likhi on {dataset}] n={}  wer={:.2}  wer_bengali_tokens={:.2}  ref_tokens={}  threads={}  {:.1}s",
        sents.len(),
        100.0 * err as f64 / refs.max(1) as f64,
        100.0 * bn_err as f64 / bn_refs.max(1) as f64,
        refs,
        threads,
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

/// Keystroke replay: type each word letter by letter and record when the gold first appears.
///
/// Reports the share of keystrokes a typist could skip by committing as soon as the right word is
/// top-1, or within top-k. These are the numbers that decide whether the keyboard *feels* fast,
/// as distinct from whether it is accurate.
fn run_replay(dataset: &str, k: usize, limit: Option<usize>, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo();
    let engine = Arc::new(Engine::open(
        &data_dir(&repo),
        EngineOptions { personal: None, ..Default::default() },
    )?);
    let ws = load_wordset(&repo, dataset)?;
    let items: Vec<WordItem> = match limit {
        Some(n) => ws.items.into_iter().take(n).collect(),
        None => ws.items,
    };
    if items.is_empty() {
        return Err(format!("{dataset} is empty").into());
    }

    let started = Instant::now();
    let items = Arc::new(items);
    let next = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let engine = Arc::clone(&engine);
        let items = Arc::clone(&items);
        let next = Arc::clone(&next);
        handles.push(std::thread::spawn(move || {
            // (keystrokes, saved_top1, saved_topk, never_top1, never_topk, latencies)
            let mut acc = (0usize, 0usize, 0usize, 0usize, 0usize, Vec::<f64>::new());
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    return acc;
                }
                let it = &items[i];
                let chars: Vec<char> = it.roman.chars().collect();
                let n = chars.len();
                if n == 0 {
                    continue;
                }
                acc.0 += n;
                let (mut first1, mut firstk) = (None, None);
                for prefix_len in 1..=n {
                    let prefix: String = chars[..prefix_len].iter().collect();
                    let t = Instant::now();
                    let cands = engine.suggest(&prefix, &[], k, false);
                    acc.5.push(t.elapsed().as_secs_f64() * 1000.0);
                    let rank = rank_of_gold(&cands, &it.golds);
                    if rank == Some(1) && first1.is_none() {
                        first1 = Some(prefix_len);
                    }
                    if rank.is_some_and(|r| r <= k) && firstk.is_none() {
                        firstk = Some(prefix_len);
                    }
                    if first1.is_some() && firstk.is_some() {
                        break;
                    }
                }
                match first1 {
                    Some(at) => acc.1 += n - at,
                    None => acc.3 += 1,
                }
                match firstk {
                    Some(at) => acc.2 += n - at,
                    None => acc.4 += 1,
                }
            }
        }));
    }
    let (mut keys, mut saved1, mut savedk, mut never1, mut neverk) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut lat: Vec<f64> = Vec::new();
    for h in handles {
        let (a, b, c, d, e, mut l) = h.join().map_err(|_| "a worker thread panicked")?;
        keys += a;
        saved1 += b;
        savedk += c;
        never1 += d;
        neverk += e;
        lat.append(&mut l);
    }
    println!(
        "[likhi on {}] n={}  keystrokes={}  saved_top1={:.2}%  saved_top{k}={:.2}%  \
         never_top1={:.2}%  never_top{k}={:.2}%  p50={:.2}ms p95={:.2}ms  threads={}  {:.1}s",
        ws.name,
        items.len(),
        keys,
        100.0 * saved1 as f64 / keys.max(1) as f64,
        100.0 * savedk as f64 / keys.max(1) as f64,
        100.0 * never1 as f64 / items.len() as f64,
        100.0 * neverk as f64 / items.len() as f64,
        percentile(&lat, 50.0),
        percentile(&lat, 95.0),
        threads,
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let usage = "usage:\n  \
        likhi-eval words     --dataset <name> [--k 5] [--limit N] [--threads N] [--no-save]\n  \
        likhi-eval sentences --dataset <name> [--limit N] [--threads N]\n  \
        likhi-eval replay    --dataset <name> [--k 5] [--limit N] [--threads N]\n\
        \n  word sets    : dakshina-<split>, aksharantar-<split>, banglatlit-<split>-words,\n  \
                         feedback-words; any with a +grouped suffix\n  \
        sentence sets: dakshina-<split>, banglatlit-<split>";
    if argv.len() < 2 || !matches!(argv[1].as_str(), "words" | "sentences" | "replay") {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let mut args = Args {
        dataset: "dakshina-dev".into(),
        k: 5,
        limit: None,
        errors: 10,
        threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        save: true,
    };
    let mut i = 2;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--dataset" => {
                args.dataset = next.expect("--dataset needs a name");
                i += 2;
            }
            "--k" => {
                args.k = next.and_then(|v| v.parse().ok()).unwrap_or(5);
                i += 2;
            }
            "--limit" => {
                args.limit = next.and_then(|v| v.parse().ok());
                i += 2;
            }
            "--errors" => {
                args.errors = next.and_then(|v| v.parse().ok()).unwrap_or(10);
                i += 2;
            }
            "--threads" => {
                args.threads = next.and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
                i += 2;
            }
            "--no-save" => {
                args.save = false;
                i += 1;
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let _ = std::io::stdout().flush();
    let result = match argv[1].as_str() {
        "words" => run(&args),
        "sentences" => run_sentences(&args.dataset, args.limit, args.threads),
        "replay" => run_replay(&args.dataset, args.k, args.limit, args.threads),
        _ => unreachable!("checked above"),
    };
    if let Err(e) = result {
        eprintln!("[eval] failed: {e}");
        std::process::exit(1);
    }
}
