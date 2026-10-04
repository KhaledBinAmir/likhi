//! The Rust port of the neural next-word model against the model it was exported from.
//!
//! `vectors.json` holds the exporter's own answers -- the ten likeliest next words and their
//! log-probabilities, from the 8-bit model in PyTorch -- for sixty held-out contexts. The port must
//! give the same words in the same order and the same probabilities to within rounding. Skipped
//! when no exported model is at hand: set `LIKHI_NEXTLM` to the folder holding model.lkm,
//! vocab.txt and vectors.json.

use std::path::PathBuf;
use std::time::Instant;

use likhi_engine::nextlm::{log_softmax, NextLm, Predictor};

fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LIKHI_NEXTLM")?);
    dir.join("model.lkm").exists().then_some(dir)
}

#[derive(serde::Deserialize)]
struct Vector {
    context: Vec<usize>,
    top: Vec<usize>,
    logp: Vec<f32>,
}

#[test]
fn matches_the_exported_model() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIPPING: set LIKHI_NEXTLM to an exported model folder");
        return;
    };
    let lm = NextLm::open(&dir).expect("model opens");
    let vectors: Vec<Vector> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("vectors.json")).expect("vectors")).expect("json");
    let (mut worst, mut order_mismatches) = (0.0f32, 0usize);
    for v in &vectors {
        // The context starts with the start marker, which `start` has already read.
        let mut s = lm.start();
        for &id in &v.context[1..] {
            lm.step(&mut s, id);
        }
        let mut scores = lm.scores(&s, None);
        log_softmax(&mut scores);
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let got: Vec<usize> = scores.iter().take(10).map(|s| s.0).collect();
        // Near-ties may swap places between two float implementations; the words must agree and
        // the probability of each expected word must match.
        if got != v.top {
            order_mismatches += 1;
        }
        for (id, want) in v.top.iter().zip(&v.logp) {
            let have = scores.iter().find(|s| s.0 == *id).map(|s| s.1).expect("scored");
            worst = worst.max((have - want).abs());
        }
    }
    eprintln!("{} contexts; worst log-prob difference {worst:.5}; top-10 order differed in {order_mismatches}", vectors.len());
    assert!(worst < 2e-3, "log-probabilities drift by {worst}");
    assert!(order_mismatches <= vectors.len() / 20, "{order_mismatches} contexts ranked differently");
}

#[test]
fn is_fast_enough_for_a_keyboard() {
    let Some(dir) = model_dir() else { return };
    let lm = NextLm::open(&dir).expect("model opens");
    let words = ["আমি", "আজকে", "অফিসে", "অনেক", "কাজ", "ছিল", "তাই", "বাসায়"];
    let mut s = lm.start();
    let started = Instant::now();
    for w in words {
        lm.step(&mut s, lm.id(w));
    }
    let per_word = started.elapsed() / words.len() as u32;
    // The first pass over the output layer faults its pages in from the mapped file; after that
    // they stay resident while the keyboard is in use, so the second pass is the one that counts.
    let started = Instant::now();
    let _ = lm.scores(&s, None);
    let cold = started.elapsed();
    let started = Instant::now();
    let all = lm.scores(&s, None);
    let full = started.elapsed();
    let subset: Vec<usize> = (0..2000).map(|i| i * 17 % lm.meta.vocab).collect();
    let started = Instant::now();
    let _ = lm.scores(&s, Some(&subset));
    let part = started.elapsed();
    eprintln!(
        "reading one word: {per_word:?}; scoring all {} words: {full:?} ({cold:?} the first time); scoring 2,000: {part:?}",
        all.len()
    );
    assert!(per_word.as_millis() < 20, "reading a word took {per_word:?}");
}

