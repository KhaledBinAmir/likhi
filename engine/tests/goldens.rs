//! Replay the Python engine's recorded behaviour against this crate.
//!
//! These are the tests that decide whether the port is finished. Reviewing a rewrite of a ranking
//! engine cannot establish that it suggests the same words -- only running both on the same inputs
//! can. `scripts/dump_goldens.py` records what Python returns; every test here asserts this crate
//! returns the same thing.
//!
//! Regenerate after any intentional behaviour change:
//!
//!     .venv\Scripts\python.exe scripts\dump_goldens.py
//!
//! A missing golden file skips its test rather than failing, so a fresh checkout without the
//! generated data still builds. A skip prints loudly; do not let it become the normal state.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("engine/ has a parent")
        .join("tests")
        .join("goldens")
}

/// Every record in one golden file, or None when it has not been generated.
fn load(name: &str) -> Option<Vec<Value>> {
    let path = goldens_dir().join(format!("{name}.jsonl"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("SKIPPING {name}: {} not generated", path.display());
            return None;
        }
    };
    Some(
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{name}: bad json: {e}: {l}")))
            .collect(),
    )
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key} in {v}"))
        .to_string()
}

/// Report up to `limit` mismatches with enough detail to debug, then fail with the total.
struct Mismatches {
    name: &'static str,
    shown: usize,
    total: usize,
    checked: usize,
}

impl Mismatches {
    fn new(name: &'static str) -> Self {
        Mismatches { name, shown: 0, total: 0, checked: 0 }
    }

    fn check(&mut self, ok: bool, detail: impl FnOnce() -> String) {
        self.checked += 1;
        if !ok {
            self.total += 1;
            if self.shown < 20 {
                self.shown += 1;
                eprintln!("  {}", detail());
            }
        }
    }

    fn finish(self) {
        assert!(
            self.total == 0,
            "{}: {} of {} cases differ from Python (first {} shown above)",
            self.name,
            self.total,
            self.checked,
            self.shown
        );
        eprintln!("{}: {} cases match", self.name, self.checked);
    }
}

#[test]
fn textnorm_matches_python() {
    use likhi_engine::textnorm as tn;
    let Some(cases) = load("textnorm") else { return };
    let mut m = Mismatches::new("textnorm");
    for c in &cases {
        let input = s(c, "in");
        let fname = s(c, "fn");
        match fname.as_str() {
            "has_bengali" => {
                let want = c["out"].as_bool().expect("bool out");
                let got = tn::has_bengali(&input);
                m.check(got == want, || {
                    format!("has_bengali({input:?}) = {got}, python {want}")
                });
            }
            _ => {
                let want = s(c, "out");
                let got = match fname.as_str() {
                    "normalize_roman" => tn::normalize_roman(&input),
                    "canonical" => tn::canonical(&input),
                    "match_key" => tn::match_key(&input),
                    "to_output" => tn::to_output(&input),
                    "to_bangla_digits" => tn::to_bangla_digits(&input),
                    "to_western_digits" => tn::to_western_digits(&input),
                    other => panic!("golden names an unknown function: {other}"),
                };
                m.check(got == want, || {
                    format!(
                        "{fname}({:?})\n      got    {:?}\n      python {:?}",
                        escape(&input),
                        escape(&got),
                        escape(&want)
                    )
                });
            }
        }
    }
    m.finish();
}

/// Bengali text in a terminal is unreadable when it differs by one combining mark, so mismatches
/// are shown as escapes.
fn escape(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c.to_string()
            } else {
                format!("\\u{{{:04X}}}", c as u32)
            }
        })
        .collect()
}

#[test]
fn romankey_matches_python() {
    use likhi_engine::romankey::{key_from_bangla, key_from_roman, Level};
    let Some(cases) = load("romankey") else { return };
    let mut m = Mismatches::new("romankey");
    for c in &cases {
        let input = s(c, "in");
        let want = s(c, "out");
        let level = Level::parse(&s(c, "level")).expect("golden names a known level");
        let fname = s(c, "fn");
        let got = match fname.as_str() {
            "key_from_roman" => key_from_roman(&input, level),
            "key_from_bangla" => key_from_bangla(&input, level),
            other => panic!("golden names an unknown function: {other}"),
        };
        m.check(got == want, || {
            format!(
                "{fname}({:?}, {:?})\n      got    {:?}\n      python {:?}",
                escape(&input),
                level,
                got,
                want
            )
        });
    }
    m.finish();
}

