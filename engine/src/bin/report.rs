//! `likhi-report`: see what telemetry holds, ship it, and aggregate a pilot.
//! A port of `src/likhi/report.py`.
//!
//! Client side, on a tester's machine:
//!
//!     likhi-report show                    what is in my local files, nothing leaves the machine
//!     likhi-report sync                    ship new lines now, to wherever this machine points
//!     likhi-report purge                   delete every local telemetry file
//!
//! Collector side, pulling from the ingest endpoint. Needs the admin key, which is never shipped
//! to clients and is the only way to read collected data back:
//!
//!     likhi-report pull --out pilot
//!     likhi-report collect --drop pilot [--min-installs 2] [--out feedback.jsonl]
//!
//! `pull` and `sync` read `pilot.local.json` at the repository root when arguments are omitted, so
//! the keys stay out of version control and out of shell history.
//!
//! `collect` prints per-install health -- words typed, how often the first suggestion was taken,
//! latency -- and the struggle words ranked by how many *different* installs hit them. Words seen
//! on a single install are held back by default, because that is what a person's own name or a
//! private term looks like; raise `--min-installs` for a stricter bar, or set 1 to review
//! everything. Reviewed rows are appended by hand to `data/feedback/words.jsonl`, which is what
//! closes the loop: real struggles become the set that tuning and evaluation answer to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use likhi_engine::http;
use likhi_engine::telemetry::{local_app_data, Config, Telemetry};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_dir() -> PathBuf {
    local_app_data().join("Likhi")
}

/// Deployment secrets, kept outside version control. The ingest key ships inside clients; the admin
/// key never does.
fn secrets() -> serde_json::Value {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    for path in [repo().join("pilot.local.json"), home.join(".likhi-pilot.json")] {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            return v;
        }
    }
    serde_json::Value::Null
}

fn secret(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Read a JSON Lines file, skipping blank and unparseable lines.
///
/// A malformed line is skipped rather than fatal: these files are appended to by a process that can
/// be killed mid-write, so a torn last line is expected and must not cost the operator the rest of
/// the data.
fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn num(row: &serde_json::Value, key: &str) -> f64 {
    row.get(key).and_then(serde_json::Value::as_f64).unwrap_or(0.0)
}

/// `hits` as a percentage of `total`, guarding an empty denominator.
///
/// The `+ 0.0` is not redundant. Rust's `Sum` for `f64` folds from `-0.0`, because that is the
/// additive identity that preserves a genuine negative zero -- so summing an empty iterator gives
/// `-0.0`, and an install that sent struggle events but no metrics row printed `-0.0` in the table
/// where the Python printed `0.0`. Adding `0.0` collapses the sign without touching any other value.
fn pct(hits: f64, total: f64) -> f64 {
    100.0 * hits / total.max(1.0) + 0.0
}

/// serde_json writes `{"a":1}`; Python's `json.dumps` defaults to `{"a": 1}`, and that is the shape
/// of every row already in `data/feedback/words.jsonl`. Rows written here are reviewed and appended
/// to that file by hand, so they have to arrive looking like its neighbours or every append shows up
/// as a reformatting of the whole file.
struct PythonJson;

impl serde_json::ser::Formatter for PythonJson {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }
}

/// Serialise the way `json.dumps(..., ensure_ascii=False)` does. Non-ASCII passes through as UTF-8,
/// which is serde_json's default too.
fn to_python_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PythonJson);
    value.serialize(&mut ser).map_err(|e| e.to_string())?;
    String::from_utf8(buf).map_err(|e| e.to_string())
}

fn text<'a>(row: &'a serde_json::Value, key: &str) -> &'a str {
    row.get(key).and_then(serde_json::Value::as_str).unwrap_or("")
}

/// Pad to `width` counting characters, not bytes, so a Bengali word lines up in the table. `{:<16}`
/// pads by bytes, and a Bengali character is three of them.
fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    let mut out = s.to_string();
    if n < width {
        out.push_str(&" ".repeat(width - n));
    }
    out
}

// ------------------------------------------------------------------------------------------ show

