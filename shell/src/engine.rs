//! The engine client: JSON lines over a local TCP socket, the protocol the PIME shell already used.
//!
//! One persistent connection, reopened on failure. Everything here runs on the application's UI
//! thread inside a key event, so two rules: never block longer than the deadline the caller gave,
//! and never let an error escape -- a dead engine must degrade to "type Latin", not to a crash in
//! someone's browser.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde::Deserialize;

use crate::log;

pub const DEFAULT_PORT: u16 = 47123;

/// How long a single suggestion may block. Matches `type_deadline_ms` / `commit_deadline_ms` in
/// the shell's config.json: the engine answers a typing request with whatever it has by the
/// deadline, and takes its time only when a word is being committed.
pub const TYPE_DEADLINE_MS: u32 = 12;
pub const COMMIT_DEADLINE_MS: u32 = 400;

/// Socket-level guard, well above any deadline: only trips when the engine has hung outright.
const SOCKET_TIMEOUT: Duration = Duration::from_millis(600);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Debug, Default, Clone)]
pub struct Suggestion {
    pub candidates: Vec<String>,
    /// True when the engine answered from its fast path because the deadline arrived first.
    pub partial: bool,
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
}

impl Engine {
    pub fn new(port: u16) -> Self {
        Engine {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            stream: None,
        }
    }

    fn connect(&mut self) -> std::io::Result<()> {
        let s = TcpStream::connect_timeout(&self.addr, CONNECT_TIMEOUT)?;
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(SOCKET_TIMEOUT))?;
        s.set_write_timeout(Some(SOCKET_TIMEOUT))?;
        self.stream = Some(BufReader::new(s));
        Ok(())
    }

    /// One request, one reply line. Reconnects once if the connection has gone away underneath us,
    /// which is what happens when the engine restarts.
    fn call(&mut self, request: &str) -> std::io::Result<String> {
        for attempt in 0..2 {
            if self.stream.is_none() {
                self.connect()?;
            }
            let result = (|| {
                let reader = self.stream.as_mut().expect("connected");
                reader.get_mut().write_all(request.as_bytes())?;
                reader.get_mut().write_all(b"\n")?;
                let mut line = String::new();
                let n = reader.read_line(&mut line)?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "engine closed the connection",
                    ));
                }
                Ok(line)
            })();
            match result {
                Ok(line) => return Ok(line),
                Err(e) => {
                    self.stream = None;
                    if attempt == 1 {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
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
        let line = match self.call(&request) {
            Ok(l) => l,
            Err(e) => {
                log!("engine unreachable: {e}");
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

    /// Tell the engine what was chosen, so it learns. Fire and forget: the reply is read only to
    /// keep the stream in step, and a failure costs nothing but one learning event.
    pub fn learn(&mut self, roman: &str, chosen: &str, context: &[String], top1: &str) {
        let request = serde_json::json!({
            "op": "learn",
            "roman": roman,
            "chosen": chosen,
            "context": context,
            "top1": top1,
        })
        .to_string();
        if let Err(e) = self.call(&request) {
            log!("learn not delivered: {e}");
        }
    }
}
