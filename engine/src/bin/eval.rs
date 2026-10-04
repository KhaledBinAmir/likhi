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
use likhi_engine::evalkit::sentences::{aligned_banglatlit, load_sentences, AlignedRow};
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
    /// An exported next-word model, for `rerank`.
    lm: Option<PathBuf>,
    /// An exported Likhi model, for `complete`.
    model: Option<PathBuf>,
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

/// `likhi-eval context`: every held-out chat word, typed after the correct words before it, and
/// where the correct word ranks among up to 50 candidates.
///
/// This separates the two ways a word comes out wrong. Ranked low but present, a better ranker can
/// fix it; never generated, only a better generator can. Teacher-forced: the context is the gold
/// sentence so far, not the engine's own earlier answers, so one mistake is not counted twice --
/// the sentence mode measures that compounding, this measures the ranking. Only rows whose Bangla
/// and romanization have the same number of words, so each word has its romanization.
/// The aligned rows of a `banglatlit-<split>` dataset name.
fn aligned_rows(repo: &Path, dataset: &str) -> Result<(Vec<AlignedRow>, usize), Box<dyn std::error::Error>> {
    let split = dataset.strip_prefix("banglatlit-").ok_or("this takes banglatlit-<split>")?;
    aligned_banglatlit(repo, split)
}
fn run_context(dataset: &str, limit: Option<usize>, threads: usize, errors: usize) -> Result<(), Box<dyn std::error::Error>> {
    use likhi_engine::textnorm::to_output;
    const DEPTH: usize = 50;
    let repo = repo();
    let engine = Arc::new(Engine::open(
        &data_dir(&repo),
        EngineOptions { personal: None, ..Default::default() },
    )?);
    let (mut rows, total_rows) = aligned_rows(&repo, dataset)?;
    if let Some(n) = limit {
        rows.truncate(n);
    }
    let positions: Vec<(usize, usize)> =
        rows.iter().enumerate().flat_map(|(r, (bn, _))| (0..bn.len()).map(move |i| (r, i))).collect();

    let started = Instant::now();
    let rows = Arc::new(rows);
    let positions = Arc::new(positions);
    let next = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let engine = Arc::clone(&engine);
        let rows = Arc::clone(&rows);
        let positions = Arc::clone(&positions);
        let next = Arc::clone(&next);
        handles.push(std::thread::spawn(move || {
            let mut out: Vec<(usize, Option<usize>, Vec<String>)> = Vec::new();
            loop {
                let p = next.fetch_add(1, Ordering::Relaxed);
                if p >= positions.len() {
                    return out;
                }
                let (r, i) = positions[p];
                let (bn, rom) = &rows[r];
                let context: Vec<String> = bn[i.saturating_sub(likhi_engine::xlit::CONTEXT_WORDS)..i].iter().map(|w| to_output(w)).collect();
                let cands = engine.suggest(&rom[i], &context, DEPTH, false);
                let rank = rank_of_gold(&cands, std::slice::from_ref(&bn[i]));
                out.push((p, rank, cands.into_iter().take(5).collect()));
            }
        }));
    }
    let mut results: Vec<(usize, Option<usize>, Vec<String>)> = Vec::new();
    for h in handles {
        results.extend(h.join().map_err(|_| "a worker thread panicked")?);
    }
    results.sort_by_key(|r| r.0);
    let n = results.len().max(1) as f64;
    let within = |k: usize| results.iter().filter(|r| r.1.is_some_and(|x| x <= k)).count() as f64 / n * 100.0;
    println!(
        "[likhi on {dataset}, context] rows {} of {}, words {}  top1={:.2}  top3={:.2}  top5={:.2}  in10={:.2}  in{DEPTH}={:.2}  absent={:.2}  {:.1}s",
        rows.len(),
        total_rows,
        results.len(),
        within(1),
        within(3),
        within(5),
        within(10),
        within(DEPTH),
        100.0 - within(DEPTH),
        started.elapsed().as_secs_f64()
    );
    // Of the words found but not first, how many are the same sounds spelled another way -- the
    // first choice and the gold share a phonetic key, as with কত and কতো -- rather than another
    // word, as with আসে and আছে. The first kind is often the reference's spelling habit, not an
    // error a typist would mind; the second is what context must decide.
    {
        use likhi_engine::romankey::{key_from_bangla, Level};
        let (mut variant, mut other) = (0usize, 0usize);
        for (p, rank, top) in &results {
            if !rank.is_some_and(|x| x >= 2) {
                continue;
            }
            let (r, i) = positions[*p];
            let gold = &rows[r].0[i];
            let first = top.first().cloned().unwrap_or_default();
            if key_from_bangla(gold, Level::Fine) == key_from_bangla(&first, Level::Fine) {
                variant += 1;
            } else {
                other += 1;
            }
        }
        println!(
            "found but not first: {:.2}% of words -- same sounds, other spelling {:.2}%; another word {:.2}%",
            100.0 * (variant + other) as f64 / n,
            100.0 * variant as f64 / n,
            100.0 * other as f64 / n
        );
    }
    // With a spelling list installed: how often the first choice is a correctly spelt word, and
    // how often it is not although a correctly spelt candidate was in the top five.
    // The reference's own rate is the yardstick: chat spells many words no dictionary lists.
    if engine.is_spelled("").is_some() {
        let (mut listed, mut missed, mut gold_listed, mut gold_first) = (0usize, 0usize, 0usize, 0usize);
        for (p, rank, top) in &results {
            let (r, i) = positions[*p];
            if engine.is_spelled(&rows[r].0[i]) == Some(true) {
                gold_listed += 1;
                if *rank == Some(1) {
                    gold_first += 1;
                }
            }
            let spelled: Vec<bool> = top.iter().map(|w| engine.is_spelled(w).unwrap_or(false)).collect();
            if spelled.first() == Some(&true) {
                listed += 1;
            } else if spelled.iter().any(|s| *s) {
                missed += 1;
            }
        }
        println!(
            "spelling: first choice correctly spelt {:.2}% of words (the reference {:.2}%); not, with a correctly spelt word in the top five, {:.2}%; top1 where the reference is correctly spelt {:.2}",
            100.0 * listed as f64 / n,
            100.0 * gold_listed as f64 / n,
            100.0 * missed as f64 / n,
            100.0 * gold_first as f64 / gold_listed.max(1) as f64
        );
    }
    let show = |label: &str, keep: &dyn Fn(Option<usize>) -> bool| {
        println!("\n{label}:");
        for (p, rank, top) in results.iter().filter(|r| keep(r.1)).take(errors) {
            let (r, i) = positions[*p];
            let (bn, rom) = &rows[r];
            let before = bn[i.saturating_sub(likhi_engine::xlit::CONTEXT_WORDS)..i].join(" ");
            println!("  [{before}] {} -> want {}  got {}  (rank {})", rom[i], bn[i], top.join(" "), rank.map_or("-".to_string(), |x| x.to_string()));
        }
    };
    show("ranked 2-5 (a better ranker fixes these)", &|r| r.is_some_and(|x| (2..=5).contains(&x)));
    if engine.is_spelled("").is_some() {
        println!("\nfirst choice not correctly spelt, a correctly spelt word in the top five:");
        for (p, rank, top) in results
            .iter()
            .filter(|(_, _, top)| {
                top.first().is_some_and(|w| engine.is_spelled(w) == Some(false))
                    && top.iter().any(|w| engine.is_spelled(w) == Some(true))
            })
            .take(errors)
        {
            let (r, i) = positions[*p];
            let (bn, rom) = &rows[r];
            println!("  {} -> want {}  got {}  (rank {})", rom[i], bn[i], top.join(" "), rank.map_or("-".to_string(), |x| x.to_string()));
        }
    }
    show("not among the candidates at all", &|r| r.is_none());
    Ok(())
}

