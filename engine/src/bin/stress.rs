//! `likhi-stress`: thousands of words, many typing habits.
//! A port of `src/likhi/eval/stress.py`.
//!
//!     likhi-stress run --limit 1500
//!     likhi-stress report
//!
//! Takes words with human romanizations -- Dakshina test, BanglaTLit test words, the feedback set
//! -- rewrites each one the way different people type, and reports top-1 and top-5 per habit. The
//! attested spelling is only one of eleven; the other ten are how the engine actually gets used,
//! and the gap between `as_is` and the rest is the honest measure of how forgiving it is.
//!
//! Variants come from `pyrandom` with the same seed the Python used, so a given seed produces the
//! same words and results stay comparable with anything measured before the port.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use likhi_engine::core::{Engine, EngineOptions};
use likhi_engine::evalkit::datasets::load_wordset;
use likhi_engine::evalkit::metrics::{percentile, rank_of_gold};
use likhi_engine::evalkit::pyrandom::PyRandom;
use likhi_engine::evalkit::styles::{apply, STYLE_NAMES};

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Variant {
    src: String,
    style: String,
    roman: String,
    orig: String,
    golds: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Scored {
    #[serde(flatten)]
    variant: Variant,
    got: Vec<String>,
    rank: Option<usize>,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn results_dir(repo: &Path) -> PathBuf {
    repo.join("results")
}

fn log(msg: &str) {
    println!("[stress] {msg}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Build the variant list, in the order the Python builds it.
///
/// One generator drives the shuffles and every style in turn, so the source order, the per-source
/// shuffle and the style order all have to match or the variants diverge after the first draw.
fn build_variants(repo: &Path, limit_per_source: usize, seed: u64) -> Result<Vec<Variant>, Box<dyn std::error::Error>> {
    let mut rng = PyRandom::new(seed);
    let mut out = Vec::new();
    let sources: [(&str, &str); 3] = [
        ("dakshina", "dakshina-test+grouped"),
        ("chat", "banglatlit-test-words"),
        ("feedback", "feedback-words"),
    ];
    for (src, dataset) in sources {
        let mut items = load_wordset(repo, dataset)?.items;
        rng.shuffle(&mut items);
        for it in items.into_iter().take(limit_per_source) {
            for style in STYLE_NAMES {
                let variant = apply(style, &it.roman, &mut rng);
                if variant.is_empty() || !variant.chars().any(|c| c.is_ascii_alphabetic()) {
                    continue;
                }
                out.push(Variant {
                    src: src.to_string(),
                    style: style.to_string(),
                    roman: variant,
                    orig: it.roman.clone(),
                    golds: it.golds.clone(),
                });
            }
        }
    }
    Ok(out)
}

fn cmd_run(limit: usize, seed: u64, threads: usize, out_name: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo();
    let engine = Arc::new(Engine::open(
        &std::env::var_os("LIKHI_MODELS")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo.join("models").join("rust")),
        EngineOptions { personal: None, ..Default::default() },
    )?);

    let variants = build_variants(&repo, limit, seed)?;
    log(&format!("{} variants from {} words per source", variants.len(), limit));

    let started = Instant::now();
    let variants = Arc::new(variants);
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let total = variants.len();

    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let engine = Arc::clone(&engine);
        let variants = Arc::clone(&variants);
        let next = Arc::clone(&next);
        let done = Arc::clone(&done);
        handles.push(std::thread::spawn(move || {
            let mut out: Vec<(usize, Scored, f64)> = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= variants.len() {
                    return out;
                }
                let v = &variants[i];
                let t = Instant::now();
                let got = engine.suggest(&v.roman, &[], 5, false);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                let rank = rank_of_gold(&got, &v.golds);
                out.push((i, Scored { variant: v.clone(), got, rank }, ms));
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(2000) {
                    println!("[stress] {n}/{total}");
                }
            }
        }));
    }

    let mut rows: Vec<(usize, Scored, f64)> = Vec::with_capacity(total);
    for h in handles {
        rows.extend(h.join().map_err(|_| "a worker thread panicked")?);
    }
    rows.sort_by_key(|(i, _, _)| *i);
    let lat: Vec<f64> = rows.iter().map(|(_, _, ms)| *ms).collect();
    let items: Vec<&Scored> = rows.iter().map(|(_, s, _)| s).collect();

    let dir = results_dir(&repo);
    std::fs::create_dir_all(&dir)?;
    let name = out_name
        .map(str::to_string)
        .unwrap_or_else(|| format!("stress_latest_likhi-rust_seed{seed}.json"));
    let path = dir.join(name);
    let payload = serde_json::json!({
        "system": "likhi-rust",
        "seed": seed,
        "limit_per_source": limit,
        "latency_p50": percentile(&lat, 50.0),
        "latency_p95": percentile(&lat, 95.0),
        "items": items,
    });
    std::fs::write(&path, serde_json::to_string(&payload)?)?;
    log(&format!(
        "wrote {} items to {} in {:.0}s",
        items.len(),
        path.display(),
        started.elapsed().as_secs_f64()
    ));

    summarise(&items);
    Ok(())
}

/// Top-1 and top-5 per style and per source, plus the worst systematic misses.
fn summarise(items: &[&Scored]) {
    // (hits at 1, hits at 5, total)
    let mut by_style: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    let mut by_source: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for s in items {
        let at1 = usize::from(s.rank == Some(1));
        let at5 = usize::from(s.rank.is_some_and(|r| r <= 5));
        for (map, key) in [
            (&mut by_style, s.variant.style.as_str()),
            (&mut by_source, s.variant.src.as_str()),
        ] {
            let e = map.entry(key).or_insert((0, 0, 0));
            e.0 += at1;
            e.1 += at5;
            e.2 += 1;
        }
    }

    let show = |title: &str, map: &BTreeMap<&str, (usize, usize, usize)>| {
        println!("\n{title}");
        println!("  {:<14} {:>8} {:>8} {:>8}", "", "top1", "top5", "n");
        let mut rows: Vec<(&&str, &(usize, usize, usize))> = map.iter().collect();
        // Worst first: that is the list worth reading.
        rows.sort_by(|a, b| {
            let pa = a.1 .0 as f64 / a.1 .2.max(1) as f64;
            let pb = b.1 .0 as f64 / b.1 .2.max(1) as f64;
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        });
        for (name, (h1, h5, n)) in rows {
            println!(
                "  {:<14} {:>7.2}% {:>7.2}% {:>8}",
                name,
                100.0 * *h1 as f64 / (*n).max(1) as f64,
                100.0 * *h5 as f64 / (*n).max(1) as f64,
                n
            );
        }
    };
    show("by typing habit (worst first)", &by_style);
    show("by source", &by_source);
}

fn cmd_report() -> Result<(), Box<dyn std::error::Error>> {
    let dir = results_dir(&repo());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("stress_") && n.ends_with(".json"))
        })
        .collect();
    files.sort();
    if files.is_empty() {
        println!("no stress results in {}", dir.display());
        return Ok(());
    }
    let mut all: Vec<Scored> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f)?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
            for it in items {
                if let Ok(s) = serde_json::from_value::<Scored>(it.clone()) {
                    all.push(s);
                }
            }
        }
        println!("  read {}", f.file_name().and_then(|n| n.to_str()).unwrap_or("?"));
    }
    let refs: Vec<&Scored> = all.iter().collect();
    println!("\n{} items across {} files", refs.len(), files.len());
    summarise(&refs);
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let usage = "usage:\n  \
        likhi-stress run [--limit 1500] [--seed 7] [--threads N] [--out <name.json>]\n  \
        likhi-stress report";
    if argv.len() < 2 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let result = match argv[1].as_str() {
        "run" => {
            let mut limit = 1500usize;
            let mut seed = 7u64;
            let mut threads =
                std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            let mut out: Option<String> = None;
            let mut i = 2;
            while i < argv.len() {
                let next = argv.get(i + 1).cloned();
                match argv[i].as_str() {
                    "--limit" => {
                        limit = next.and_then(|v| v.parse().ok()).unwrap_or(1500);
                        i += 2;
                    }
                    "--seed" => {
                        seed = next.and_then(|v| v.parse().ok()).unwrap_or(7);
                        i += 2;
                    }
                    "--threads" => {
                        threads = next.and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
                        i += 2;
                    }
                    "--out" => {
                        out = next;
                        i += 2;
                    }
                    other => {
                        eprintln!("unknown argument {other}\n{usage}");
                        std::process::exit(2);
                    }
                }
            }
            cmd_run(limit, seed, threads, out.as_deref())
        }
        "report" => cmd_report(),
        other => {
            eprintln!("unknown command {other}\n{usage}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("[stress] failed: {e}");
        std::process::exit(1);
    }
}
