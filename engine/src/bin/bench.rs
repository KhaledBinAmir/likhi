//! Latency of the engine's two paths, which is the whole reason the port exists.
//!
//!     cargo run --release --bin bench
//!
//! The numbers that matter, from the Python engine on this machine: the fast path (tables only, run
//! on every keystroke) at 0.8 ms p50, and the full path (transformer beam search plus teacher-forced
//! scoring) at 77 ms p50. The fast path has a hard budget because it sits between a key press and a
//! character appearing; the full path runs behind it and only has to finish before someone stops
//! typing to look.
//!
//! Reports percentiles rather than a mean: a mean hides exactly the stalls a typist notices.

use std::path::Path;
use std::time::Instant;

use likhi_engine::core::{Engine, EngineOptions};

const WORDS: &[&str] = &[
    "amar", "tumi", "kemon", "acho", "bhalo", "achi", "dhonnobad", "kothay", "jabe", "ekhon",
    "korchi", "khacche", "bangla", "sonar", "rapid", "bajay", "chiro", "chirodin", "maam", "myam",
    "screensaver", "computer", "office", "amr", "tmi", "korci", "koria", "shoytan", "khaitecho",
    "protidin",
];

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn report(label: &str, mut times: Vec<f64>) {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  {label:<12} p50 {:6.2} ms   p90 {:6.2} ms   p99 {:6.2} ms   max {:6.2} ms   n={}",
        percentile(&times, 0.50),
        percentile(&times, 0.90),
        percentile(&times, 0.99),
        percentile(&times, 1.0),
        times.len()
    );
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("engine/ has a parent")
        .join("models")
        .join("rust");
    if !dir.join("indicxlit").join("model.lkw").exists() {
        eprintln!("run scripts/build_rust_data.py first");
        std::process::exit(1);
    }

    let started = Instant::now();
    let e = Engine::open(&dir, EngineOptions { personal: None, ..Default::default() })
        .expect("engine opens");
    println!("startup: {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);

    // Warm the mapped pages: the first touch of each table is a page fault against the file, and
    // measuring that as if it were compute would flatter every later number.
    for w in WORDS.iter().take(5) {
        let _ = e.suggest(w, &[], 5, false);
    }

    let mut fast = Vec::new();
    let mut full = Vec::new();
    let mut prefixes = Vec::new();
    for _round in 0..5 {
        for w in WORDS {
            let t = Instant::now();
            let _ = e.suggest(w, &[], 5, true);
            fast.push(t.elapsed().as_secs_f64() * 1000.0);

            let t = Instant::now();
            let _ = e.suggest(w, &[], 5, false);
            full.push(t.elapsed().as_secs_f64() * 1000.0);

            // What actually happens while someone types: the fast path on every growing prefix.
            for n in 1..=w.len() {
                let t = Instant::now();
                let _ = e.suggest(&w[..n], &[], 5, true);
                prefixes.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    println!("\nlatency:");
    report("fast", fast);
    report("full", full);
    report("keystrokes", prefixes);
    println!("\n  (python on this machine: fast 0.8 ms p50, full 77 ms p50)");
}