/// `likhi-eval rerank --dataset banglatlit-<split> --lm <dir>`: how much the sentence model adds to
/// today's ranking, before anything is trained.
///
/// Each held-out word's top 20 are re-ranked by the engine's score plus `w` times the model's score
/// for the candidate after the whole correct sentence so far, plus `c` for a word the model does not
/// know (it then scores as its unknown-word token). The model's normaliser is the same for every
/// candidate of a word, so raw scores rank exactly as probabilities would. Top-1 is reported over a
/// grid of `w` and `c`: choose them on the validation split, then read the test split at that
/// choice -- choosing on the test split would report the best of 36 tries as if it were one.
fn run_rerank(dataset: &str, lm_dir: &Path, limit: Option<usize>, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    use likhi_engine::nextlm::NextLm;
    use likhi_engine::textnorm::{canonical, match_key, to_output};
    const WEIGHTS: [f64; 9] = [0.0, 0.1, 0.2, 0.35, 0.5, 0.75, 1.0, 1.5, 2.0];
    const OOV: [f64; 4] = [0.0, -2.0, -4.0, -8.0];
    let repo = repo();
    let engine = Arc::new(Engine::open(
        &data_dir(&repo),
        EngineOptions { personal: None, ..Default::default() },
    )?);
    let lm = Arc::new(NextLm::open(lm_dir).map_err(|e| e.to_string())?);
    let (mut rows, _) = aligned_rows(&repo, dataset)?;
    if let Some(n) = limit {
        rows.truncate(n);
    }
    let started = Instant::now();
    let rows = Arc::new(rows);
    let next = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let engine = Arc::clone(&engine);
        let lm = Arc::clone(&lm);
        let rows = Arc::clone(&rows);
        let next = Arc::clone(&next);
        handles.push(std::thread::spawn(move || {
            let mut hits = [[0usize; 4]; 9];
            let mut words = 0usize;
            loop {
                let r = next.fetch_add(1, Ordering::Relaxed);
                if r >= rows.len() {
                    return (hits, words);
                }
                let (bn, rom) = &rows[r];
                let mut state = lm.start();
                for i in 0..bn.len() {
                    words += 1;
                    let context: Vec<String> = bn[i.saturating_sub(likhi_engine::xlit::CONTEXT_WORDS)..i].iter().map(|w| to_output(w)).collect();
                    let ranked = engine.ranked(&rom[i], &context, 20);
                    let gold = match_key(&bn[i]);
                    if let Some(gold_at) = ranked.iter().position(|(w, _, _)| match_key(w) == gold) {
                        let ids: Vec<usize> = ranked.iter().map(|(w, _, _)| lm.id(&canonical(w))).collect();
                        let lm_scores = lm.scores(&state, Some(&ids));
                        for (wi, w) in WEIGHTS.iter().enumerate() {
                            for (ci, c) in OOV.iter().enumerate() {
                                let total = |j: usize| {
                                    let unknown = if ids[j] == lm.meta.unk { *c } else { 0.0 };
                                    ranked[j].1 + w * (f64::from(lm_scores[j].1) + unknown)
                                };
                                let mut best = 0;
                                for j in 1..ranked.len() {
                                    if total(j) > total(best) {
                                        best = j;
                                    }
                                }
                                if best == gold_at {
                                    hits[wi][ci] += 1;
                                }
                            }
                        }
                    }
                    lm.step(&mut state, lm.id(&bn[i]));
                }
            }
        }));
    }
    let mut hits = [[0usize; 4]; 9];
    let mut words = 0usize;
    for h in handles {
        let (part, n) = h.join().map_err(|_| "a worker thread panicked")?;
        words += n;
        for (row, part_row) in hits.iter_mut().zip(part) {
            for (cell, p) in row.iter_mut().zip(part_row) {
                *cell += p;
            }
        }
    }
    println!("[likhi on {dataset}, rerank] words {words}; top-1 % by model weight (rows) and unknown-word offset (columns)  {:.1}s", started.elapsed().as_secs_f64());
    print!("     w ");
    for c in OOV {
        print!("  c={c:>4}");
    }
    println!();
    for (wi, w) in WEIGHTS.iter().enumerate() {
        print!("  {w:>4.2} ");
        for cell in hits[wi] {
            print!("  {:>6.2}", 100.0 * cell as f64 / words.max(1) as f64);
        }
        println!();
    }
    Ok(())
}

