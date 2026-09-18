//! Opt-in usage telemetry. A port of `src/likhi/telemetry.py`.
//!
//! Design rule: **never record what the user wrote.** Two streams, both local files the user owns:
//!
//! * `metrics` -- counters only: words committed, how often candidate 1 was taken, which position
//!   was chosen, latency percentiles, per foreground application. Answers "is it working" and
//!   contains no text at all.
//! * `events` -- *struggle events* only: the user did not take the first suggestion. One row holds
//!   the typed roman string, the word they chose, and the word we wrongly ranked first. Words
//!   accepted first time are never recorded, because they teach us nothing.
//!
//! Redaction happens before anything is written: anything with a digit, `@`, `:` or a slash is
//! dropped (identifiers, passwords, URLs, times, money); over-long strings are dropped (pasted or
//! concatenated text); a field the caller marks secure (a password box) records nothing at all; and
//! no timestamp is finer than the hour.
//!
//! The file format is fixed by something already deployed: a collector is parsing these files in
//! production, so the rows this writes must be byte-compatible with the Python's. That is why the
//! JSON here is assembled in a specific key order rather than serialised from a struct.
//!
//! Every failure is swallowed. Telemetry must never affect typing, and a broken counter is not
//! worth a dropped keystroke.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Strings longer than this are dropped: pasted or concatenated text, not a word.
const MAX_LEN: usize = 32;
/// Write a counter row every N committed words. Machines get shut down or killed without a clean
/// exit and TerminateProcess cannot be caught, so counters must not sit in memory for an hour.
/// Rows carry their hour bucket, so several rows per hour simply sum at collection time.
const FLUSH_EVERY: u64 = 20;
/// Cap one upload, so a machine offline for a week catches up in steps rather than one huge POST.
const MAX_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Metrics,
    Full,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "metrics" => Mode::Metrics,
            "full" => Mode::Full,
            _ => Mode::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Metrics => "metrics",
            Mode::Full => "full",
        }
    }
}

/// True when this pair is safe to write: no digits, no symbols that mark an identifier or a URL,
/// nothing over-long.
pub fn is_recordable(roman: &str, word: &str) -> bool {
    if roman.is_empty() || word.is_empty() {
        return false;
    }
    // Python measures `len()` in characters, not bytes, and a Bengali word is three bytes a
    // character -- comparing byte length would reject almost every real word.
    if roman.chars().count() > MAX_LEN || word.chars().count() > MAX_LEN {
        return false;
    }
    let unsafe_char = |c: char| c.is_ascii_digit() || matches!(c, '@' | ':' | '/' | '\\');
    !(roman.chars().any(unsafe_char) || word.chars().any(unsafe_char))
}

fn hour_bucket() -> String {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::GetLocalTime;
        let t = unsafe { GetLocalTime() };
        format!("{:04}-{:02}-{:02}T{:02}", t.wYear, t.wMonth, t.wDay, t.wHour)
    }
    #[cfg(not(windows))]
    {
        "1970-01-01T00".to_string()
    }
}

/// Minimal JSON string escaping, matching `json.dumps(..., ensure_ascii=False)`: only the
/// characters JSON requires are escaped, and non-ASCII is written through as UTF-8.
fn json_escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub struct Config {
    pub mode: Mode,
    pub dir: PathBuf,
    pub drop: Option<PathBuf>,
    pub endpoint: Option<String>,
    pub key: Option<String>,
    pub sync_seconds: f64,
}

struct State {
    /// BTreeMap so counter keys come out in a stable order. The Python uses a Counter, whose order
    /// is insertion order; the collector sums by key and does not care, and a deterministic order
    /// makes the files diffable.
    counters: BTreeMap<String, u64>,
    latencies: Vec<f64>,
    bucket: String,
}

pub struct Telemetry {
    pub mode: Mode,
    pub dir: PathBuf,
    drop: Option<PathBuf>,
    endpoint: Option<String>,
    key: Option<String>,
    install_id: String,
    state: Mutex<State>,
}