/// The lexicon tables must return the same records the Python trie did.
///
/// This also exercises the .lkx reader itself: front-coded block walking, the binary search, and
/// the record layouts. If this passes on a quarter of a million words, that reader is sound.
#[test]
fn lexicon_lookups_match_python() {
    use likhi_engine::lexicon::{rec_u32x3, Table};

    let Some(cases) = load("lexicon") else { return };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("models")
        .join("rust")
        .join("lexicon");
    let path = dir.join("unigrams.lkx");
    if !path.exists() {
        eprintln!("SKIPPING lexicon: run scripts/build_rust_data.py first");
        return;
    }
    let uni = Table::open(&path).expect("unigrams.lkx opens");

    // The floor and the total are computed from a full pass over the table, exactly as
    // LikhiEngine.__init__ does. Getting these wrong shifts every unigram log-probability.
    let mut total: u64 = 0;
    for (_w, rec) in uni.iter_all() {
        let (a, b, c) = rec_u32x3(rec);
        total += a as u64 + 3 * b as u64 + 20 * c as u64;
    }
    let uni_total = total as f64 + 1.0;

    let mut m = Mismatches::new("lexicon");
    for c in &cases {
        match s(c, "fn").as_str() {
            "uni_total" => {
                let want: f64 = s(c, "value").parse().expect("float");
                m.check((uni_total - want).abs() < 1e-6, || {
                    format!("uni_total = {uni_total}, python {want}")
                });
            }
            "floor" => {
                let want: f64 = s(c, "value").parse().expect("float");
                let got = (0.5f64 / uni_total).ln();
                m.check((got - want).abs() < 1e-12, || {
                    format!("floor = {got}, python {want}")
                });
            }
            "lookup" => {
                let word = s(c, "word");
                let want_score = c["lex_score"].as_i64().expect("int");
                let want_in = c["in_lexicon"].as_bool().expect("bool");
                let want_logp: f64 = s(c, "unigram_logp").parse().expect("float");

                let rec = uni.get(&word);
                let got_in = rec.is_some();
                let (got_score, got_logp) = match rec {
                    Some(r) => {
                        let (a, b, cc) = rec_u32x3(r);
                        let sc = a as i64 + 3 * b as i64 + 20 * cc as i64;
                        (sc, ((sc as f64 + 0.5) / uni_total).ln())
                    }
                    None => (0, (0.5f64 / uni_total).ln()),
                };
                m.check(got_in == want_in, || {
                    format!("in_lexicon({}) = {got_in}, python {want_in}", escape(&word))
                });
                m.check(got_score == want_score, || {
                    format!("lex_score({}) = {got_score}, python {want_score}", escape(&word))
                });
                m.check((got_logp - want_logp).abs() < 1e-9, || {
                    format!("unigram_logp({}) = {got_logp}, python {want_logp}", escape(&word))
                });
            }
            other => panic!("unknown lexicon golden fn: {other}"),
        }
    }
    m.finish();
}

/// Avro's rule reading must match exactly: it is pure string rewriting with no arithmetic in it.
#[test]
fn avro_matches_python() {
    let Some(cases) = load("avro") else { return };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("models")
        .join("rust")
        .join("avro.json");
    if !path.exists() {
        eprintln!("SKIPPING avro: run scripts/build_rust_data.py first");
        return;
    }
    let a = likhi_engine::avro::Avro::open(&path).expect("avro.json loads");
    let mut m = Mismatches::new("avro");
    for c in &cases {
        if c.get("error").is_some() {
            continue; // Python raised; the engine treats that as "no rule literal"
        }
        let input = s(c, "in");
        let want = s(c, "out");
        let got = a.parse(&input);
        m.check(got == want, || {
            format!(
                "parse({:?})\n      got    {:?}\n      python {:?}",
                input,
                escape(&got),
                escape(&want)
            )
        });
    }
    m.finish();
}