/// Ask until the worker has the answer, as the keyboard would on its next keystroke.
fn answered(p: &Predictor, context: &[String], key: &str, k: usize) -> Vec<(String, f32)> {
    let started = Instant::now();
    loop {
        if let Some(words) = p.best(context, key, k) {
            return words;
        }
        assert!(started.elapsed().as_secs() < 10, "no answer for {context:?}");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn predicts_off_the_keystroke_path_and_extends_by_one_word() {
    let Some(dir) = model_dir() else { return };
    let words = |ws: &[&str]| ws.iter().map(|w| w.to_string()).collect::<Vec<String>>();
    let started = Instant::now();
    let p = Predictor::open(&dir).expect("model opens");
    let opened = started.elapsed();
    let first = words(&["আমি", "আজকে"]);
    // The first question cannot have its answer yet; asking starts the work.
    assert!(p.best(&first, "", 3).is_none());
    let started = Instant::now();
    let a = answered(&p, &first, "", 3);
    eprintln!("opened in {opened:?}; first answer (with the key index built) after {:?}: {a:?}", started.elapsed());
    assert_eq!(a.len(), 3);
    assert!(a.windows(2).all(|w| w[0].1 >= w[1].1), "likeliest first");
    // One word more: extended from the kept state, which must equal reading it all afresh.
    let longer = words(&["আমি", "আজকে", "অফিসে"]);
    p.prepare(&longer);
    let started = Instant::now();
    let extended = answered(&p, &longer, "", 5);
    eprintln!("one more word after {:?}", started.elapsed());
    let fresh = Predictor::open(&dir).expect("model opens");
    assert_eq!(answered(&fresh, &longer, "", 5), extended);
    // Letters narrow it to the words they could begin, and the shares are of those words alone.
    use likhi_engine::romankey::{key_from_bangla, key_from_roman, Level};
    let typed = key_from_roman("k", Level::Coarse);
    let k_words = answered(&p, &longer, &typed, 5);
    assert!(!k_words.is_empty());
    for (w, _) in &k_words {
        let key = key_from_bangla(w, Level::Coarse);
        assert!(key.starts_with(&typed), "{w} ({key}) does not fit k ({typed})");
    }
    let shares: f32 = k_words.iter().map(|w| w.1).sum();
    assert!(shares <= 1.0001, "shares of the fitting words add up to {shares}");
    eprintln!("after k: {k_words:?}");
    // Waiting for an answer being worked out returns as soon as it is ready, not at the deadline.
    let next = words(&["আমি", "আজকে", "অফিসে", "যাব"]);
    p.prepare(&next);
    let started = Instant::now();
    let waited = p.best_within(&next, "", 1, std::time::Duration::from_secs(5));
    eprintln!("best_within answered after {:?}", started.elapsed());
    assert!(waited.is_some() && started.elapsed().as_millis() < 1000);
}

/// Through the service, as the keyboard uses it: committing a word prepares what follows it, and
/// the next word's first letter is then answered by the model rather than the counted table.
#[test]
fn a_committed_word_prepares_the_next_words_prediction() {
    let Some(dir) = model_dir() else { return };
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("repo").join("models").join("rust");
    if !data.join("indicxlit").join("model.lkw").exists() {
        return;
    }
    use likhi_engine::romankey::{key_from_roman, Level};
    let opts = likhi_engine::core::EngineOptions { personal: None, ..Default::default() };
    let engine = std::sync::Arc::new(likhi_engine::core::Engine::open(&data, opts).expect("engine opens"));
    let svc = likhi_engine::service::SuggestService::new(engine, 64).with_predictor(Predictor::open(&dir).expect("model"));
    let before = vec!["আমি".to_string(), "আজকে".to_string()];
    svc.learn("office", "অফিসে", &before, None);
    // What the keyboard sends for the next word: its last two committed words.
    let sent = vec!["আজকে".to_string(), "অফিসে".to_string()];
    let reference = Predictor::open(&dir).expect("model");
    let full = vec!["আমি".to_string(), "আজকে".to_string(), "অফিসে".to_string()];
    let want: Vec<String> =
        answered(&reference, &full, &key_from_roman("k", Level::Coarse), 2).into_iter().map(|w| w.0).collect();
    let started = Instant::now();
    loop {
        let (got, from_model) = svc.next_completions("k", &sent, None);
        let got: Vec<String> = got.into_iter().map(|w| w.0).collect();
        if got == want {
            assert!(from_model);
            break;
        }
        assert!(started.elapsed().as_secs() < 10, "the service offers {got:?}, the model {want:?}");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    eprintln!("after committing, k is answered from the whole context: {want:?}");
    // The document's own text, when the keyboard can read it, is the context instead -- from its
    // last line break, so an earlier line is not read as the start of this sentence.
    let before = format!("{}\n{} ", "\u{0986}\u{0997}\u{09C7}\u{09B0}", full.join(" "));
    let got: Vec<String> = svc.next_completions("k", &[], Some(&before)).0.into_iter().map(|w| w.0).collect();
    assert_eq!(got, want);
}