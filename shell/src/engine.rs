//! The engine client: JSON lines over a local TCP socket, the protocol the PIME shell already used.
//!
//! One persistent connection, reopened on failure. Everything here runs on the application's UI
//! thread inside a key event, so two rules: never block longer than the deadline the caller gave,
//! and never let an error escape -- a dead engine must degrade to "type Latin", not to a crash in
//! someone's browser.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::log;

/// How long a single suggestion may block. Matches `type_deadline_ms` / `commit_deadline_ms` in
/// the shell's config.json: the engine answers a typing request with whatever it has by the
/// deadline, and takes its time only when a word is being committed.
pub const TYPE_DEADLINE_MS: u32 = 12;
pub const COMMIT_DEADLINE_MS: u32 = 400;

/// Connecting is the one thing here that can stall with nothing to show for it, so it gets a
/// short leash -- and after a failure, no leash at all for a while: with the engine down, retrying
/// on every keystroke would cost every keystroke this much. Typing must stay instant even when the
/// suggestions are gone.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(120);
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// The read timeout is derived from the caller's deadline, not fixed: the engine promises to answer
/// within the deadline, so waiting much longer only means it is wedged, and a wedged engine must
/// not hold a keystroke for longer than a typist would notice.
fn read_timeout_for(deadline_ms: u32) -> Duration {
    Duration::from_millis(u64::from(deadline_ms) * 2 + 80)
}

#[derive(Debug, Default, Clone)]
pub struct Suggestion {
    pub candidates: Vec<String>,
    /// True when the engine answered from its fast path because the deadline arrived first.
    pub partial: bool,
}

/// What the engine is told when a word is committed, so it can learn and the pilot can count.
///
/// `index` is the candidate position taken, 0 for the first suggestion; it is how "first
/// suggestion taken" is measured, so it has to be sent for every commit and not only the corrected
/// ones. `retyped` marks a word the person backspaced inside. `app` is the executable being typed
/// into, for per-application counters.
pub struct Commit<'a> {
    pub roman: &'a str,
    pub chosen: &'a str,
    pub top1: &'a str,
    pub index: usize,
    pub retyped: bool,
    pub app: &'a str,
    pub context: &'a [String],
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    #[serde(default)]
    candidates: Vec<String>,
    #[serde(default)]
    partial: bool,
    #[serde(default)]
    error: Option<String>,
}

pub struct Engine {
    addr: SocketAddr,
    stream: Option<BufReader<TcpStream>>,
    /// Set after a failed connection; until then no further attempt is made.
    retry_at: Option<Instant>,
}

impl Engine {
    pub fn new(port: u16) -> Self {
        Engine {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            stream: None,
            retry_at: None,
        }
    }

    fn connect(&mut self) -> std::io::Result<()> {
        if let Some(at) = self.retry_at {
            if Instant::now() < at {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "engine was unreachable a moment ago; not retrying yet",
                ));
            }
        }
        match TcpStream::connect_timeout(&self.addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                s.set_nodelay(true)?;
                s.set_write_timeout(Some(Duration::from_millis(200)))?;
                self.stream = Some(BufReader::new(s));
                self.retry_at = None;
                Ok(())
            }
            Err(e) => {
                self.retry_at = Some(Instant::now() + RETRY_AFTER);
                Err(e)
            }
        }
    }

    /// One request, one reply line. Reconnects once if the connection has gone away underneath us,
    /// which is what happens when the engine restarts.
    fn call(&mut self, request: &str, read_timeout: Duration) -> std::io::Result<String> {
        let mut last_error = None;
        for _attempt in 0..2 {
            if self.stream.is_none() {
                self.connect()?;
            }
            let reader = self.stream.as_mut().expect("connected");
            let result = reader
                .get_mut()
                .set_read_timeout(Some(read_timeout))
                .and_then(|_| reader.get_mut().write_all(request.as_bytes()))
                .and_then(|_| reader.get_mut().write_all(b"\n"))
                .and_then(|_| {
                    let mut line = String::new();
                    match reader.read_line(&mut line)? {
                        0 => Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "engine closed the connection",
                        )),
                        _ => Ok(line),
                    }
                });
            match result {
                Ok(line) => return Ok(line),
                Err(e) => {
                    self.stream = None;
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.expect("two attempts, at least one error"))
    }

    pub fn suggest(
        &mut self,
        roman: &str,
        context: &[String],
        k: usize,
        deadline_ms: u32,
    ) -> Option<Suggestion> {
        let request = serde_json::json!({
            "op": "suggest",
            "roman": roman,
            "context": context,
            "k": k,
            "deadline_ms": deadline_ms,
        })
        .to_string();
        let line = match self.call(&request, read_timeout_for(deadline_ms)) {
            Ok(l) => l,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    log!("engine unreachable: {e}");
                }
                return None;
            }
        };
        match serde_json::from_str::<Reply>(&line) {
            Ok(r) if r.ok => Some(Suggestion {
                candidates: r.candidates,
                partial: r.partial,
            }),
            Ok(r) => {
                log!("engine error: {}", r.error.unwrap_or_default());
                None
            }
            Err(e) => {
                log!("engine sent something unreadable: {e}: {line}");
                None
            }
        }
    }

    /// Report a committed word so the engine learns and the pilot counts it. The reply is read only
    /// to keep the stream in step; a failure costs nothing but one learning event.
    pub fn learn(&mut self, commit: &Commit<'_>) {
        let request = serde_json::json!({
            "op": "learn",
            "roman": commit.roman,
            "chosen": commit.chosen,
            "top1": commit.top1,
            "index": commit.index,
            "retyped": commit.retyped,
            "app": commit.app,
            "context": commit.context,
        })
        .to_string();
        if let Err(e) = self.call(&request, read_timeout_for(COMMIT_DEADLINE_MS)) {
            if e.kind() != std::io::ErrorKind::WouldBlock {
                log!("learn not delivered: {e}");
            }
        }
    }
}