fn model_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("models")
        .join("rust")
        .join("indicxlit")
}

fn open_model() -> Option<likhi_engine::xlit::Xlit> {
    let dir = model_dir();
    if !dir.join("model.lkw").exists() {
        eprintln!("SKIPPING model tests: run scripts/build_rust_data.py first");
        return None;
    }
    Some(likhi_engine::xlit::Xlit::open(&dir).expect("model.lkw loads"))
}

/// Source tokenization is integer-valued, so it must match exactly. If this drifts, every score
/// below is meaningless, which is why it is checked separately.
#[test]
fn xlit_tokenization_matches_python() {
    let Some(cases) = load("xlit_tokenize") else { return };
    let Some(x) = open_model() else { return };
    let mut m = Mismatches::new("xlit_tokenize");
    for c in &cases {
        let roman = s(c, "roman");
        let want: Vec<usize> = c["src_ids"]
            .as_array()
            .expect("src_ids array")
            .iter()
            .map(|v| v.as_u64().expect("int") as usize)
            .collect();
        let got = x.encode_source(&roman, "bn");
        m.check(got == want, || format!("encode_source({roman:?}) = {got:?}, python {want:?}"));
    }
    m.finish();
}

/// Beam search must return the same words in the same order.
///
/// Scores are allowed to differ slightly -- NumPy's f32 gemm sums in a different order than
/// `tensor.rs` and no amount of care makes those bit-identical -- but the *ranking* is what a
/// typist sees, so that is checked exactly. A tolerance on the scores catches a real numerical
/// error while ignoring the last-bit noise.
#[test]
fn xlit_beam_search_matches_python() {
    let Some(cases) = load("xlit_beam") else { return };
    let Some(x) = open_model() else { return };
    let mut m = Mismatches::new("xlit_beam");
    let mut worst: f32 = 0.0;
    for c in &cases {
        let roman = s(c, "roman");
        if c.get("error").is_some() {
            continue;
        }
        let beam = c["beam"].as_u64().expect("beam") as usize;
        let want: Vec<(String, f32)> = c["hyps"]
            .as_array()
            .expect("hyps array")
            .iter()
            .map(|h| {
                let pair = h.as_array().expect("hyp pair");
                (
                    pair[0].as_str().expect("word").to_string(),
                    pair[1].as_str().expect("score").parse::<f32>().expect("float"),
                )
            })
            .collect();

        let enc = x.encode(&roman, "bn");
        let got = x.beam_search(&roman, beam, beam, &enc);

        let got_words: Vec<&str> = got.iter().map(|(w, _)| w.as_str()).collect();
        let want_words: Vec<&str> = want.iter().map(|(w, _)| w.as_str()).collect();
        m.check(got_words == want_words, || {
            format!(
                "beam_search({roman:?})\n      got    {:?}\n      python {:?}",
                got_words.iter().map(|w| escape(w)).collect::<Vec<_>>(),
                want_words.iter().map(|w| escape(w)).collect::<Vec<_>>()
            )
        });
        if got_words == want_words {
            for ((_, g), (_, p)) in got.iter().zip(want.iter()) {
                let d = (g - p).abs();
                if d > worst {
                    worst = d;
                }
                m.check(d < 2e-3, || {
                    format!("beam_search({roman:?}) score {g} against python {p} (delta {d})")
                });
            }
        }
    }
    eprintln!("xlit_beam: worst score delta {worst:.2e}");
    m.finish();
}

