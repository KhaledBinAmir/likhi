//! The deadline-aware wrapper around the engine, and the JSON-lines protocol.
//! A port of `SuggestService` and `handle_request` from `src/likhi/server.py`.
//!
//! **The latency design, which is the whole point of the server.** Every request is answered within
//! `deadline_ms`. The table and rule channels answer in a few milliseconds; the transliteration
//! model takes tens of milliseconds and runs on a worker thread. If it finishes inside the deadline
//! the full ranking is returned; otherwise the fast ranking is returned with `"partial": true` and
//! the full result is cached for the next request. The shell re-asks with a long deadline when the
//! user commits, so commits always get the full ranking. Typing therefore never waits on the model.
//!
//! The engine is shared by `&self` and takes no lock on the read path -- see the note on
//! `Engine::personal`. Holding a lock around the model call would make every keystroke wait for the
//! model, which is the failure the Python measured at 120 ms round trips instead of 12.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::core::Engine;
use crate::nextlm::Predictor;
use crate::romankey::{key_from_roman, Level};
use crate::telemetry::{Mode, Telemetry};
use crate::textnorm::normalize_roman;

/// What `ping` reports: the product version this build was made as, or "dev".
pub const VERSION: &str = match crate::update::PRODUCT_VERSION {
    Some(v) => v,
    None => "dev",
};
pub const DEFAULT_PORT: u16 = 47123;
pub const DEFAULT_DEADLINE_MS: f64 = 12.0;

/// First-letter prediction stops after this many letters. Measured on held-out chat
/// (`likhi-nextword --letters`): by four letters the ordinary list has the word on screen more
/// often than not, and each further letter leaves prediction less to add.
pub const PREDICT_MAX_LETTERS: usize = 4;
/// How many predicted words are offered per keystroke. Two: a third added nothing measurable and
/// pushes one more ordinary candidate off the list.
pub const PREDICT_OFFERED: usize = 2;
/// How long the next-word request after a commit may wait for the neural model's answer, which the
/// commit's own learn request started a moment before (`Predictor::best_within`).
const NEXT_WAIT: Duration = Duration::from_millis(10);

/// Identifies one suggestion request: the input, the words of context that can change the ranking
/// (as many as the transliteration model reads -- one, the bigram's, when it reads none), and how
/// many candidates were asked for.
type Key = (String, Vec<String>, usize);

#[derive(Default)]
struct JobState {
    result: Option<Vec<String>>,
    finished: bool,
}

/// One model computation, which several requests may wait on.
struct Job {
    state: Mutex<JobState>,
    done: Condvar,
    /// Raised when a newer input has superseded this job and nobody is waiting for it.
    abort: AtomicBool,
}

struct Queued {
    key: Key,
    roman: String,
    context: Vec<String>,
    k: usize,
    job: Arc<Job>,
}

/// A bounded most-recently-used map. The Python uses an OrderedDict with `move_to_end`.
struct Lru {
    map: HashMap<Key, Vec<String>>,
    order: Vec<Key>,
    cap: usize,
}

impl Lru {
    fn new(cap: usize) -> Lru {
        Lru { map: HashMap::new(), order: Vec::new(), cap }
    }

