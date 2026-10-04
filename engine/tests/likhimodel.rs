//! The Likhi model as the engine runs it, against the exporter's own answers.
//!
//! `vectors.json` holds PyTorch's teacher-forced log P(word | the sentence so far, the letters) for
//! held-out examples, from the model that was exported. The engine's transformer -- the same code
//! that runs IndicXlit -- must give the same numbers from the exported file. Skipped unless
//! `LIKHI_MODEL` names an exported model folder: model.lkw, the two vocabularies, vectors.json.

use std::path::PathBuf;
use std::time::Instant;

use likhi_engine::xlit::Xlit;

fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LIKHI_MODEL")?);
    dir.join("model.lkw").exists().then_some(dir)
}

#[derive(serde::Deserialize)]
struct Vector {
    mode: String,
    context: String,
    roman: String,
    target: String,
    logp: f32,
}

fn words(s: &str) -> Vec<String> {
    s.split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

#[test]
fn matches_the_exported_model() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIPPING: set LIKHI_MODEL to an exported Likhi model folder");
        return;
    };
    let x = Xlit::open(&dir).expect("model opens");
    assert!(x.reads_context(), "this is not a Likhi model: it has no mode tokens");
    let vectors: Vec<Vector> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("vectors.json")).expect("vectors")).expect("json");
    let mut worst = 0.0f32;
    for v in &vectors {
        let enc = x.encode_ctx(v.mode == "c", &words(&v.context), &v.roman);
        let got = x.score_candidates(std::slice::from_ref(&v.target), &enc)[0];
        worst = worst.max((got - v.logp).abs());
    }
    eprintln!("{} examples; worst log-probability difference {worst:.5}", vectors.len());
    assert!(worst < 2e-3, "the port drifts from the exported model by {worst}");
}

#[test]
fn transliterates_in_context_quickly() {
    let Some(dir) = model_dir() else { return };
    let x = Xlit::open(&dir).expect("model opens");
    let context = words("\u{0986}\u{09AE}\u{09BF} \u{0986}\u{099C}\u{0995}\u{09C7}");
    for roman in ["office", "jabo", "khub", "kaj", "korchi"] {
        let started = Instant::now();
        let enc = x.encode_ctx(false, &context, roman);
        let beam = x.beam_search(roman, 4, 4, &enc);
        let took = started.elapsed();
        eprintln!("{roman}: {took:?} {beam:?}");
        assert!(took.as_millis() < 200, "{roman} took {took:?}");
    }
}

/// The word head in 8 bits (`xlit::quantize_word_head`) ranks the words as the floats do, and
/// leaves the transliterating half of the file exactly as it was.
#[test]
fn word_head_survives_quantization() {
    let Some(dir) = model_dir() else { return };
    let x = Xlit::open(&dir).expect("model opens");
    if x.words().is_empty() || dir.join("word_head.lkq").exists() {
        eprintln!("SKIPPING: needs an export with its word head still in floats");
        return;
    }
    let copy = std::env::temp_dir().join(format!("likhi-quantize-test-{}", std::process::id()));
    std::fs::create_dir_all(&copy).expect("temp dir");
    for f in ["model.lkw", "source_vocabulary.json", "target_vocabulary.json", "word_vocabulary.json"] {
        std::fs::copy(dir.join(f), copy.join(f)).expect("copy");
    }
    let (before, after) = likhi_engine::xlit::quantize_word_head(&copy).expect("quantizes");
    assert!(after < before, "model.lkw did not shrink");
    let q = Xlit::open(&copy).expect("quantized model opens");
    for context in ["", "\u{0986}\u{09AE}\u{09BF}", "\u{0986}\u{09AE}\u{09BF} \u{0986}\u{099C}\u{0995}\u{09C7}"] {
        let ctx = words(context);
        let a = x.word_scores(&x.encode_ctx(true, &ctx, "")).expect("scores");
        let b = q.word_scores(&q.encode_ctx(true, &ctx, "")).expect("scores");
        let top = |s: &[f32]| {
            let mut ids: Vec<usize> = (0..s.len()).collect();
            ids.sort_by(|i, j| s[*j].total_cmp(&s[*i]));
            ids.truncate(10);
            ids
        };
        let (ta, tb) = (top(&a), top(&b));
        assert_eq!(ta[0], tb[0], "a different first word after [{context}]");
        assert!(ta.iter().filter(|i| tb.contains(i)).count() >= 8, "the top ten moved after [{context}]");
        // A word-aware model reads its context words from the same table, so its transliteration
        // moves a little too: the same words in the same order, scores within a few hundredths.
        let enc = (x.encode_ctx(false, &ctx, "jabo"), q.encode_ctx(false, &ctx, "jabo"));
        let (ba, bb) = (x.beam_search("jabo", 4, 4, &enc.0), q.beam_search("jabo", 4, 4, &enc.1));
        let names = |b: &[(String, f32)]| b.iter().map(|w| w.0.clone()).collect::<Vec<_>>();
        assert_eq!(names(&ba), names(&bb), "transliteration changed after [{context}]");
        let drift = ba.iter().zip(&bb).map(|(a, b)| (a.1 - b.1).abs()).fold(0.0f32, f32::max);
        assert!(drift < 0.05, "transliteration scores moved by {drift} after [{context}]");
    }
    let _ = std::fs::remove_dir_all(&copy);
}

/// The same model's word head as the next-word predictor: one model doing both jobs.
#[test]
fn predicts_the_next_word_from_its_word_head() {
    let Some(dir) = model_dir() else { return };
    let x = std::sync::Arc::new(Xlit::open(&dir).expect("model opens"));
    if x.words().is_empty() {
        eprintln!("SKIPPING: this export has no word head");
        return;
    }
    let p = likhi_engine::nextlm::Predictor::from_likhi(x).expect("predictor");
    let context = words("\u{0986}\u{09AE}\u{09BF} \u{0986}\u{099C}\u{0995}\u{09C7}");
    let started = Instant::now();
    let next = p.best_within(&context, "", 3, std::time::Duration::from_secs(5)).expect("an answer");
    eprintln!("after the context, in {:?}: {next:?}", started.elapsed());
    assert_eq!(next.len(), 3);
    assert!(next.windows(2).all(|w| w[0].1 >= w[1].1));
    assert!(next.iter().map(|w| w.1).sum::<f32>() <= 1.0001);
}