/// `likhi-eval complete --dataset banglatlit-<split> --model <dir>`: the Likhi model's completion
/// mode, judged as a next-word model is: for every
/// held-out chat word, after the correct words before it and 0 to 3 of its letters, whether the
/// model's single best completion is the word. With no letters this is next-word prediction.
///
/// Also shown/right at a confidence bar, where confidence is the best completion's share of the
/// beam -- an overestimate of its share of everything, since the beam holds only the likeliest few.
fn run_complete(dataset: &str, model_dir: &Path, limit: Option<usize>, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    use likhi_engine::textnorm::match_key;
    use likhi_engine::xlit::Xlit;
    const BARS: [f32; 3] = [0.0, 0.5, 0.7];
    let repo = repo();
    let x = Arc::new(Xlit::open(model_dir).map_err(|e| e.to_string())?);
    if !x.reads_context() {
        return Err("that is not a Likhi model: it has no completion mode".into());
    }
    let (mut rows, _) = aligned_rows(&repo, dataset)?;
    if let Some(n) = limit {
        rows.truncate(n);
    }
    let positions: Vec<(usize, usize)> =
        rows.iter().enumerate().flat_map(|(r, (bn, _))| (0..bn.len()).map(move |i| (r, i))).collect();
    let started = Instant::now();
    let rows = Arc::new(rows);
    let positions = Arc::new(positions);
    let next = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let x = Arc::clone(&x);
        let rows = Arc::clone(&rows);
        let positions = Arc::clone(&positions);
        let next = Arc::clone(&next);
        handles.push(std::thread::spawn(move || {
            // [letters][bar] -> (positions, shown, right)
            let mut t = [[(0usize, 0usize, 0usize); 3]; 4];
            loop {
                let p = next.fetch_add(1, Ordering::Relaxed);
                if p >= positions.len() {
                    return t;
                }
                let (r, i) = positions[p];
                let (bn, rom) = &rows[r];
                let context = &bn[i.saturating_sub(3)..i];
                let gold = match_key(&bn[i]);
                let letters: Vec<char> = rom[i].chars().collect();
                for (n, cells) in t.iter_mut().enumerate() {
                    if n >= letters.len() {
                        break;
                    }
                    let prefix: String = letters[..n].iter().collect();
                    let enc = x.encode_ctx(true, context, &prefix);
                    let beam = x.beam_search_complete(4, 4, &enc);
                    let Some((best, _)) = beam.first() else {
                        for c in cells.iter_mut() {
                            c.0 += 1;
                        }
                        continue;
                    };
                    // Back to sequence probabilities: the beam's scores are per character.
                    let probs: Vec<f32> = beam.iter().map(|(w, lp)| (lp * (w.chars().count() + 1) as f32).exp()).collect();
                    let share = probs[0] / probs.iter().sum::<f32>().max(f32::MIN_POSITIVE);
                    let right = match_key(best) == gold;
                    for (c, bar) in cells.iter_mut().zip(BARS) {
                        c.0 += 1;
                        if share >= bar {
                            c.1 += 1;
                            c.2 += usize::from(right);
                        }
                    }
                }
            }
        }));
    }
    let mut t = [[(0usize, 0usize, 0usize); 3]; 4];
    for h in handles {
        let part = h.join().map_err(|_| "a worker thread panicked")?;
        for (row, prow) in t.iter_mut().zip(part) {
            for (c, pc) in row.iter_mut().zip(prow) {
                c.0 += pc.0;
                c.1 += pc.1;
                c.2 += pc.2;
            }
        }
    }
    println!("[likhi model on {dataset}, complete] one guess; shown / right when shown, by beam-share bar  {:.1}s", started.elapsed().as_secs_f64());
    for (n, row) in t.iter().enumerate() {
        let cells: Vec<String> = row
            .iter()
            .zip(BARS)
            .map(|(c, bar)| format!("{bar:.1}: {:5.1}% / {:5.1}%", 100.0 * c.1 as f64 / c.0.max(1) as f64, 100.0 * c.2 as f64 / c.1.max(1) as f64))
            .collect();
        println!("  after {n} letter(s)   {}", cells.join("    "));
    }
    Ok(())
}