fn cmd_show(dir: Option<&str>, limit: usize) -> i32 {
    let d = dir.map(PathBuf::from).unwrap_or_else(default_dir);
    let metrics = read_jsonl(&d.join("metrics.jsonl"));
    let events = read_jsonl(&d.join("events.jsonl"));
    println!("telemetry files in {}", d.display());
    println!("  metrics rows : {}", metrics.len());
    println!("  struggle rows: {}", events.len());
    if !metrics.is_empty() {
        let words: f64 = metrics.iter().map(|r| num(r, "words")).sum();
        let top1: f64 = metrics.iter().map(|r| num(r, "top1_taken")).sum();
        println!(
            "  words committed: {}   first suggestion taken: {:.1}%",
            words as i64,
            pct(top1, words)
        );
    }
    println!("\nEvery line that would be shared (this is the whole content):");
    for r in events.iter().take(limit) {
        println!(
            "  typed {} chose {} we ranked first {}",
            pad(text(r, "roman"), 16),
            pad(text(r, "chose"), 14),
            text(r, "we_said")
        );
    }
    if events.len() > limit {
        println!("  ... {} more", events.len() - limit);
    }
    0
}

// ------------------------------------------------------------------------------------------ sync

fn cmd_sync(dir: Option<&str>, drop: Option<&str>, endpoint: Option<&str>, key: Option<&str>) -> i32 {
    // The machine's own configuration is what a plain `sync` uses, so a manual sync and the
    // engine's automatic one can never reach different destinations.
    let mut cfg = Config::discover();
    if cfg.drop.is_none() && cfg.endpoint.is_none() {
        let sec = secrets();
        cfg.endpoint = secret(&sec, "endpoint");
        cfg.key = secret(&sec, "ingest_key");
    }
    if let Some(d) = dir {
        cfg.dir = PathBuf::from(d);
    }
    let overriding = drop.is_some() || endpoint.is_some();
    if cfg.drop.is_none() && cfg.endpoint.is_none() && !overriding {
        println!("no destination configured: nothing to sync to");
        return 1;
    }

    let t = Telemetry::new(Config { mode: cfg.mode, ..cfg });
    // Anything passed on the command line goes through `sync_to`, which refuses to record progress
    // when the destination is not the configured one. Constructing a Telemetry around the override
    // instead would look identical and quietly consume this install's telemetry: the offsets would
    // advance and those lines would never reach the collector the pilot reads.
    match t.sync_to(drop.map(Path::new), endpoint, key) {
        Ok(r) => {
            println!("{{\"sent\": {}, \"ad_hoc\": {}}}", r.sent, r.ad_hoc);
            if r.ad_hoc {
                println!("(ad hoc: progress was not recorded, so the configured destination still gets these lines)");
            }
            0
        }
        Err(e) => {
            println!("{{\"error\": {}}}", serde_json::Value::String(e));
            1
        }
    }
}

// ----------------------------------------------------------------------------------------- purge

fn cmd_purge(dir: Option<&str>) -> i32 {
    let d = dir.map(PathBuf::from).unwrap_or_else(default_dir);
    let mut n = 0;
    for name in ["metrics.jsonl", "events.jsonl", "sync_state.json"] {
        let p = d.join(name);
        if p.exists() && std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    println!(
        "deleted {n} telemetry files from {} (install_id and learning data untouched)",
        d.display()
    );
    0
}

// ------------------------------------------------------------------------------------------ pull

/// True when `name` is safe to use as one path component.
///
/// The install id and chunk name come from the server and are used to build a path. The Python this
/// replaces trusted both, so an endpoint answering with `"_chunk": "../../x"` would have written
/// outside the output directory. Rejecting anything that is not a plain name closes that.
fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(|c| c.is_control())
}

