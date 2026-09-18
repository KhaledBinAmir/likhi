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
use crate::telemetry::{Mode, Telemetry};

pub const VERSION: &str = "0.0.1";
pub const DEFAULT_PORT: u16 = 47123;
pub const DEFAULT_DEADLINE_MS: f64 = 12.0;

/// Identifies one suggestion request: the input, the single word of context that can change the
/// ranking, and how many candidates were asked for.
type Key = (String, Option<String>, usize);

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
        let svc = SuggestService { engine: Arc::clone(&engine), shared: Arc::clone(&shared) };
        // One model worker, as in the Python: the model is the scarce resource and running two
        // copies of it would halve neither latency nor memory.
        std::thread::Builder::new()
            .name("likhi-model".into())
            .spawn(move || model_worker(engine, shared))
            .expect("the model worker thread must start");
        svc
    }

    fn key(roman: &str, context: &[String], k: usize) -> Key {
        (roman.to_string(), context.last().cloned(), k)
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

    pub fn learn(&self, roman: &str, chosen: &str) {
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
                    let shown = crate::uihost::show(items, cursor, rect);
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
                    let (cands, partial, strong) = svc.suggest(roman, &context, k, deadline);
                    json!({
                        "ok": true,
                        "candidates": cands,
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
                    svc.learn(roman, chosen);
                    if tel.mode != Mode::Off {
                        let index = req.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let top1 = match req.get("top1").and_then(Value::as_str) {
                            Some(t) if !t.is_empty() => t.to_string(),
                            _ => svc.top_candidate(roman, &context, 5),
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