    fn get(&mut self, key: &Key) -> Option<Vec<String>> {
        let hit = self.map.get(key)?.clone();
        if let Some(i) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(i);
            self.order.push(k);
        }
        Some(hit)
    }

    fn put(&mut self, key: Key, value: Vec<String>) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push(key);
        }
        while self.order.len() > self.cap {
            let oldest = self.order.remove(0);
            self.map.remove(&oldest);
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

struct Shared {
    pending: HashMap<Key, Arc<Job>>,
    cache: Lru,
    /// The most recent input. Older jobs may abort.
    latest: Option<Key>,
    /// Keys a request is currently blocked on; these must never be aborted.
    waiting: Vec<Key>,
    /// The single slot the model worker takes its next job from. A newer submission replaces
    /// whatever is queued, which is how the Python cancels not-yet-started futures: earlier
    /// prefixes of the word being typed are stale the moment another key arrives.
    queue: Option<Queued>,
}

pub struct SuggestService {
    engine: Arc<Engine>,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    /// The neural next-word model, for those who downloaded it (`nextlm`). Without it, and while
    /// its answer for a context is still being worked out, next words come from the counted table.
    predictor: Option<Predictor>,
}

impl SuggestService {
    pub fn new(engine: Arc<Engine>, cache_size: usize) -> SuggestService {
        let shared = Arc::new((
            Mutex::new(Shared {
                pending: HashMap::new(),
                cache: Lru::new(cache_size),
                latest: None,
                waiting: Vec::new(),
                queue: None,
            }),
            Condvar::new(),
        ));
        let svc = SuggestService { engine: Arc::clone(&engine), shared: Arc::clone(&shared), predictor: None };
        // One model worker, as in the Python: the model is the scarce resource and running two
        // copies of it would halve neither latency nor memory.
        std::thread::Builder::new()
            .name("likhi-model".into())
            .spawn(move || model_worker(engine, shared))
            .expect("the model worker thread must start");
        svc
    }

    /// Answer next words from the neural model where it can.
    pub fn with_predictor(mut self, predictor: Predictor) -> SuggestService {
        self.predictor = Some(predictor);
        self
    }

    fn key(roman: &str, context: &[String], k: usize) -> Key {
        let start = context.len().saturating_sub(crate::xlit::CONTEXT_WORDS);
        (roman.to_string(), context[start..].to_vec(), k)
    }

    /// Candidates, whether this came from the fast path, and whether the fast path is confident.
    ///
    /// The third value is what lets the caller decide not to ask again. The fast path is *strong*
    /// when some candidate is an attested spelling of exactly what was typed, seen more than once;
    /// measured over Dakshina, the chat set and the feedback words, that beats the full ranking on
    /// those words -- 90.2 against 85.7 top-1 on chat. Replacing a strong fast answer with the
    /// model's is how "rapid" showed র‍্যাপিড and then changed its mind to রাপিড.
    pub fn suggest(
        &self,
        roman: &str,
        context: &[String],
        k: usize,
        deadline_ms: f64,
    ) -> (Vec<String>, bool, bool) {
        let key = Self::key(roman, context, k);
        let job = {
            let (lock, cv) = &*self.shared;
            let mut sh = lock.lock().unwrap_or_else(|e| e.into_inner());
            sh.latest = Some(key.clone());
            if let Some(hit) = sh.cache.get(&key) {
                return (hit, false, false);
            }
            let job = match sh.pending.get(&key) {
                Some(j) => Arc::clone(j),
                None => {
                    let job = Arc::new(Job {
                        state: Mutex::new(JobState::default()),
                        done: Condvar::new(),
                        abort: AtomicBool::new(false),
                    });
                    sh.pending.insert(key.clone(), Arc::clone(&job));
                    // Replace whatever was queued: it is an earlier prefix of the same word and the
                    // typist has moved on. Its job stays in `pending` and simply never completes,
                    // which is what the Python's `future.cancel()` amounts to.
                    if let Some(stale) = sh.queue.replace(Queued {
                        key: key.clone(),
                        roman: roman.to_string(),
                        context: context.to_vec(),
                        k,
                        job: Arc::clone(&job),
                    }) {
                        stale.job.abort.store(true, Ordering::Relaxed);
                        let mut st = stale.job.state.lock().unwrap_or_else(|e| e.into_inner());
                        st.finished = true;
                        stale.job.done.notify_all();
                        drop(st);
                        sh.pending.remove(&stale.key);
                    }
                    cv.notify_one();
                    job
                }
            };
            sh.waiting.push(key.clone());
            job
        };

        let full = wait_for(&job, Duration::from_secs_f64(deadline_ms.max(0.0) / 1000.0));

        {
            let (lock, _) = &*self.shared;
            let mut sh = lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = sh.waiting.iter().position(|w| w == &key) {
                sh.waiting.remove(i);
            }
        }

        match full {
            Some(cands) => (cands, false, false),
            None => {
                let (fast, strong) = self.engine.fast_suggest(roman, context, k);
                (fast, true, strong)
            }
        }
    }

    /// Words likely to follow the last committed word; empty when not confident enough to show.
    /// And whether the neural model answered, rather than the counted table. `before` as for
    /// `next_completions`.
    pub fn next_words(&self, context: &[String], before: Option<&str>, k: usize, min_share: f64) -> (Vec<String>, bool) {
        let model = self.predictor.as_ref().and_then(|p| p.best_within(&model_context(context, before), "", k, NEXT_WAIT));
        if let Some(words) = model {
            let words = match words.first() {
                Some((_, share)) if f64::from(*share) >= min_share => words.into_iter().map(|w| w.0).collect(),
                _ => Vec::new(),
            };
            return (words, true);
        }
        let words = match context.last() {
            Some(prev) => self.engine.next_words(prev, k, min_share),
            None => Vec::new(),
        };
        (words, false)
    }

    /// First-letter prediction: words that follow the last committed word and fit what has been
    /// typed of the next one. Only while the word is short -- see `PREDICT_MAX_LETTERS`. `before`
    /// is the document's text before the word, when the keyboard could read it (`model_context`).
    /// Each word comes with its share of the likely words that fit the letters, which is what lets
    /// the keyboard show only a guess that is usually right as gray text. And whether the neural
    /// model answered, rather than the counted table.
    pub fn next_completions(&self, roman: &str, context: &[String], before: Option<&str>) -> (Vec<(String, f32)>, bool) {
        let letters = roman.chars().count();
        if let (Some(p), 1..=PREDICT_MAX_LETTERS) = (&self.predictor, letters) {
            let typed = key_from_roman(&normalize_roman(roman), Level::Coarse);
            if typed.is_empty() {
                return (Vec::new(), false);
            }
            if let Some(words) = p.best(&model_context(context, before), &typed, PREDICT_OFFERED) {
                return (words, true);
            }
        }
        let words = match context.last() {
            Some(prev) if !roman.is_empty() && roman.chars().count() <= PREDICT_MAX_LETTERS => {
                self.engine.next_completions(prev, roman, PREDICT_OFFERED)
            }
            _ => Vec::new(),
        };
        (words, false)
    }

    /// Learn a committed word, typed after `context` (and `before`, as for `next_completions`);
    /// and start working out what follows it, so the answer is ready by the time the next word is
    /// begun.
    pub fn learn(&self, roman: &str, chosen: &str, context: &[String], before: Option<&str>) {
        if let Some(p) = &self.predictor {
            let mut next = model_context(context, before);
            next.push(chosen.to_string());
            p.prepare(&next);
        }
        self.engine.learn(roman, chosen);
        let (lock, _) = &*self.shared;
        let mut sh = lock.lock().unwrap_or_else(|e| e.into_inner());
        sh.cache.clear();
    }

    /// What we would have shown first, for telemetry. Cache only; never computes.
    pub fn top_candidate(&self, roman: &str, context: &[String], k: usize) -> String {
        let key = Self::key(roman, context, k);
        let (lock, _) = &*self.shared;
        let mut sh = lock.lock().unwrap_or_else(|e| e.into_inner());
        sh.cache.get(&key).and_then(|c| c.first().cloned()).unwrap_or_default()
    }
}

/// The words before the one being typed, as the ranking and the Likhi model read them: from the
/// document's own text when the keyboard sent it, otherwise the words it committed -- the Bangla
/// words only, the last `CONTEXT_WORDS`, in the form they were typed in. Two requests with the same
/// such words get the same answer, which is what the suggestion cache is keyed on.
fn sentence_context(context: &[String], before: Option<&str>) -> Vec<String> {
    let words = crate::textnorm::tokens(&model_context(context, before).join(" "));
    let start = words.len().saturating_sub(crate::xlit::CONTEXT_WORDS);
    words[start..].iter().map(|w| crate::textnorm::to_output(w)).collect()
}

/// What the next-word model reads as the sentence so far: the document's text before the word, from
/// its last line break, when the keyboard could read it -- a message box, a paragraph -- and
/// otherwise the words the keyboard itself committed. The document is the truth: it has the words
/// typed in English, pasted, or written before a click moved the caret, and our history has none
/// of them.
fn model_context(context: &[String], before: Option<&str>) -> Vec<String> {
    match before {
        Some(text) => vec![text.rsplit(['\n', '\r']).next().unwrap_or("").to_string()],
        None => context.to_vec(),
    }
}

fn wait_for(job: &Job, timeout: Duration) -> Option<Vec<String>> {
    let deadline = Instant::now() + timeout;
    let mut st = job.state.lock().unwrap_or_else(|e| e.into_inner());
    while !st.finished {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let (guard, _) = job
            .done
            .wait_timeout(st, left)
            .unwrap_or_else(|e| e.into_inner());
        st = guard;
    }
    st.result.clone()
}

fn model_worker(engine: Arc<Engine>, shared: Arc<(Mutex<Shared>, Condvar)>) {
    loop {
        let queued = {
            let (lock, cv) = &*shared;
            let mut sh = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(q) = sh.queue.take() {
                    break q;
                }
                sh = cv.wait(sh).unwrap_or_else(|e| e.into_inner());
            }
        };

        // The abort flag is raised when a newer input has arrived and nobody is blocked on this
        // key. It is checked inside the engine between the beam search and candidate scoring, the
        // only two steps long enough to be worth abandoning.
        let watch = Arc::clone(&queued.job);
        let shared_for_abort = Arc::clone(&shared);
        let key_for_abort = queued.key.clone();
        let stop_watch = Arc::new(AtomicBool::new(false));
        let stop_watch_thread = Arc::clone(&stop_watch);
        // A small poller rather than signalling from `suggest`: the engine checks a flag, and the
        // condition ("something newer arrived and nobody wants this") depends on state `suggest`
        // updates without knowing which job is running.
        let watcher = std::thread::Builder::new()
            .name("likhi-abort".into())
            .spawn(move || {
                while !stop_watch_thread.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(4));
                    let (lock, _) = &*shared_for_abort;
                    let sh = lock.lock().unwrap_or_else(|e| e.into_inner());
                    let superseded = sh.latest.as_ref() != Some(&key_for_abort);
                    let wanted = sh.waiting.iter().any(|w| w == &key_for_abort);
                    if superseded && !wanted {
                        watch.abort.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
            .ok();

        let result = engine.suggest_abortable(
            &queued.roman,
            &queued.context,
            queued.k,
            false,
            Some(&queued.job.abort),
        );

        stop_watch.store(true, Ordering::Relaxed);
        if let Some(w) = watcher {
            let _ = w.join();
        }

        {
            let (lock, _) = &*shared;
            let mut sh = lock.lock().unwrap_or_else(|e| e.into_inner());
            sh.pending.remove(&queued.key);
            if let Ok(cands) = &result {
                sh.cache.put(queued.key.clone(), cands.clone());
            }
        }
        let mut st = queued.job.state.lock().unwrap_or_else(|e| e.into_inner());
        st.result = result.ok();
        st.finished = true;
        queued.job.done.notify_all();
    }
}

/// One request line in, one reply line out. Shared by both transports.
///
/// Never fails: a malformed request from one keyboard must not take down the connection, let alone
/// the engine every other application is talking to.
pub fn handle_request(svc: &SuggestService, tel: &Telemetry, raw: &[u8]) -> Vec<u8> {
    let reply = match serde_json::from_slice::<Value>(raw) {
        Err(e) => json!({"ok": false, "error": format!("JSONDecodeError: {e}")}),
        Ok(req) => {
            let op = req.get("op").and_then(Value::as_str).unwrap_or("suggest");
            match op {
                "ping" => json!({"ok": true, "version": VERSION}),
                // Next-word suggestions, asked right after a word is committed with Space. Counted
                // when shown and when taken -- numbers only, never the words -- so the pilot says
                // whether the strip earns its place rather than anyone guessing.
                "next" => {
                    let context = string_list(&req, "context");
                    let k = req.get("k").and_then(Value::as_u64).unwrap_or(3).clamp(1, 5) as usize;
                    let min_share = req.get("min_share").and_then(Value::as_f64).unwrap_or(0.2);
                    let (words, from_model) = svc.next_words(&context, req.get("before").and_then(Value::as_str), k, min_share);
                    if !words.is_empty() && tel.mode != Mode::Off {
                        tel.note("next_shown");
                    }
                    json!({"ok": true, "candidates": words, "model": from_model})
                }
                "next_taken" => {
                    if tel.mode != Mode::Off {
                        tel.note("next_taken");
                    }
                    json!({"ok": true})
                }
                // "Check now", from the tray menu or the Likhi window. Runs the same signed check the
                // daily one does and offers what it finds; the answer says what happened so the window
                // can show it. Works with daily checks switched off, because asking is consent.
                #[cfg(windows)]
                "update_check" => json!(crate::update::check_now()),
                // Drawing the candidate list for a text service that cannot draw one itself. Only
                // the drawing: what the candidates are and which is highlighted was decided by the
                // caller, which is the only side that knows what is being typed.
                #[cfg(windows)]
                "ui_show" => {
                    let items = string_list(&req, "items");
                    let cursor = req.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let r = req.get("rect").and_then(Value::as_array).cloned().unwrap_or_default();
                    let at = |i: usize| r.get(i).and_then(Value::as_i64).unwrap_or(0) as i32;
                    let rect = windows::Win32::Foundation::RECT {
                        left: at(0),
                        top: at(1),
                        right: at(2),
                        bottom: at(3),
                    };
                    let index = |key: &str| req.get(key).and_then(Value::as_u64).map(|n| n as usize);
                    let content = likhi_ui::Content {
                        candidates: items,
                        cursor,
                        tab_hint: req.get("tab_hint").and_then(Value::as_bool).unwrap_or(false),
                        predicted_from: index("predicted_from"),
                        typed_at: index("typed_at"),
                        tab_at: index("tab_at"),
                    };
                    let shown = crate::uihost::show(content, rect);
                    json!({"ok": true, "shown": shown})
                }
                #[cfg(windows)]
                "ui_hide" => json!({"ok": true, "shown": crate::uihost::hide()}),
                "suggest" => {
                    let started = Instant::now();
                    let roman = req.get("roman").and_then(Value::as_str).unwrap_or("");
                    let context = string_list(&req, "context");
                    let k = req.get("k").and_then(Value::as_u64).unwrap_or(5) as usize;
                    let deadline = req
                        .get("deadline_ms")
                        .and_then(Value::as_f64)
                        .unwrap_or(DEFAULT_DEADLINE_MS);
                    let words = sentence_context(&context, req.get("before").and_then(Value::as_str));
                    let (cands, partial, strong) = svc.suggest(roman, &words, k, deadline);
                    let (predicted, from_model) = svc.next_completions(roman, &context, req.get("before").and_then(Value::as_str));
                    let (next, next_shares): (Vec<String>, Vec<f32>) = predicted.into_iter().unzip();
                    json!({
                        "ok": true,
                        "candidates": cands,
                        // Kept apart from `candidates` rather than merged in here: the keyboard
                        // decides where they go, and the ranking every golden test pins stays
                        // exactly what it was.
                        "next": next,
                        // Parallel to `next`: two decimals are plenty for a threshold.
                        "next_shares": next_shares.iter().map(|s| (s * 100.0).round() / 100.0).collect::<Vec<f32>>(),
                        // Whether the neural model made those guesses. Only its guesses are sure
                        // enough to show as gray text; the counted table's stay in the list.
                        "next_model": from_model,
                        "partial": partial,
                        "strong": strong,
                        // Two decimals, as Python's round(x, 2) produces.
                        "ms": (started.elapsed().as_secs_f64() * 100_000.0).round() / 100.0,
                    })
                }
                "learn" => {
                    let roman = req.get("roman").and_then(Value::as_str).unwrap_or("");
                    let chosen = req.get("chosen").and_then(Value::as_str).unwrap_or("");
                    let context = string_list(&req, "context");
                    svc.learn(roman, chosen, &context, req.get("before").and_then(Value::as_str));
                    // Whether first-letter prediction earns its place is how often the word taken
                    // was one it put on screen. Counted, never with the word.
                    if tel.mode != Mode::Off && req.get("predicted").and_then(Value::as_bool).unwrap_or(false) {
                        tel.note("predicted_taken");
                    }
                    if tel.mode != Mode::Off {
                        let index = req.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let top1 = match req.get("top1").and_then(Value::as_str) {
                            Some(t) if !t.is_empty() => t.to_string(),
                            _ => svc.top_candidate(roman, &sentence_context(&context, req.get("before").and_then(Value::as_str)), 5),
                        };
                        tel.commit(
                            roman,
                            chosen,
                            index,
                            &top1,
                            req.get("app").and_then(Value::as_str).unwrap_or(""),
                            req.get("retyped").and_then(Value::as_bool).unwrap_or(false),
                            req.get("secure").and_then(Value::as_bool).unwrap_or(false),
                            req.get("latency_ms").and_then(Value::as_f64),
                        );
                    }
                    json!({"ok": true})
                }
                other => json!({"ok": false, "error": format!("unknown op '{other}'")}),
            }
        }
    };
    serde_json::to_vec(&reply).unwrap_or_else(|_| br#"{"ok": false, "error": "reply encoding"}"#.to_vec())
}

fn string_list(req: &Value, field: &str) -> Vec<String> {
    req.get(field)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