fn cmd_pull(endpoint: Option<&str>, admin_key: Option<&str>, since: Option<&str>, out: &str) -> i32 {
    let sec = secrets();
    let endpoint = endpoint
        .map(str::to_string)
        .or_else(|| secret(&sec, "endpoint"))
        .unwrap_or_default();
    let admin_key = admin_key
        .map(str::to_string)
        .or_else(|| secret(&sec, "admin_key"))
        .unwrap_or_default();
    if endpoint.is_empty() || admin_key.is_empty() {
        println!("need an endpoint and an admin key (arguments, or pilot.local.json)");
        return 1;
    }
    let base = endpoint.rsplit_once("/v1/").map(|(b, _)| b).unwrap_or(&endpoint);
    let mut url = format!("{base}/v1/export");
    if let Some(s) = since {
        url.push_str(&format!("?since={s}"));
    }

    let body = match http::get(
        &url,
        &[("X-Likhi-Admin-Key", admin_key)],
        http::EXPORT_TIMEOUT_MS,
    ) {
        Ok(b) => b,
        Err(e) => {
            println!("pull failed: {e}");
            return 1;
        }
    };
    let body = String::from_utf8_lossy(&body);

    let out_dir = PathBuf::from(out);
    let mut chunks: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut truncated = false;
    let mut rejected = 0usize;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(mut row) = serde_json::from_str::<serde_json::Value>(line) else {
            rejected += 1;
            continue;
        };
        let Some(obj) = row.as_object_mut() else {
            rejected += 1;
            continue;
        };
        if obj.contains_key("_truncated") {
            truncated = true;
            continue;
        }
        let install = obj
            .remove("_install")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into());
        let chunk = obj
            .remove("_chunk")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "chunk.jsonl".into());
        if !safe_component(&install) || !safe_component(&chunk) {
            rejected += 1;
            continue;
        }
        // Re-serialised, so keys come out in sorted order rather than the order the server sent.
        // Nothing reads these positionally -- `collect` looks fields up by name -- and a fixed order
        // makes two pulls diffable.
        match to_python_json(&row) {
            Ok(s) => chunks.entry((install, chunk)).or_default().push(s),
            Err(_) => rejected += 1,
        }
    }

    let mut written = 0usize;
    for ((install, name), lines) in &chunks {
        let d = out_dir.join(install);
        if let Err(e) = std::fs::create_dir_all(&d) {
            println!("could not create {}: {e}", d.display());
            return 1;
        }
        let mut text = lines.join("\n");
        text.push('\n');
        if let Err(e) = std::fs::write(d.join(name), text) {
            println!("could not write {}: {e}", d.join(name).display());
            return 1;
        }
        written += lines.len();
    }
    let installs: BTreeSet<&String> = chunks.keys().map(|(i, _)| i).collect();
    println!(
        "pulled {written} rows from {} installs into {}",
        installs.len(),
        out_dir.display()
    );
    if rejected > 0 {
        println!("skipped {rejected} rows that were malformed or had an unsafe name");
    }
    if truncated {
        println!("NOTE: the export hit its size cap; pull again with --since to continue");
    }
    println!("next: likhi-report collect --drop {}", out_dir.display());
    0
}

// --------------------------------------------------------------------------------------- collect

