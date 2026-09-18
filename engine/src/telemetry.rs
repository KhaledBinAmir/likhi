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

/// `%LOCALAPPDATA%`, where telemetry files and the per-user config live.
pub fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The per-user override file, the one the Likhi window writes.
pub fn user_config_path() -> PathBuf {
    local_app_data().join("Likhi").join("config.json")
}

/// Every place the shell's config.json may live, most specific first.
///
/// The installed layout puts the engine under `<app>\engine\` and the config beside the text
/// service, so a path relative to the executable is what an installed engine actually needs; the
/// launcher also sets LIKHI_CONFIG. Missing that was a silent failure in the Python once:
/// telemetry simply stayed off on every fresh install.
fn config_paths() -> Vec<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    config_paths_from(
        std::env::var_os("LIKHI_CONFIG").map(PathBuf::from),
        exe_dir.as_deref(),
        user_config_path(),
    )
}

/// The candidate list, with its inputs passed in so it can be tested against a layout other than
/// the one the test binary happens to sit in.
fn config_paths_from(
    explicit: Option<PathBuf>,
    exe_dir: Option<&Path>,
    user_path: PathBuf,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = explicit {
        out.push(explicit);
    }
    if let Some(dir) = exe_dir {
        for up in [0usize, 1] {
            let mut base = dir.to_path_buf();
            for _ in 0..up {
                base.pop();
            }
            out.push(base.join("likhi").join("config.json"));
            out.push(base.join("config.json"));
        }
    }
    out.push(user_path);
    out
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    // Strip a UTF-8 byte-order mark: administrators edit this file and Notepad writes one, which
    // plain UTF-8 parsing rejects. That silently disabled telemetry once already.
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    serde_json::from_str(text).ok()
}

impl Config {
    /// Where this machine is configured to report, from the environment or the config files.
    ///
    /// Lives in the library rather than in the server binary because `likhi-report sync` has to
    /// reach exactly the same destination the engine would. Two copies of this logic would mean a
    /// tester's manual sync could quietly go somewhere else than their automatic one.
    pub fn discover() -> Config {
        Config::discover_with(
            &config_paths(),
            &user_config_path(),
            local_app_data().join("Likhi"),
            &|k| std::env::var(k).ok().filter(|v| !v.is_empty()),
        )
    }

