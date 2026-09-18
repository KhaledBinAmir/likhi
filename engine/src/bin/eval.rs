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
use likhi_engine::evalkit::metrics::{percentile, WordEval};

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

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 2 || argv[1] != "words" {
        eprintln!("usage: likhi-eval words --dataset <name> [--k 5] [--limit N] [--threads N] [--no-save]");
        eprintln!("  datasets: dakshina-<split>, aksharantar-<split>, banglatlit-<split>-words,");
        eprintln!("            feedback-words; any of them with a +grouped suffix");
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
    if let Err(e) = run(&args) {
        eprintln!("[eval] failed: {e}");
        std::process::exit(1);
    }
}