fn cmd_collect(drop: &str, min_installs: usize, limit: usize, out: Option<&str>) -> i32 {
    let drop = PathBuf::from(drop);
    if !drop.exists() {
        println!("drop folder not found: {}", drop.display());
        return 1;
    }
    let mut installs: Vec<PathBuf> = match std::fs::read_dir(&drop) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(e) => {
            println!("could not read {}: {e}", drop.display());
            return 1;
        }
    };
    installs.sort();
    println!("{} installs reporting in {}\n", installs.len(), drop.display());
    println!(
        "{:18} {:>8} {:>7} {:>8} {:>7} {:>7} {:>8} {:>9}",
        "install", "words", "top1%", "retyped", "p50ms", "p95ms", "misses", "friction"
    );

    let (mut total_words, mut total_top1) = (0.0f64, 0.0f64);
    // A ranking miss: (roman, chose) -> installs that saw it, and what we wrongly ranked first.
    let mut miss_installs: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut word_wrong: BTreeMap<(String, String), BTreeMap<String, usize>> = BTreeMap::new();
    // Typing friction: our first suggestion was taken, but the word was retyped on the way.
    let mut friction_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    // Which position held the word they wanted, when ours was wrong. 1 means the second entry.
    let mut chosen_at: BTreeMap<usize, usize> = BTreeMap::new();
    let (mut misses, mut friction) = (0usize, 0usize);

    for inst in &installs {
        let name = inst.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let mut metrics = Vec::new();
        let mut events = Vec::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(inst)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for f in &files {
            let Some(fname) = f.file_name().and_then(|n| n.to_str()) else { continue };
            if fname.starts_with("metrics-") && fname.ends_with(".jsonl") {
                metrics.extend(read_jsonl(f));
            } else if fname.starts_with("events-") && fname.ends_with(".jsonl") {
                events.extend(read_jsonl(f));
            }
        }
        let words: f64 = metrics.iter().map(|r| num(r, "words")).sum();
        let top1: f64 = metrics.iter().map(|r| num(r, "top1_taken")).sum();
        let retyped: f64 = metrics.iter().map(|r| num(r, "retyped")).sum();
        let mean = |key: &str| -> f64 {
            let v: Vec<f64> = metrics
                .iter()
                .filter(|r| r.get(key).is_some())
                .map(|r| num(r, key))
                .collect();
            if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
        };
        total_words += words;
        total_top1 += top1;
        let (before_misses, before_friction) = (misses, friction);
        for e in &events {
            let (roman, chose) = (text(e, "roman").to_string(), text(e, "chose").to_string());
            if roman.is_empty() || chose.is_empty() {
                continue;
            }
            // Two different things are recorded here and they must not be added together.
            //
            // `pos > 0` is a ranking miss: the word they wanted was not the one we put first.
            // `pos == 0` with `retyped` is typing friction: our first suggestion *was* the word
            // they took, they just used backspace getting there. On the first pilot data, 127 of
            // 174 events were the second kind, and reporting them together made a list of words
            // Likhi gets right look like a list of failures -- and would have put them into the
            // feedback set, where they teach nothing.
            let pos = e.get("pos").and_then(serde_json::Value::as_u64).unwrap_or(0);
            let key = (roman, chose);
            if pos > 0 {
                miss_installs.entry(key.clone()).or_default().insert(name.clone());
                let we_said = text(e, "we_said");
                if !we_said.is_empty() {
                    *word_wrong.entry(key).or_default().entry(we_said.to_string()).or_insert(0) += 1;
                }
                *chosen_at.entry(pos as usize).or_insert(0) += 1;
                misses += 1;
            } else {
                *friction_counts.entry(key).or_insert(0) += 1;
                friction += 1;
            }
        }
        println!(
            "{} {:>8} {:>7.1} {:>8} {:>7.1} {:>7.1} {:>8} {:>9}",
            pad(&name, 18),
            words as i64,
            pct(top1, words),
            retyped as i64,
            mean("lat_p50"),
            mean("lat_p95"),
            misses - before_misses,
            friction - before_friction
        );
    }
    println!(
        "\ntotal words {}, first suggestion taken {:.1}%",
        total_words as i64,
        pct(total_top1, total_words)
    );

    // What the two kinds of event add up to, so the next section is not read as the whole story.
    println!(
        "\n{misses} ranking misses (our first suggestion was not the word taken)"
    );
    if !chosen_at.is_empty() {
        let near: usize = chosen_at.iter().filter(|(p, _)| **p == 1).map(|(_, n)| *n).sum();
        println!(
            "  the word they wanted was second {}/{} times ({:.0}%), so it was on screen and one key away",
            near,
            misses,
            100.0 * near as f64 / misses.max(1) as f64
        );
        let positions: Vec<String> = chosen_at
            .iter()
            .map(|(p, n)| format!("#{}: {n}", p + 1))
            .collect();
        println!("  position taken: {}", positions.join("   "));
    }
    println!(
        "{friction} typing friction (our first suggestion WAS taken; the word was retyped on the way)"
    );

    let shared: Vec<(&(String, String), &BTreeSet<String>)> = miss_installs
        .iter()
        .filter(|(_, v)| v.len() >= min_installs)
        .collect();
    println!(
        "\nranking misses seen on >= {min_installs} installs: {} of {}",
        shared.len(),
        miss_installs.len()
    );
    println!(
        "{:<18} {:<16} {:<16} {:>8}",
        "typed", "they chose", "we ranked first", "installs"
    );
    // Most widely shared first. The full key breaks ties so the order is fully determined: an
    // operator comparing two collections should not see rows move for no reason.
    let mut ranked = shared;
    ranked.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| a.0.cmp(b.0))
    });

    for ((roman, chose), insts) in ranked.iter().take(limit) {
        let wrong = word_wrong
            .get(&(roman.clone(), chose.clone()))
            .and_then(|m| m.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0))))
            .map(|(w, _)| w.as_str())
            .unwrap_or("");
        println!(
            "{} {} {} {:>8}",
            pad(roman, 18),
            pad(chose, 16),
            pad(wrong, 16),
            insts.len()
        );
    }

    // Friction is a different question -- where typing is awkward rather than where ranking is
    // wrong -- so it gets its own short list and never reaches the feedback set.
    if friction > 0 {
        println!("\nmost retyped words (we ranked these first and they were taken)");
        let mut top: Vec<(&(String, String), &usize)> = friction_counts.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        println!("{:<18} {:<16} {:>8}", "typed", "they chose", "times");
        for ((roman, chose), n) in top.iter().take(12) {
            println!("{} {} {:>8}", pad(roman, 18), pad(chose, 16), n);
        }
    }

    if let Some(out) = out {
        // Only the ranking misses. A word we already put first teaches the ranker nothing, and
        // adding it to the feedback set would dilute the one measurement that decides whether a
        // change helped -- on the first pilot data that would have been 127 rows of 174.
        //
        // A struct, not a map: serde writes fields in declaration order, which keeps these rows
        // shaped like the ones already in data/feedback/words.jsonl.
        #[derive(serde::Serialize)]
        struct Row<'a> {
            roman: &'a str,
            gold: [&'a str; 1],
            tags: [&'a str; 1],
            installs: usize,
        }
        let mut text = String::new();
        for ((roman, chose), insts) in &ranked {
            match to_python_json(&Row {
                roman,
                gold: [chose],
                tags: ["pilot"],
                installs: insts.len(),
            }) {
                Ok(s) => {
                    text.push_str(&s);
                    text.push('\n');
                }
                Err(e) => {
                    println!("could not serialise a row: {e}");
                    return 1;
                }
            }
        }
        if let Err(e) = std::fs::write(out, text) {
            println!("could not write {out}: {e}");
            return 1;
        }
        println!(
            "\nwrote {} rows to {out} (review, then append to data/feedback/words.jsonl)",
            ranked.len()
        );
    }
    0
}