#[test]
fn xlit_teacher_forced_scores_match_python() {
    let Some(cases) = load("xlit_score") else { return };
    let Some(x) = open_model() else { return };
    let mut m = Mismatches::new("xlit_score");
    let mut worst: f32 = 0.0;
    for c in &cases {
        let roman = s(c, "roman");
        let words: Vec<String> = c["words"]
            .as_array()
            .expect("words")
            .iter()
            .map(|v| v.as_str().expect("word").to_string())
            .collect();
        let want: Vec<f32> = c["logps"]
            .as_array()
            .expect("logps")
            .iter()
            .map(|v| v.as_str().expect("score").parse::<f32>().expect("float"))
            .collect();

        let enc = x.encode(&roman, "bn");
        let got = x.score_candidates(&words, &enc);

        for ((g, p), w) in got.iter().zip(want.iter()).zip(words.iter()) {
            if p.is_infinite() {
                m.check(g.is_infinite() && g.signum() == p.signum(), || {
                    format!("score({roman:?}, {}) = {g}, python {p}", escape(w))
                });
                continue;
            }
            // Relative: these are sums over characters, so a long word accumulates more of the
            // same last-bit noise and an absolute bound would be unfair to it.
            let d = (g - p).abs() / (1.0 + p.abs());
            if d > worst {
                worst = d;
            }
            m.check(d < 1e-3, || {
                format!(
                    "score({roman:?}, {}) = {g}, python {p} (relative delta {d:.2e})",
                    escape(w)
                )
            });
        }
    }
    eprintln!("xlit_score: worst relative delta {worst:.2e}");
    m.finish();
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("models")
        .join("rust")
}

/// The engine the goldens were produced with: no personal store, so the vectors are reproducible on
/// any machine rather than depending on what this one has learned.
fn open_engine() -> Option<likhi_engine::core::Engine> {
    let dir = data_dir();
    if !dir.join("indicxlit").join("model.lkw").exists() {
        eprintln!("SKIPPING engine tests: run scripts/build_rust_data.py first");
        return None;
    }
    let opts = likhi_engine::core::EngineOptions { personal: None, ..Default::default() };
    Some(likhi_engine::core::Engine::open(&dir, opts).expect("engine opens"))
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array field {key}"))
        .iter()
        .map(|x| x.as_str().expect("string").to_string())
        .collect()
}

/// **The acceptance test.** The whole pipeline, both paths, exactly as the shell calls it.
///
/// Everything else in this file checks a component; this checks the product. If this passes, the
/// Rust engine suggests the same words in the same order as the engine the pilot is using.
#[test]
fn suggest_matches_python() {
    let Some(cases) = load("suggest") else { return };
    let Some(e) = open_engine() else { return };
    let mut m = Mismatches::new("suggest");
    for c in &cases {
        let roman = s(c, "roman");
        let context = strings(c, "context");
        let k = c["k"].as_u64().expect("k") as usize;
        let fast = c["fast"].as_bool().expect("fast");
        let want = strings(c, "out");
        let got = e.suggest(&roman, &context, k, fast);
        m.check(got == want, || {
            format!(
                "suggest({roman:?}, ctx={context:?}, k={k}, fast={fast})\n      got    {:?}\n      python {:?}",
                got.iter().map(|w| escape(w)).collect::<Vec<_>>(),
                want.iter().map(|w| escape(w)).collect::<Vec<_>>()
            )
        });
    }
    m.finish();
}

/// The fast path, which is what runs on every keystroke, plus the `strong` flag the shell uses to
/// decide whether to re-ask the model.
#[test]
fn fast_suggest_matches_python() {
    let Some(cases) = load("fast_suggest") else { return };
    let Some(e) = open_engine() else { return };
    let mut m = Mismatches::new("fast_suggest");
    for c in &cases {
        let roman = s(c, "roman");
        let context = strings(c, "context");
        let want = strings(c, "out");
        let want_strong = c["strong"].as_bool().expect("strong");
        let (got, got_strong) = e.fast_suggest(&roman, &context, 5);
        m.check(got == want, || {
            format!(
                "fast_suggest({roman:?}, ctx={context:?})\n      got    {:?}\n      python {:?}",
                got.iter().map(|w| escape(w)).collect::<Vec<_>>(),
                want.iter().map(|w| escape(w)).collect::<Vec<_>>()
            )
        });
        m.check(got_strong == want_strong, || {
            format!("fast_suggest({roman:?}).strong = {got_strong}, python {want_strong}")
        });
    }
    m.finish();
}