impl Telemetry {
    pub fn new(cfg: Config) -> Telemetry {
        let install_id = Self::install_id(&cfg.dir);
        Telemetry {
            mode: cfg.mode,
            drop: cfg.drop,
            endpoint: cfg.endpoint,
            key: cfg.key,
            install_id,
            state: Mutex::new(State {
                counters: BTreeMap::new(),
                latencies: Vec::new(),
                bucket: hour_bucket(),
            }),
            dir: cfg.dir,
        }
    }

    /// A random identifier generated once, so rows from one machine can be grouped without naming
    /// anyone. Sixteen hex characters, matching the Python's `uuid4().hex[:16]`.
    fn install_id(dir: &Path) -> String {
        let path = dir.join("install_id");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        let _ = std::fs::create_dir_all(dir);
        // Not cryptographic, and does not need to be: this only has to be unlikely to collide
        // across a pilot's worth of machines.
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15);
        let mut id = String::with_capacity(16);
        for _ in 0..16 {
            // xorshift64*
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            let v = (seed.wrapping_mul(0x2545F4914F6CDD1D) >> 60) as u8;
            id.push(char::from_digit(v as u32, 16).unwrap_or('0'));
        }
        let _ = std::fs::write(&path, &id);
        id
    }

    fn write_line(&self, name: &str, line: &str) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.dir.join(name)) {
            let _ = writeln!(f, "{line}");
        }
    }

    /// One committed word. `index` is the candidate position taken, 0 being the first suggestion.
    #[allow(clippy::too_many_arguments)]
    pub fn commit(
        &self,
        roman: &str,
        chosen: &str,
        index: usize,
        top1: &str,
        app: &str,
        retyped: bool,
        secure: bool,
        latency_ms: Option<f64>,
    ) {
        if self.mode == Mode::Off || secure {
            return;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if hour_bucket() != st.bucket {
            self.flush_locked(&mut st);
        }
        *st.counters.entry("words".into()).or_insert(0) += 1;
        *st.counters.entry(format!("pos_{}", index.min(9))).or_insert(0) += 1;
        if index == 0 {
            *st.counters.entry("top1_taken".into()).or_insert(0) += 1;
        }
        if retyped {
            *st.counters.entry("retyped".into()).or_insert(0) += 1;
        }
        if !app.is_empty() {
            let clean: String = app
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                .take(24)
                .collect();
            *st.counters.entry(format!("app_{clean}")).or_insert(0) += 1;
        }
        if let Some(ms) = latency_ms {
            if st.latencies.len() < 20_000 {
                st.latencies.push(ms);
            }
        }
        if self.mode == Mode::Full
            && (index > 0 || retyped)
            && is_recordable(roman, chosen)
            && (top1.is_empty() || is_recordable(roman, top1))
        {
            // Key order matches the Python's dict literal, so the two engines produce identical
            // files and the collector cannot tell them apart.
            let mut line = String::with_capacity(96);
            line.push_str("{\"h\": ");
            json_escape(&st.bucket, &mut line);
            line.push_str(", \"roman\": ");
            json_escape(&roman.to_lowercase(), &mut line);
            line.push_str(", \"chose\": ");
            json_escape(chosen, &mut line);
            line.push_str(", \"we_said\": ");
            json_escape(top1, &mut line);
            line.push_str(&format!(", \"pos\": {index}, \"retyped\": "));
            line.push_str(if retyped { "true}" } else { "false}" });
            self.write_line("events.jsonl", &line);
        }
        if st.counters.get("words").copied().unwrap_or(0) >= FLUSH_EVERY {
            // Local write only. Shipping is network I/O and stays on the background timer, never on
            // the path of a committed keystroke.
            self.flush_locked(&mut st);
        }
    }

    pub fn note(&self, counter: &str) {
        if self.mode == Mode::Off {
            return;
        }
        let clean: String = counter
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .take(32)
            .collect();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *st.counters.entry(clean).or_insert(0) += 1;
    }

    fn flush_locked(&self, st: &mut State) {
        if !st.counters.is_empty() || !st.latencies.is_empty() {
            let mut line = String::with_capacity(160);
            line.push_str("{\"h\": ");
            json_escape(&st.bucket, &mut line);
            line.push_str(", \"id\": ");
            json_escape(&self.install_id, &mut line);
            for (k, v) in &st.counters {
                line.push_str(", ");
                json_escape(k, &mut line);
                line.push_str(&format!(": {v}"));
            }
            if !st.latencies.is_empty() {
                let mut s = st.latencies.clone();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let p50 = s[s.len() / 2];
                let p95 = s[std::cmp::min(s.len() - 1, (s.len() as f64 * 0.95) as usize)];
                line.push_str(&format!(
                    ", \"lat_p50\": {}, \"lat_p95\": {}, \"lat_n\": {}",
                    round1(p50),
                    round1(p95),
                    s.len()
                ));
            }
            line.push('}');
            self.write_line("metrics.jsonl", &line);
        }
        st.counters.clear();
        st.latencies.clear();
        st.bucket = hour_bucket();
    }

    pub fn flush(&self) {
        if self.mode == Mode::Off {
            return;
        }
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.flush_locked(&mut st);
        }
        if self.drop.is_some() || self.endpoint.is_some() {
            let _ = self.sync();
        }
    }

    /// Ship new lines to the configured destinations.
    ///
    /// Only the bytes written since the last successful sync are sent, as immutable numbered
    /// chunks. Immutable chunks mean no append races, no locking, and a retry that can never
    /// duplicate or corrupt anything: if a destination is unreachable the offset is not advanced
    /// and the next attempt sends exactly the same bytes under the same sequence number.
    ///
    /// Background thread only. Never call this from a keystroke path.
    pub fn sync(&self) -> Result<usize, String> {
        if self.drop.is_none() && self.endpoint.is_none() {
            return Err("no drop folder or endpoint configured".into());
        }
        let state_path = self.dir.join("sync_state.json");
        let mut state: BTreeMap<String, u64> = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();

        let mut sent = 0usize;
        let mut errors: Vec<String> = Vec::new();
        for stream in ["metrics.jsonl", "events.jsonl"] {
            let src = self.dir.join(stream);
            let Ok(meta) = std::fs::metadata(&src) else { continue };
            let size = meta.len();
            let mut offset = state.get(stream).copied().unwrap_or(0);
            if size < offset {
                offset = 0; // the file was rotated or deleted: start over
                state.insert(stream.to_string(), 0);
            }
            while offset < size {
                let chunk = match read_at(&src, offset, MAX_CHUNK_BYTES) {
                    Ok(c) => c,
                    Err(e) => {
                        errors.push(format!("{stream}: {e}"));
                        break;
                    }
                };
                // Never split a line across chunks. A chunk with no newline is a partial line still
                // being written; wait for the next round.
                let Some(cut) = chunk.iter().rposition(|&b| b == b'\n') else { break };
                let chunk = &chunk[..cut + 1];
                let seq = state.get(&format!("{stream}.seq")).copied().unwrap_or(0) + 1;

                let mut failed = None;
                if let Some(target) = &self.drop {
                    if let Err(e) = self.send_folder(target, stream, seq, chunk) {
                        failed = Some(e);
                    }
                }
                if failed.is_none() {
                    if let Some(url) = &self.endpoint {
                        if let Err(e) = self.send_http(url, stream, seq, chunk) {
                            failed = Some(e);
                        }
                    }
                }
                if let Some(e) = failed {
                    errors.push(format!("{stream}: {e}"));
                    break;
                }
                offset += chunk.len() as u64;
                state.insert(stream.to_string(), offset);
                state.insert(format!("{stream}.seq"), seq);
                sent += chunk.iter().filter(|&&b| b == b'\n').count();
            }
        }
        if let Ok(text) = serde_json::to_string(&state) {
            let _ = std::fs::write(&state_path, text);
        }
        if errors.is_empty() {
            Ok(sent)
        } else {
            Err(errors.join("; "))
        }
    }

    fn send_folder(&self, target: &Path, stream: &str, seq: u64, chunk: &[u8]) -> Result<(), String> {
        let out_dir = target.join(&self.install_id);
        std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        let name = format!("{}-{:05}.jsonl", stream.split('.').next().unwrap_or(stream), seq);
        let tmp = out_dir.join(format!("{name}.part"));
        std::fs::write(&tmp, chunk).map_err(|e| e.to_string())?;
        // Atomic publish: a reader never sees a half-written file.
        std::fs::rename(&tmp, out_dir.join(&name)).map_err(|e| e.to_string())
    }

    fn send_http(&self, url: &str, stream: &str, seq: u64, chunk: &[u8]) -> Result<(), String> {
        let headers = [
            ("Content-Type", "application/x-ndjson".to_string()),
            ("X-Likhi-Install", self.install_id.clone()),
            ("X-Likhi-Stream", stream.split('.').next().unwrap_or(stream).to_string()),
            ("X-Likhi-Seq", seq.to_string()),
            ("X-Likhi-Version", "1".to_string()),
        ];
        let mut all: Vec<(&str, String)> = headers.to_vec();
        if let Some(k) = &self.key {
            all.push(("X-Likhi-Key", k.clone()));
        }
        crate::http::post(url, &all, chunk)
    }
}