// ------------------------------------------------------------------------------------------ main

const USAGE: &str = "usage:
  likhi-report show    [--dir D] [--limit 40]
  likhi-report sync    [--dir D] [--drop D] [--endpoint URL] [--key K]
  likhi-report purge   [--dir D]
  likhi-report pull    [--endpoint URL] [--admin-key K] [--since 2026-09-16T00] [--out pilot]
  likhi-report collect --drop DIR [--min-installs 2] [--limit 60] [--out feedback.jsonl]";

/// `--name value` pairs. Unknown flags are an error rather than ignored: a mistyped `--admin-key`
/// would otherwise look like a missing key and send the operator hunting through pilot.local.json.
fn flags(argv: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i < argv.len() {
        let name = argv[i].strip_prefix("--").ok_or(format!("unexpected argument {}", argv[i]))?;
        if !allowed.contains(&name) {
            return Err(format!("unknown argument --{name}"));
        }
        let value = argv.get(i + 1).ok_or(format!("--{name} needs a value"))?;
        out.insert(name.to_string(), value.clone());
        i += 2;
    }
    Ok(out)
}

fn number(f: &BTreeMap<String, String>, key: &str, default: usize) -> Result<usize, String> {
    match f.get(key) {
        Some(v) => v.parse().map_err(|_| format!("--{key} needs a number, got {v}")),
        None => Ok(default),
    }
}