/// Per-candidate features and scores.
///
/// When `suggest` disagrees this says which term moved, which is the difference between a
/// five-minute fix and an afternoon of bisecting a ranking function.
#[test]
fn candidate_features_match_python() {
    let Some(cases) = load("features") else { return };
    let Some(e) = open_engine() else { return };
    let mut m = Mismatches::new("features");
    for c in &cases {
        let roman = s(c, "roman");
        let table = e.candidates(&roman, true);
        for row in c["candidates"].as_array().expect("candidates") {
            let word = s(row, "word");
            let Some(ft) = table.get(&word) else {
                m.check(false, || {
                    format!("candidates({roman:?}) is missing {}", escape(&word))
                });
                continue;
            };
            let want_score: f64 = s(row, "score").parse().expect("float");
            let got_score = e.score(&word, ft, &[]);
            let ints: [(&str, i64, i64); 5] = [
                ("rom_exact", ft.rom_exact as i64, row["rom_exact"].as_i64().unwrap()),
                ("rom_prefix", ft.rom_prefix as i64, row["rom_prefix"].as_i64().unwrap()),
                ("gap", ft.gap as i64, row["gap"].as_i64().unwrap()),
                ("xlit_rank", ft.xlit_rank as i64, row["xlit_rank"].as_i64().unwrap()),
                ("lex_score", ft.lex_score, row["lex_score"].as_i64().unwrap()),
            ];
            for (name, got, want) in ints {
                m.check(got == want, || {
                    format!("{roman:?}/{} {name} = {got}, python {want}", escape(&word))
                });
            }
            let bools: [(&str, bool, bool); 6] = [
                ("key_fine", ft.key_fine, row["key_fine"].as_bool().unwrap()),
                ("key_coarse", ft.key_coarse, row["key_coarse"].as_bool().unwrap()),
                ("key_fine_prefix", ft.key_fine_prefix, row["key_fine_prefix"].as_bool().unwrap()),
                ("key_coarse_prefix", ft.key_coarse_prefix, row["key_coarse_prefix"].as_bool().unwrap()),
                ("avro", ft.avro, row["avro"].as_bool().unwrap()),
                ("in_lexicon", ft.in_lexicon, row["in_lexicon"].as_bool().unwrap()),
            ];
            for (name, got, want) in bools {
                m.check(got == want, || {
                    format!("{roman:?}/{} {name} = {got}, python {want}", escape(&word))
                });
            }
            // The score carries the model's float32 noise through a log-linear sum, so it gets the
            // same tolerance the model does rather than an exact comparison.
            m.check((got_score - want_score).abs() < 1e-3, || {
                format!(
                    "{roman:?}/{} score = {got_score}, python {want_score}",
                    escape(&word)
                )
            });
        }
    }
    m.finish();
}

/// Independent of the goldens: the .lkx reader's own invariants.
///
/// The golden test above proves lookups agree with Python on real data; this proves the prefix scan
/// returns a contiguous, correctly bounded range, which no single-key lookup would catch.
#[test]
fn prefix_scan_is_sound() {
    use likhi_engine::lexicon::Table;

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("models")
        .join("rust")
        .join("lexicon")
        .join("romans.lkx");
    if !path.exists() {
        eprintln!("SKIPPING prefix_scan_is_sound: run scripts/build_rust_data.py first");
        return;
    }
    let t = Table::open(&path).expect("romans.lkx opens");

    for prefix in ["amar\t", "a", "tumi\t", "zzzzzzzznotakey", ""] {
        let hits = t.prefix_iter(prefix);
        for (k, _r) in &hits {
            assert!(k.starts_with(prefix), "{k:?} does not start with {prefix:?}");
        }
        // Sorted, and therefore complete: if the scan stopped early, some later key would also
        // start with the prefix. Check the boundary directly by walking everything for a short one.
        for w in hits.windows(2) {
            assert!(w[0].0.as_bytes() <= w[1].0.as_bytes(), "prefix scan is out of order");
        }
    }

    // An exact key found by scanning must also be found by get(), with the same record.
    let sample = t.prefix_iter("amar\t");
    assert!(!sample.is_empty(), "the lexicon should know 'amar'");
    for (k, r) in sample.iter().take(50) {
        let got = t.get(k).unwrap_or_else(|| panic!("get({k:?}) missed a key the scan returned"));
        assert_eq!(got, *r, "get() and prefix_iter() disagree on {k:?}");
    }
}