/// `likhi-eval predict --dataset banglatlit-<split> (--lm <dir> | --model <dir>)`: the next-word
/// predictor exactly as the keyboard asks it (`nextlm::Predictor`), from the separate LSTM or from
/// the Likhi model's word head. For every held-out chat word, after the correct words before it and
/// 0 to 3 of its letters (narrowed by coarse phonetic key, as the service does): whether the first
/// guess is the word, whether either of the two offered is, and at the Tab guess's bar (a share of
/// at least 0.5) how often a guess is shown and how often it is right. With a spelling list in the
/// models folder, again over the words whose reference is correctly spelt: a model that offers only
/// correct spellings cannot name chat's misspellings, and should not. `LIKHI_PREDICT_WORDS=N` keeps
/// only the last N words of context, to see what context length is worth. `LIKHI_PREDICT_SKIP=<file>`
/// leaves out the sentences listed in it (one a line, words separated by spaces) -- held-out
/// sentences a model was trained on, which would flatter it.
fn run_predict(dataset: &str, lm: Option<&Path>, model: Option<&Path>, limit: Option<usize>) -> Result<(), Box<dyn std::error::Error>> {
    use likhi_engine::nextlm::Predictor;
    use likhi_engine::romankey::{key_from_roman, Level};
    use likhi_engine::textnorm::{match_key, normalize_roman, to_output};
    let p = match (lm, model) {
        (Some(dir), _) => Predictor::open(dir).map_err(|e| e.0)?,
        (None, Some(dir)) => {
            let x = likhi_engine::xlit::Xlit::open(dir).map_err(|e| e.to_string())?;
            Predictor::from_likhi(Arc::new(x)).map_err(|e| e.0)?
        }
        _ => return Err("predict needs --lm <next-word model> or --model <Likhi model>".into()),
    };
    let repo = repo();
    let (mut rows, _) = aligned_rows(&repo, dataset)?;
    if let Some(n) = limit {
        rows.truncate(n);
    }
    let spelling = likhi_engine::lexicon::Table::open(&data_dir(&repo).join("lexicon").join("spelling.lkx")).ok();
    let keep_words: usize = std::env::var("LIKHI_PREDICT_WORDS").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let skip: std::collections::HashSet<String> = std::env::var_os("LIKHI_PREDICT_SKIP")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.lines().map(|l| match_key(l.trim())).collect())
        .unwrap_or_default();
    let before = rows.len();
    rows.retain(|(bn, _)| !skip.contains(&match_key(&bn.join(" "))));
    if rows.len() < before {
        println!("left out {} sentences listed in LIKHI_PREDICT_SKIP", before - rows.len());
    }
    let started = Instant::now();
    // [all, correctly spelt reference][letters] ->
    //     (asked, first right, either of two right, shown at 0.5, right when shown)
    let mut tables = [[(0usize, 0usize, 0usize, 0usize, 0usize); 4]; 2];
    let wait = std::time::Duration::from_secs(10);
    for (bn, rom) in &rows {
        for i in 0..bn.len() {
            let context: Vec<String> = bn[i.saturating_sub(keep_words)..i].iter().map(|w| to_output(w)).collect();
            let gold = match_key(&bn[i]);
            let spelled = spelling.as_ref().is_some_and(|s| s.get(&likhi_engine::textnorm::canonical(&bn[i])).is_some());
            let letters: Vec<char> = rom[i].chars().collect();
            if p.best_within(&context, "", 1, wait).is_none() {
                return Err("the predictor did not answer".into());
            }
            for n in 0..letters.len().min(4) {
                let prefix: String = letters[..n].iter().collect();
                let typed = key_from_roman(&normalize_roman(&prefix), Level::Coarse);
                let guesses = p.best(&context, &typed, 2).unwrap_or_default();
                let right = |k: usize| guesses.get(k).is_some_and(|(w, _)| match_key(w) == gold);
                let shown = guesses.first().is_some_and(|g| g.1 >= 0.5);
                for (which, table) in tables.iter_mut().enumerate() {
                    if which == 1 && !spelled {
                        continue;
                    }
                    let cell = &mut table[n];
                    cell.0 += 1;
                    cell.1 += usize::from(right(0));
                    cell.2 += usize::from(right(0) || right(1));
                    if shown {
                        cell.3 += 1;
                        cell.4 += usize::from(right(0));
                    }
                }
            }
        }
    }
    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    println!("[predictor on {dataset}] {:.1}s", started.elapsed().as_secs_f64());
    for (which, table) in tables.iter().enumerate() {
        if which == 1 {
            if spelling.is_none() {
                break;
            }
            println!("where the reference is correctly spelt ({:.1}% of next words):", pct(table[0].0, tables[0][0].0));
        }
        for (n, c) in table.iter().enumerate() {
            println!(
                "  after {n} letter(s): first {:5.1}%  either of two {:5.1}%  | share>=0.5 shown {:5.1}%, right when shown {:5.1}%",
                pct(c.1, c.0),
                pct(c.2, c.0),
                pct(c.3, c.0),
                pct(c.4, c.3)
            );
        }
    }
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
        likhi-eval replay    --dataset <name> [--k 5] [--limit N] [--threads N]\n  \
        likhi-eval context   --dataset banglatlit-<split> [--limit N] [--errors N] [--threads N]\n  \
        likhi-eval rerank    --dataset banglatlit-<split> --lm <dir> [--limit N] [--threads N]\n  \
        likhi-eval complete  --dataset banglatlit-<split> --model <dir> [--limit N] [--threads N]\n\
        \n  word sets    : dakshina-<split>, aksharantar-<split>, banglatlit-<split>-words,\n  \
                         feedback-words; any with a +grouped suffix\n  \
        sentence sets: dakshina-<split>, banglatlit-<split>";
    if argv.len() < 2 || !matches!(argv[1].as_str(), "words" | "sentences" | "replay" | "context" | "rerank" | "complete" | "predict") {
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
        lm: None,
        model: None,
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
            "--lm" => {
                args.lm = next.map(PathBuf::from);
                i += 2;
            }
            "--model" => {
                args.model = next.map(PathBuf::from);
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
        "context" => run_context(&args.dataset, args.limit, args.threads, args.errors),
        "rerank" => match &args.lm {
            Some(dir) => run_rerank(&args.dataset, dir, args.limit, args.threads),
            None => Err("rerank needs --lm <exported model folder>".into()),
        },
        "predict" => run_predict(&args.dataset, args.lm.as_deref(), args.model.as_deref(), args.limit),
        "complete" => match &args.model {
            Some(dir) => run_complete(&args.dataset, dir, args.limit, args.threads),
            None => Err("complete needs --model <exported Likhi model folder>".into()),
        },
        _ => unreachable!("checked above"),
    };
    if let Err(e) = result {
        eprintln!("[eval] failed: {e}");
        std::process::exit(1);
    }
}