    /// The logic of `discover`, with its three inputs passed in.
    ///
    /// Split out so it can be tested. The alternative -- setting `LIKHI_CONFIG` and `LOCALAPPDATA`
    /// around each test -- mutates process-global state that Rust's parallel test threads share, so
    /// the tests would interfere with each other and pass or fail by timing.
    fn discover_with(
        paths: &[PathBuf],
        user_path: &Path,
        dir: PathBuf,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Config {
        if let Some(mode) = env("LIKHI_TELEMETRY") {
            return Config {
                mode: Mode::parse(&mode),
                dir,
                drop: env("LIKHI_TELEMETRY_DROP").map(PathBuf::from),
                endpoint: env("LIKHI_TELEMETRY_ENDPOINT"),
                key: env("LIKHI_TELEMETRY_KEY"),
                sync_seconds: env("LIKHI_TELEMETRY_SYNC_S")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(900.0),
            };
        }

        for path in paths {
            let Some(mut cfg) = read_json(path) else { continue };
            // A person's own choice overrides the machine default, and only the keys they set. The
            // installed config carries the endpoint and the shared key, which a per-user file has no
            // business restating: turning reporting off in the Likhi window must not also erase where
            // reports would go if it were turned back on. Skipped when this *is* the per-user file.
            if path.as_path() != user_path {
                if let Some(user) = read_json(user_path) {
                    if let (Some(base), Some(over)) = (cfg.as_object_mut(), user.as_object()) {
                        for (k, v) in over {
                            base.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            let s = |k: &str| {
                cfg.get(k)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|v| !v.is_empty())
            };
            return Config {
                mode: Mode::parse(&s("telemetry").unwrap_or_default()),
                // Cloned rather than moved: the compiler cannot see that this loop body runs at
                // most once.
                dir: dir.clone(),
                drop: s("telemetry_drop").map(PathBuf::from),
                endpoint: s("telemetry_endpoint"),
                key: s("telemetry_key"),
                sync_seconds: cfg
                    .get("telemetry_sync_seconds")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(900.0),
            };
        }
        Config {
            mode: Mode::Off,
            dir,
            drop: None,
            endpoint: None,
            key: None,
            sync_seconds: 900.0,
        }
    }
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
    /// The anonymous per-install identifier, which is also the directory the collector files this
    /// machine's chunks under.
    pub fn install_id(&self) -> &str {
        &self.install_id
    }

    pub fn new(cfg: Config) -> Telemetry {
        let install_id = Self::read_or_create_install_id(&cfg.dir);
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
    fn read_or_create_install_id(dir: &Path) -> String {
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
        self.sync_to(None, None, None).map(|r| r.sent)
    }

    /// Ship to a destination given here instead of the configured one.
    ///
    /// A destination that differs from the configured one is *ad hoc*, and an ad-hoc sync does not
    /// record progress. `sync_state.json` holds one offset per stream rather than one per
    /// destination, because the configured destinations are always shipped together -- so advancing
    /// the offset for a one-off copy would mean those bytes never reach the destination the pilot
    /// actually reads.
    ///
    /// Learned the hard way in the Python this replaces: a run that shipped to a temporary folder
    /// consumed part of a live install's telemetry, and that machine's words never arrived.
    pub fn sync_to(
        &self,
        drop: Option<&Path>,
        endpoint: Option<&str>,
        key: Option<&str>,
    ) -> Result<SyncResult, String> {
        // Ad hoc only when it actually points somewhere else. Passing the configured destination
        // explicitly, which is what a plain `likhi-report sync` does after reading the config, must
        // behave exactly like passing nothing.
        let ad_hoc = (drop.is_some() && drop != self.drop.as_deref())
            || (endpoint.is_some() && endpoint != self.endpoint.as_deref());
        let drop_target = drop.map(Path::to_path_buf).or_else(|| self.drop.clone());
        let endpoint_target = endpoint.map(str::to_string).or_else(|| self.endpoint.clone());
        let key = key.map(str::to_string).or_else(|| self.key.clone());

        if drop_target.is_none() && endpoint_target.is_none() {
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
                if let Some(target) = &drop_target {
                    if let Err(e) = self.send_folder(target, stream, seq, chunk) {
                        failed = Some(e);
                    }
                }
                if failed.is_none() {
                    if let Some(url) = &endpoint_target {
                        if let Err(e) = self.send_http(url, key.as_deref(), stream, seq, chunk) {
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
        // The whole point of `ad_hoc`: a one-off copy elsewhere leaves the offsets untouched, so the
        // configured destination still receives every one of these bytes.
        if !ad_hoc {
            if let Ok(text) = serde_json::to_string(&state) {
                let _ = std::fs::write(&state_path, text);
            }
        }
        if errors.is_empty() {
            Ok(SyncResult { sent, ad_hoc })
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

    fn send_http(
        &self,
        url: &str,
        key: Option<&str>,
        stream: &str,
        seq: u64,
        chunk: &[u8],
    ) -> Result<(), String> {
        let headers = [
            ("Content-Type", "application/x-ndjson".to_string()),
            ("X-Likhi-Install", self.install_id.clone()),
            ("X-Likhi-Stream", stream.split('.').next().unwrap_or(stream).to_string()),
            ("X-Likhi-Seq", seq.to_string()),
            ("X-Likhi-Version", "1".to_string()),
        ];
        let mut all: Vec<(&str, String)> = headers.to_vec();
        if let Some(k) = key {
            all.push(("X-Likhi-Key", k.to_string()));
        }
        crate::http::post(url, &all, chunk)
    }
}

/// What one `sync_to` did.
pub struct SyncResult {
    pub sent: usize,
    /// True when this went somewhere other than the configured destination, and so deliberately
    /// left the offsets where they were.
    pub ad_hoc: bool,
}

/// Python's `round(x, 1)`.
///
/// Not `(x * 10.0).round() / 10.0`, which was the first attempt here and is wrong: multiplying by
/// ten rounds first, so 3.15 -- whose nearest double is *below* 3.15 -- becomes exactly 31.5 and
/// then rounds up, where Python gives 3.1. Python rounds the true value of the double, half to
/// even, and Rust's float formatter does exactly the same thing, so formatting and reparsing is
/// both correct and obviously correct. Checked against CPython on the table in the tests below.
fn round1(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.1}").parse().unwrap_or(x)
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

    /// A unique empty directory under the system temp directory, removed when it goes out of scope.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "likhi-test-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn write(&self, rel: &str, text: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("create parent");
            }
            std::fs::write(&p, text).expect("write");
            p
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MACHINE: &str = r#"{"telemetry": "full",
        "telemetry_endpoint": "https://ingest.example/v1/ingest",
        "telemetry_key": "machine-key",
        "telemetry_sync_seconds": 3600}"#;

    /// No environment variables set, which is the normal case on a user's machine.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn discover(paths: &[PathBuf], user: &Path, env: &dyn Fn(&str) -> Option<String>) -> Config {
        Config::discover_with(paths, user, PathBuf::from("dir"), env)
    }

    // ------------------------------------------------------------------ where the config is found

    /// The regression this guards is a real one, reported as "telemetry never turns on": the
    /// installed layout puts the engine one directory below the config, and a search that only
    /// looked beside the executable found nothing on every fresh install.
    #[test]
    fn the_installed_layout_is_searched() {
        // Installed as <app>\engine\likhi-server.exe, with the config at <app>\.
        let exe_dir = PathBuf::from(r"C:\Program Files\Likhi\engine");
        let paths = config_paths_from(None, Some(&exe_dir), PathBuf::from("user.json"));
        for wanted in [
            r"C:\Program Files\Likhi\engine\likhi\config.json",
            r"C:\Program Files\Likhi\engine\config.json",
            r"C:\Program Files\Likhi\likhi\config.json",
            r"C:\Program Files\Likhi\config.json",
        ] {
            assert!(
                paths.contains(&PathBuf::from(wanted)),
                "{wanted} missing from {paths:?}"
            );
        }
    }

    #[test]
    fn the_per_user_file_is_searched_last() {
        let user = PathBuf::from(r"C:\Users\x\AppData\Local\Likhi\config.json");
        let paths = config_paths_from(None, Some(Path::new(r"C:\app")), user.clone());
        assert_eq!(paths.last(), Some(&user));
    }

    #[test]
    fn an_explicit_path_is_searched_first() {
        let explicit = PathBuf::from(r"C:\somewhere\config.json");
        let paths = config_paths_from(
            Some(explicit.clone()),
            Some(Path::new(r"C:\app")),
            PathBuf::from("user.json"),
        );
        assert_eq!(paths.first(), Some(&explicit));
    }

    #[test]
    fn an_explicit_config_wins() {
        let t = TempDir::new("explicit");
        let cfg = t.write(
            "somewhere/config.json",
            r#"{"telemetry": "full", "telemetry_endpoint": "https://example.test/v1/ingest"}"#,
        );
        let got = discover(&[cfg], Path::new("no-such-user.json"), &no_env);
        assert_eq!(got.mode, Mode::Full);
        assert_eq!(got.endpoint.as_deref(), Some("https://example.test/v1/ingest"));
    }

    #[test]
    fn no_config_anywhere_means_off() {
        let t = TempDir::new("missing");
        let got = discover(&[t.path("nope.json")], &t.path("also-nope.json"), &no_env);
        assert_eq!(got.mode, Mode::Off);
        assert_eq!(got.endpoint, None);
    }

    /// Administrators edit this file and Notepad writes a byte-order mark. Plain UTF-8 parsing
    /// rejects it, and that silently disabled telemetry once already.
    #[test]
    fn a_byte_order_mark_does_not_disable_telemetry() {
        let t = TempDir::new("bom");
        let cfg = t.write("config.json", &format!("\u{FEFF}{MACHINE}"));
        let got = discover(&[cfg], Path::new("no-such-user.json"), &no_env);
        assert_eq!(got.mode, Mode::Full);
        assert_eq!(got.endpoint.as_deref(), Some("https://ingest.example/v1/ingest"));
    }

    // ------------------------------------------------------------- the per-user file overrides it

    #[test]
    fn the_machine_config_alone() {
        let t = TempDir::new("machine");
        let cfg = t.write("machine/config.json", MACHINE);
        let got = discover(&[cfg], &t.path("local/Likhi/config.json"), &no_env);
        assert_eq!(got.mode, Mode::Full);
        assert_eq!(got.endpoint.as_deref(), Some("https://ingest.example/v1/ingest"));
        assert_eq!(got.sync_seconds, 3600.0);
    }

    /// Turning reporting off in the Likhi window must not erase where reports would go, or turning
    /// it back on would need an administrator to re-edit Program Files.
    #[test]
    fn a_user_can_turn_reporting_off_without_losing_the_destination() {
        let t = TempDir::new("off");
        let cfg = t.write("machine/config.json", MACHINE);
        let user = t.write("local/Likhi/config.json", r#"{"telemetry": "off"}"#);
        let got = discover(&[cfg], &user, &no_env);
        assert_eq!(got.mode, Mode::Off);
        assert_eq!(got.endpoint.as_deref(), Some("https://ingest.example/v1/ingest"));
        assert_eq!(got.key.as_deref(), Some("machine-key"));
    }

    #[test]
    fn a_user_can_turn_reporting_back_on() {
        let t = TempDir::new("on");
        let cfg = t.write("machine/config.json", MACHINE);
        let user = t.write("local/Likhi/config.json", r#"{"telemetry": "full"}"#);
        let got = discover(&[cfg], &user, &no_env);
        assert_eq!(got.mode, Mode::Full);
        assert_eq!(got.endpoint.as_deref(), Some("https://ingest.example/v1/ingest"));
    }

    #[test]
    fn a_corrupt_user_file_does_not_break_the_machine_config() {
        let t = TempDir::new("corrupt");
        let cfg = t.write("machine/config.json", MACHINE);
        let user = t.write("local/Likhi/config.json", "{ this is not json");
        let got = discover(&[cfg], &user, &no_env);
        assert_eq!(got.mode, Mode::Full);
        assert_eq!(got.endpoint.as_deref(), Some("https://ingest.example/v1/ingest"));
    }

    /// Reading the per-user file while it *is* the file being read would merge it into itself. The
    /// guard matters because that path is also a legitimate machine config on a per-user install.
    #[test]
    fn the_user_file_is_not_merged_into_itself() {
        let t = TempDir::new("self");
        let user = t.write("local/Likhi/config.json", MACHINE);
        let got = discover(std::slice::from_ref(&user), &user, &no_env);
        assert_eq!(got.mode, Mode::Full);
        assert_eq!(got.key.as_deref(), Some("machine-key"));
    }

    // --------------------------------------------------------------------- the environment wins

    #[test]
    fn the_environment_wins_over_both_files() {
        let t = TempDir::new("env");
        let cfg = t.write("machine/config.json", MACHINE);
        let user = t.write("local/Likhi/config.json", r#"{"telemetry": "off"}"#);
        let env = |k: &str| match k {
            "LIKHI_TELEMETRY" => Some("metrics".to_string()),
            _ => None,
        };
        assert_eq!(discover(&[cfg], &user, &env).mode, Mode::Metrics);
    }

    #[test]
    fn an_empty_environment_variable_is_not_a_setting() {
        // `discover` filters empty values before calling this, so an empty LIKHI_TELEMETRY must
        // fall through to the files rather than parsing as Off.
        let t = TempDir::new("empty");
        let cfg = t.write("machine/config.json", MACHINE);
        let env = |_: &str| None;
        assert_eq!(discover(&[cfg], Path::new("none.json"), &env).mode, Mode::Full);
    }

    // ----------------------------------------------------------------- what each mode may record
    //
    // These are the privacy guarantees the README makes, so they are tested as behaviour of the
    // files on disk rather than of any function: what matters is that nothing the user typed can be
    // found in them.

    fn local(dir: &Path, mode: Mode) -> Telemetry {
        Telemetry::new(Config {
            mode,
            dir: dir.to_path_buf(),
            drop: None,
            endpoint: None,
            key: None,
            sync_seconds: 900.0,
        })
    }

    fn commit(t: &Telemetry, roman: &str, chosen: &str, index: usize, top1: &str) {
        t.commit(roman, chosen, index, top1, "test.exe", false, false, Some(1.0));
    }

    fn read(dir: &Path, name: &str) -> Option<String> {
        std::fs::read_to_string(dir.join(name)).ok()
    }

    #[test]
    fn off_mode_writes_nothing() {
        let t = TempDir::new("off-mode");
        let tel = local(&t.0, Mode::Off);
        commit(&tel, "amr", "আমরা", 1, "আমার");
        tel.flush();
        assert_eq!(read(&t.0, "events.jsonl"), None);
        assert_eq!(read(&t.0, "metrics.jsonl"), None);
    }

    #[test]
    fn metrics_mode_counts_but_stores_no_text() {
        let t = TempDir::new("metrics-mode");
        let tel = local(&t.0, Mode::Metrics);
        commit(&tel, "amar", "আমার", 0, "");
        commit(&tel, "amr", "আমরা", 1, "আমার");
        tel.flush();
        assert_eq!(read(&t.0, "events.jsonl"), None, "metrics mode records no struggle text");
        let row = read(&t.0, "metrics.jsonl").expect("a counter row");
        let v: serde_json::Value = serde_json::from_str(row.trim()).expect("valid json");
        assert_eq!(v["words"], 2);
        assert_eq!(v["top1_taken"], 1);
        assert_eq!(v["pos_1"], 1);
        // The point of the mode: nothing typed appears anywhere in the row.
        for secret in ["amar", "amr", "আমার", "আমরা"] {
            assert!(!row.contains(secret), "{secret:?} leaked into a metrics row: {row}");
        }
    }

    #[test]
    fn full_mode_records_only_struggles() {
        let t = TempDir::new("full-mode");
        let tel = local(&t.0, Mode::Full);
        commit(&tel, "amar", "আমার", 0, ""); // taken first: nothing to learn
        commit(&tel, "amr", "আমরা", 2, "আমার"); // a struggle: recorded
        commit(&tel, "pin1234", "পিন", 1, "পিনা"); // has digits: redacted
        tel.flush();
        let text = read(&t.0, "events.jsonl").expect("an events file");
        let rows: Vec<serde_json::Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid json"))
            .collect();
        assert_eq!(rows.len(), 1, "only the struggle is recorded: {text}");
        assert_eq!(rows[0]["roman"], "amr");
        assert_eq!(rows[0]["chose"], "আমরা");
        assert_eq!(rows[0]["we_said"], "আমার");
        assert!(!text.contains("pin1234"), "a string with digits must never be written");
    }

    /// A password box. Nothing at all, not even a counter.
    #[test]
    fn a_secure_field_records_nothing() {
        let t = TempDir::new("secure");
        let tel = local(&t.0, Mode::Full);
        tel.commit("gopon", "গোপন", 1, "গোপনে", "test.exe", false, true, Some(1.0));
        tel.flush();
        assert_eq!(read(&t.0, "events.jsonl"), None);
        assert_eq!(read(&t.0, "metrics.jsonl"), None);
    }

    /// A machine can be shut down or killed without a clean exit, and TerminateProcess cannot be
    /// caught, so counters must not sit in memory waiting for a flush that never comes.
    #[test]
    fn counters_are_written_without_a_clean_shutdown() {
        let t = TempDir::new("nokill");
        let tel = local(&t.0, Mode::Metrics);
        for _ in 0..FLUSH_EVERY {
            commit(&tel, "amar", "আমার", 0, "");
        }
        // No flush() here on purpose.
        let text = read(&t.0, "metrics.jsonl").expect("counters on disk without a flush");
        let total: u64 = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v["words"].as_u64())
            .sum();
        assert_eq!(total, FLUSH_EVERY);
    }

    /// Telemetry must never cost a keystroke, so an unwritable destination is swallowed.
    #[test]
    fn an_unreachable_drop_does_not_break_writing() {
        let t = TempDir::new("baddrop");
        let tel = Telemetry::new(Config {
            mode: Mode::Full,
            dir: t.path("local"),
            // A path under a file, which cannot be created as a directory.
            drop: Some(t.write("a-file", "x").join("nope")),
            endpoint: None,
            key: None,
            sync_seconds: 900.0,
        });
        commit(&tel, "amr", "আমরা", 1, "আমার");
        tel.flush(); // must not panic
        assert!(read(&t.path("local"), "events.jsonl").is_some(), "the local file is still written");
    }

    // ------------------------------------------------ shipping elsewhere must not cost the pilot
    //
    // sync_state.json holds one offset per stream, not one per destination, because the configured
    // destinations are always shipped together. That makes a one-off sync elsewhere destructive
    // unless it refuses to record progress. Found the hard way in the Python: a run that shipped to
    // a temporary folder consumed part of a live install's telemetry.

    /// Local files written and nothing shipped yet, so the offsets start clean.
    ///
    /// The destination is attached *after* the flush on purpose: `flush` ships as soon as one is
    /// configured, so building with it would leave every assertion below comparing against an
    /// already-advanced offset instead of a clean slate.
    fn ready_to_ship(t: &TempDir, drop: Option<PathBuf>) -> Telemetry {
        let mut tel = Telemetry::new(Config {
            mode: Mode::Full,
            dir: t.path("state"),
            drop: None,
            endpoint: None,
            key: None,
            sync_seconds: 900.0,
        });
        commit(&tel, "amr", "আমার", 1, "আমি");
        tel.flush();
        tel.drop = drop;
        tel
    }

    fn offset(tel: &Telemetry) -> u64 {
        std::fs::read_to_string(tel.dir.join("sync_state.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("events.jsonl").and_then(serde_json::Value::as_u64))
            .unwrap_or(0)
    }

    fn any_jsonl(dir: &Path) -> bool {
        fn walk(dir: &Path) -> bool {
            std::fs::read_dir(dir).is_ok_and(|rd| {
                rd.filter_map(|e| e.ok()).any(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p)
                    } else {
                        p.extension().is_some_and(|x| x == "jsonl")
                    }
                })
            })
        }
        walk(dir)
    }

    #[test]
    fn a_configured_sync_records_progress() {
        let t = TempDir::new("configured");
        let tel = ready_to_ship(&t, Some(t.path("drop")));
        let r = tel.sync_to(None, None, None).expect("sync");
        assert!(!r.ad_hoc);
        assert!(r.sent > 0);
        assert!(offset(&tel) > 0);
        // A second round has nothing left to send.
        assert_eq!(tel.sync_to(None, None, None).expect("second sync").sent, 0);
    }

    #[test]
    fn an_ad_hoc_drop_does_not_consume_the_configured_destination() {
        let t = TempDir::new("adhoc");
        let real = t.path("real");
        let tel = ready_to_ship(&t, Some(real.clone()));
        let elsewhere = t.path("elsewhere");

        let r = tel.sync_to(Some(&elsewhere), None, None).expect("ad-hoc sync");
        assert!(r.ad_hoc, "a different destination is ad hoc");
        assert!(r.sent > 0);
        assert!(any_jsonl(&elsewhere), "the ad-hoc copy is still written");
        assert_eq!(offset(&tel), 0, "an ad-hoc sync must not record progress");

        // And the real destination still receives everything.
        let r = tel.sync_to(None, None, None).expect("configured sync");
        assert!(r.sent > 0, "the configured destination still gets the lines");
        assert!(any_jsonl(&real));
        assert!(offset(&tel) > 0);
    }

    #[test]
    fn an_ad_hoc_endpoint_does_not_consume_progress() {
        let t = TempDir::new("adhoc-ep");
        let tel = ready_to_ship(&t, Some(t.path("real")));
        // Unreachable on purpose: the point is that the offset is untouched either way.
        let _ = tel.sync_to(None, Some("http://127.0.0.1:9/v1/ingest"), None);
        assert_eq!(offset(&tel), 0);
        assert!(tel.sync_to(None, None, None).expect("configured sync").sent > 0);
    }

    /// What `likhi-report sync` does after reading the config: it passes the configured destination
    /// explicitly, and that must behave exactly like passing nothing.
    #[test]
    fn the_same_destination_passed_explicitly_is_not_ad_hoc() {
        let t = TempDir::new("same");
        let real = t.path("real");
        let tel = ready_to_ship(&t, Some(real.clone()));
        let r = tel.sync_to(Some(&real), None, None).expect("sync");
        assert!(!r.ad_hoc, "the configured destination is never ad hoc");
        assert!(offset(&tel) > 0);
    }

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

    /// Every expected value here was produced by running `round(v, 1)` in CPython, not derived.
    /// The interesting ones are 3.15 and 3.35, which look like symmetric ties and are not: the
    /// nearest double to 3.15 is below it and the nearest to 3.35 is above it, so they round in
    /// opposite directions.
    // 3.14 here is a rounding test case taken from CPython, not an approximation of pi.
    #[allow(clippy::approx_constant)]
    #[test]
    fn rounding_matches_python_round() {
        for (input, want) in [
            (3.14, 3.1),
            (3.15, 3.1),
            (3.25, 3.2),
            (3.35, 3.4),
            (2.0, 2.0),
            (0.05, 0.1),
            (0.15, 0.1),
            (27.0, 27.0),
            (1.005, 1.0),
            (12.349999, 12.3),
            (99.95, 100.0),
            (0.0, 0.0),
            (123.456, 123.5),
        ] {
            assert_eq!(round1(input), want, "round({input}, 1)");
        }
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