/// Python's `round(x, 1)`, which is banker's rounding on ties. Reproduced so the files match.
fn round1(x: f64) -> f64 {
    let scaled = x * 10.0;
    let r = scaled.round();
    // `f64::round` rounds half away from zero; Python rounds half to even.
    let v = if (scaled - scaled.trunc()).abs() == 0.5 {
        let down = scaled.trunc();
        if (down as i64) % 2 == 0 {
            down
        } else {
            down + scaled.signum()
        }
    } else {
        r
    };
    v / 10.0
}

fn read_at(path: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    let mut filled = 0;
    while filled < len {
        match f.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_rejects_what_the_python_rejects() {
        assert!(is_recordable("amar", "আমার"));
        assert!(!is_recordable("", "আমার"));
        assert!(!is_recordable("amar", ""));
        assert!(!is_recordable("pass123", "আমার"), "digits");
        assert!(!is_recordable("me@example", "আমার"), "at sign");
        assert!(!is_recordable("http://x", "আমার"), "url");
        assert!(!is_recordable("a:b", "আমার"), "colon");
        assert!(!is_recordable("a\\b", "আমার"), "backslash");
        assert!(!is_recordable(&"x".repeat(33), "আমার"), "over-long");
    }

    #[test]
    fn length_is_measured_in_characters_not_bytes() {
        // 20 Bengali characters is 60 bytes. Measuring bytes would reject almost every real word.
        let word: String = "আ".repeat(20);
        assert!(is_recordable("amar", &word));
    }

    #[test]
    fn rounding_matches_python_round() {
        assert_eq!(round1(3.14), 3.1);
        assert_eq!(round1(3.15), 3.1, "half to even, as Python does");
        assert_eq!(round1(3.25), 3.2, "half to even");
        assert_eq!(round1(2.0), 2.0);
    }

    #[test]
    fn json_escaping_leaves_bengali_alone() {
        let mut out = String::new();
        json_escape("আমার", &mut out);
        assert_eq!(out, "\"আমার\"", "ensure_ascii=False means no \\u escapes");
        out.clear();
        json_escape("a\"b\\c", &mut out);
        assert_eq!(out, "\"a\\\"b\\\\c\"");
    }
}