fn run() -> Result<i32, String> {
    let argv: Vec<String> = std::env::args().collect();
    let Some(cmd) = argv.get(1) else {
        return Err(USAGE.to_string());
    };
    let rest = &argv[2..];
    let get = |f: &BTreeMap<String, String>, k: &str| f.get(k).cloned();
    match cmd.as_str() {
        "show" => {
            let f = flags(rest, &["dir", "limit"])?;
            Ok(cmd_show(get(&f, "dir").as_deref(), number(&f, "limit", 40)?))
        }
        "sync" => {
            let f = flags(rest, &["dir", "drop", "endpoint", "key"])?;
            Ok(cmd_sync(
                get(&f, "dir").as_deref(),
                get(&f, "drop").as_deref(),
                get(&f, "endpoint").as_deref(),
                get(&f, "key").as_deref(),
            ))
        }
        "purge" => {
            let f = flags(rest, &["dir"])?;
            Ok(cmd_purge(get(&f, "dir").as_deref()))
        }
        "pull" => {
            let f = flags(rest, &["endpoint", "admin-key", "since", "out"])?;
            Ok(cmd_pull(
                get(&f, "endpoint").as_deref(),
                get(&f, "admin-key").as_deref(),
                get(&f, "since").as_deref(),
                f.get("out").map(String::as_str).unwrap_or("pilot"),
            ))
        }
        "collect" => {
            let f = flags(rest, &["drop", "min-installs", "limit", "out"])?;
            let drop = f.get("drop").ok_or("collect needs --drop")?.clone();
            Ok(cmd_collect(
                &drop,
                number(&f, "min-installs", 2)?,
                number(&f, "limit", 60)?,
                get(&f, "out").as_deref(),
            ))
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this guards found its way into a real table: one pilot install had sent struggle
    /// events but no metrics row, and its top-1 column read `-0.0`.
    #[test]
    fn an_empty_sum_does_not_print_as_negative_zero() {
        let empty: Vec<f64> = Vec::new();
        let sum: f64 = empty.iter().sum();
        assert!(sum.is_sign_negative(), "Rust's empty f64 sum is -0.0; if this ever changes, pct's guard can go");
        assert_eq!(format!("{:.1}", pct(sum, sum)), "0.0");
        assert_eq!(format!("{:.1}", pct(0.0, 0.0)), "0.0");
        // Ordinary values are untouched.
        assert!((pct(1.0, 4.0) - 25.0).abs() < 1e-12);
        assert!((pct(3.0, 3.0) - 100.0).abs() < 1e-12);
    }

    /// Matches `json.dumps(..., ensure_ascii=False)`, which is how every row already in
    /// data/feedback/words.jsonl is written.
    #[test]
    fn json_is_spaced_like_python() {
        #[derive(serde::Serialize)]
        struct Row<'a> {
            roman: &'a str,
            gold: [&'a str; 2],
            installs: usize,
        }
        let s = to_python_json(&Row { roman: "apnar", gold: ["আপনার", "আপনি"], installs: 2 }).unwrap();
        assert_eq!(
            s,
            "{\"roman\": \"apnar\", \"gold\": [\"আপনার\", \"আপনি\"], \"installs\": 2}"
        );
    }

    /// `_install` and `_chunk` arrive from the network and become a path.
    #[test]
    fn unsafe_path_components_are_rejected() {
        assert!(safe_component("metrics-00001.jsonl"));
        assert!(safe_component("29c842f911ed4756"));
        for bad in ["", "..", ".", "../x", "..\\x", "a/b", "a\\b", "C:evil", "a\0b", "a\nb"] {
            assert!(!safe_component(bad), "{bad:?} should be rejected");
        }
        assert!(!safe_component(&"x".repeat(129)));
    }

    /// Python pads by characters; `{:<16}` pads by bytes, and a Bengali character is three of them.
    #[test]
    fn padding_counts_characters() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("আপনার", 7), "আপনার  ");
        assert_eq!(pad("toolong", 3), "toolong");
    }

    #[test]
    fn unknown_flags_are_an_error() {
        let argv: Vec<String> = ["--admin-ky", "x"].iter().map(|s| s.to_string()).collect();
        assert!(flags(&argv, &["admin-key"]).is_err());
        let argv: Vec<String> = ["--admin-key"].iter().map(|s| s.to_string()).collect();
        assert!(flags(&argv, &["admin-key"]).is_err(), "a flag with no value is an error");
        let argv: Vec<String> = ["--admin-key", "x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(flags(&argv, &["admin-key"]).unwrap().get("admin-key").unwrap(), "x");
    }
}
