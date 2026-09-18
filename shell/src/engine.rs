//! The engine client: JSON lines, over a named pipe where possible and a local socket otherwise.
//!
//! Everything here runs on the application's UI thread inside a key event, so two rules: never
//! block longer than the deadline the caller gave, and never let an error escape -- a dead engine
//! must degrade to "type Latin", not to a crash in someone's browser.
//!
//! The pipe comes first because a Store application cannot use the socket at all. An AppContainer
//! blocks loopback, and the text service runs inside the application, so in Unigram or WhatsApp the
//! socket simply fails. The socket stays as a fallback for an engine older than this change.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_MODE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
// ProcessIdToSessionId lives under RemoteDesktop rather than Threading: sessions are a Terminal
// Services concept, and a signed-in desktop is session 1 of the same mechanism.
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, ResetEvent, WaitForSingleObject,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::log;
use crate::vlog;

/// How long a single suggestion may block. Matches `type_deadline_ms` / `commit_deadline_ms` in
/// the shell's config.json: the engine answers a typing request with whatever it has by the
/// deadline, and takes its time only when a word is being committed.
pub const TYPE_DEADLINE_MS: u32 = 12;
pub const COMMIT_DEADLINE_MS: u32 = 400;

/// Connecting is the one thing here that can stall with nothing to show for it, so it gets a
/// short leash -- and after a failure, no leash at all for a while: with the engine down, retrying
/// on every keystroke would cost every keystroke this much. Typing must stay instant even when the
/// suggestions are gone.
const CONNECT_TIMEOUT_MS: u32 = 120;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(CONNECT_TIMEOUT_MS as u64);
const RETRY_AFTER: Duration = Duration::from_secs(2);
/// Shorter, used just after starting the engine: it needs about 130 ms to be ready and waiting the
/// full retry period would leave a word or two unsuggested for no reason.
const START_RETRY_AFTER: Duration = Duration::from_millis(600);
/// Don't try to start the engine more often than this. A failure to start is usually permanent --
/// no permission, or nothing installed -- and retrying per keystroke would fork a process per key.
const START_COOLDOWN: Duration = Duration::from_secs(30);

/// The read timeout is derived from the caller's deadline, not fixed: the engine promises to answer
/// within the deadline, so waiting much longer only means it is wedged, and a wedged engine must
/// not hold a key for longer than a typist would notice.
fn read_timeout_for(deadline_ms: u32) -> Duration {
    Duration::from_millis(u64::from(deadline_ms) * 2 + 80)
}

#[derive(Debug, Default, Clone)]
pub struct Suggestion {
    pub candidates: Vec<String>,
    /// True when the engine answered from its fast path because the deadline arrived first.
    pub partial: bool,
    /// True when that fast answer is an attested spelling of exactly what was typed, seen more than
    /// once. Asking again with the model then makes the answer worse more often than better, which
    /// is why a strong answer is never refined and never re-asked at commit.
    pub strong: bool,
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
    strong: bool,
    #[serde(default)]
    error: Option<String>,
}

/// The engine's pipe for this Windows session. Two people signed in to one machine each run their
/// own engine with their own personal dictionary, so the session is part of the name.
fn pipe_name() -> String {
    let mut session = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
    }
    format!(r"\\.\pipe\likhi-engine-s{session}")
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// A connection to the engine's named pipe, with overlapped reads so a deadline can be enforced.
///
/// A plain blocking read on a pipe cannot be given a timeout, and a keystroke that waits forever on
/// a wedged engine is the one failure this crate must not have. Each read is issued overlapped and
/// waited on with a timeout; on expiry the I/O is cancelled and the connection dropped, which is
/// what any other transport error does too.
struct PipeConnection {
    handle: HANDLE,
    event: HANDLE,
    pending: Vec<u8>,
}

impl PipeConnection {
    fn connect(name: &str) -> std::io::Result<Self> {
        let wide_name = wide(name);
        let open = || unsafe {
            CreateFileW(
                PCWSTR(wide_name.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        let handle = match open() {
            Ok(h) => h,
            // The engine creates its next pipe instance only after the previous one is taken, so
            // two applications connecting at once can find every instance busy for a moment. That
            // is a wait, not a failure -- but a short one, because this may be a keystroke.
            Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                unsafe {
                    let _ = WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), CONNECT_TIMEOUT_MS);
                }
                open().map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, format!("pipe: {e}"))
                })?
            }
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("pipe: {e}"),
                ))
            }
        };

        let event = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| std::io::Error::other(format!("event: {e}")))?;

        Ok(PipeConnection {
            handle,
            event,
            pending: Vec::new(),
        })
    }

    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        }
    }

    /// Wait for an overlapped operation, cancelling it if the deadline passes.
    ///
    /// On the timeout path the wait afterwards is not optional. `CancelIoEx` only *asks*; until the
    /// cancellation completes the kernel still owns the OVERLAPPED and the buffer, both of which
    /// are on this stack frame and about to go away. Waiting is bounded -- a cancelled I/O on a
    /// pipe completes at once -- and it is the difference between a timeout and memory corruption
    /// inside someone's browser.
    fn finish(&self, ov: &mut OVERLAPPED, timeout: Duration) -> std::io::Result<u32> {
        let ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        let timed_out = unsafe { WaitForSingleObject(self.event, ms) } != WAIT_OBJECT_0;
        if timed_out {
            unsafe {
                let _ = CancelIoEx(self.handle, Some(ov));
                let mut discarded = 0u32;
                let _ = GetOverlappedResult(self.handle, ov, &mut discarded, true);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "engine did not answer within the deadline",
            ));
        }
        let mut moved = 0u32;
        unsafe { GetOverlappedResult(self.handle, ov, &mut moved, false) }
            .map_err(|e| std::io::Error::other(format!("overlapped: {e}")))?;
        Ok(moved)
    }

    fn write_all(&mut self, data: &[u8], timeout: Duration) -> std::io::Result<()> {
        let mut sent = 0usize;
        while sent < data.len() {
            let mut ov = self.overlapped();
            unsafe {
                let _ = ResetEvent(self.event);
                let _ = WriteFile(self.handle, Some(&data[sent..]), None, Some(&mut ov));
            }
            sent += self.finish(&mut ov, timeout)? as usize;
        }
        Ok(())
    }

    /// One reply line, without its newline.
    fn read_line(&mut self, timeout: Duration) -> std::io::Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
                let line = self.pending.drain(..=at).collect::<Vec<_>>();
                return String::from_utf8(line[..line.len() - 1].to_vec())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "engine did not answer within the deadline",
                ));
            }
            let mut buf = [0u8; 8192];
            let mut ov = self.overlapped();
            unsafe {
                let _ = ResetEvent(self.event);
                let _ = ReadFile(self.handle, Some(&mut buf), None, Some(&mut ov));
            }
            let n = self.finish(&mut ov, left)? as usize;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "engine closed the pipe",
                ));
            }
            self.pending.extend_from_slice(&buf[..n]);
        }
    }
}

impl Drop for PipeConnection {
    fn drop(&mut self) {
        unsafe {
            let _ = CancelIoEx(self.handle, None);
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

enum Transport {
    Pipe(PipeConnection),
    Socket(BufReader<TcpStream>),
}

impl Transport {
    fn call(&mut self, request: &str, timeout: Duration) -> std::io::Result<String> {
        match self {
            Transport::Pipe(p) => {
                p.write_all(request.as_bytes(), timeout)?;
                p.write_all(b"\n", timeout)?;
                p.read_line(timeout)
            }
            Transport::Socket(reader) => {
                reader.get_mut().set_read_timeout(Some(timeout))?;
                reader.get_mut().write_all(request.as_bytes())?;
                reader.get_mut().write_all(b"\n")?;
                let mut line = String::new();
                match reader.read_line(&mut line)? {
                    0 => Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "engine closed the connection",
                    )),
                    _ => Ok(line),
                }
            }
        }
    }
}

pub struct Engine {
    addr: SocketAddr,
    pipe: String,
    transport: Option<Transport>,
    /// Set after a failed connection; until then no further attempt is made.
    retry_at: Option<Instant>,
}

/// The installed engine, found relative to this DLL rather than by searching or by configuration.
///
/// The installed layout is `<app>\shell\<arch>\LikhiTextService.dll` beside `<app>\engine\
/// likhi-server.exe`, so the executable is three directories up and across. Relative because it is
/// the one path that is right for every installation without anything being written down, and
/// because this code is running inside somebody else's process where nothing about the environment
/// can be assumed.
fn engine_exe() -> Option<std::path::PathBuf> {
    let dll = std::path::PathBuf::from(crate::module_path());
    if dll.as_os_str().is_empty() {
        return None;
    }
    // <arch> -> shell -> <app>
    let app = dll.parent()?.parent()?.parent()?;
    let exe = app.join("engine").join("likhi-server.exe");
    exe.exists().then_some(exe)
}

/// Start the engine, at most once every `START_COOLDOWN`. Returns whether a process was launched.
///
/// Best effort in the strictest sense: inside a sandboxed application this cannot work at all,
/// because an AppContainer may not launch an executable outside its own package. That is fine. The
/// same user is typing in other applications that are not sandboxed, and the first keystroke in any
/// of them brings the engine back for all of them.
fn start_engine_once() -> bool {
    use std::sync::Mutex;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);

    let Ok(mut last) = LAST.lock() else { return false };
    if let Some(at) = *last {
        if at.elapsed() < START_COOLDOWN {
            return false;
        }
    }
    *last = Some(Instant::now());

    let Some(exe) = engine_exe() else {
        log!("engine is not running and no installed likhi-server.exe was found next to this DLL");
        return false;
    };
    // CREATE_NO_WINDOW: this is a background service and the person is typing in something else.
    // DETACHED_PROCESS would also do, but it additionally denies the child a console it may want
    // for its own logging when run by hand.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt;
    match std::process::Command::new(&exe)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            log!("engine was not running; started {} (pid {})", exe.display(), child.id());
            true
        }
        Err(e) => {
            log!("engine was not running and could not be started ({e})");
            false
        }
    }
}

impl Engine {
    pub fn new(port: u16) -> Self {
        Engine {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            pipe: pipe_name(),
            transport: None,
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
        // The pipe first: it is the only transport a Store application can use, and it is no worse
        // anywhere else.
        match PipeConnection::connect(&self.pipe) {
            Ok(p) => {
                self.transport = Some(Transport::Pipe(p));
                self.retry_at = None;
                log!("engine connected over {}", self.pipe);
                return Ok(());
            }
            Err(e) => log!("pipe unavailable ({e}); trying the socket"),
        }
        match TcpStream::connect_timeout(&self.addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                s.set_nodelay(true)?;
                s.set_write_timeout(Some(Duration::from_millis(200)))?;
                self.transport = Some(Transport::Socket(BufReader::new(s)));
                log!("engine connected over {} (the pipe was not available)", self.addr);
                self.retry_at = None;
                Ok(())
            }
            Err(e) => {
                // Neither transport answered, so there is very likely no engine running. Start it.
                //
                // Worth doing because the alternative is what a Windows update did on 2026-09-18:
                // the machine restarted, the engine did not come back, and the keyboard did nothing
                // in every application with no indication why. A keyboard that repairs itself is
                // the difference between "it broke" and nobody noticing.
                //
                // Not waited for. The engine takes about 130 ms to be ready and this runs on a
                // keystroke, so this keystroke still goes without suggestions; the retry window is
                // shortened so the next one picks it up.
                let started = start_engine_once();
                self.retry_at = Some(Instant::now() + if started { START_RETRY_AFTER } else { RETRY_AFTER });
                Err(e)
            }
        }
    }

    /// One request, one reply line. Reconnects once if the connection has gone away underneath us,
    /// which is what happens when the engine restarts.
    fn call(&mut self, request: &str, timeout: Duration) -> std::io::Result<String> {
        let mut last_error = None;
        for _attempt in 0..2 {
            if self.transport.is_none() {
                self.connect()?;
            }
            let transport = self.transport.as_mut().expect("connected");
            match transport.call(request, timeout) {
                Ok(line) => return Ok(line),
                Err(e) => {
                    self.transport = None;
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
                strong: r.strong,
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

    /// Ask the engine to draw the candidate list, because this process cannot.
    ///
    /// Used only inside a sandboxed application, where a window created here never reaches the
    /// desktop. `rect` is in screen coordinates and is the same anchor the local window would have
    /// been placed against, so the list lands in the same place either way.
    ///
    /// Short deadline: this is on the keystroke path and a list that arrives late is worse than one
    /// that does not arrive, because the next keystroke is already replacing it.
    pub fn ui_show(&mut self, items: &[String], cursor: usize, rect: (i32, i32, i32, i32)) {
        let request = serde_json::json!({
            "op": "ui_show",
            "items": items,
            "cursor": cursor,
            "rect": [rect.0, rect.1, rect.2, rect.3],
        })
        .to_string();
        if let Err(e) = self.call(&request, read_timeout_for(TYPE_DEADLINE_MS)) {
            if e.kind() != std::io::ErrorKind::WouldBlock {
                vlog!("ui_show not delivered: {e}");
            }
        }
    }

    pub fn ui_hide(&mut self) {
        let request = r#"{"op":"ui_hide"}"#;
        if let Err(e) = self.call(request, read_timeout_for(TYPE_DEADLINE_MS)) {
            if e.kind() != std::io::ErrorKind::WouldBlock {
                vlog!("ui_hide not delivered: {e}");
            }
        }
    }

    /// Which transport is in use, for the round-trip test below. `None` before the first request.
    #[cfg(test)]
    fn transport_name(&self) -> Option<&'static str> {
        self.transport.as_ref().map(|t| match t {
            Transport::Pipe(_) => "pipe",
            Transport::Socket(_) => "socket",
        })
    }
}

/// These talk to a real engine, so they are ignored by default and run with
/// `cargo test -- --ignored --nocapture` while one is up. Mocking the transport would test the mock:
/// the thing worth checking is that a real pipe, with the security descriptor the engine sets,
/// answers a real request inside the deadline.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a running engine"]
    fn suggests_over_the_pipe() {
        let mut engine = Engine::new(47123);
        let answer = engine
            .suggest("amar", &[], 5, TYPE_DEADLINE_MS)
            .expect("the engine should answer");
        assert_eq!(engine.transport_name(), Some("pipe"), "should prefer the pipe");
        assert!(!answer.candidates.is_empty(), "amar should suggest something");
        println!("transport=pipe amar -> {:?}", answer.candidates);
    }

    /// A wedged engine must cost one deadline, not a hung application. There is no way to wedge the
    /// real engine on demand, so this checks the budget on the ordinary path: a full commit-deadline
    /// request still has to come back inside the timeout the caller would have allowed.
    #[test]
    #[ignore = "needs a running engine"]
    fn stays_inside_the_deadline() {
        let mut engine = Engine::new(47123);
        let _ = engine.suggest("ami", &[], 5, TYPE_DEADLINE_MS);

        let started = Instant::now();
        let answer = engine.suggest("bhalobasha", &[], 5, COMMIT_DEADLINE_MS);
        let took = started.elapsed();
        assert!(answer.is_some(), "the engine should answer");
        assert!(
            took < read_timeout_for(COMMIT_DEADLINE_MS),
            "took {took:?}, over the {:?} budget",
            read_timeout_for(COMMIT_DEADLINE_MS)
        );
        println!("bhalobasha took {took:?}");
    }

    /// With no engine at all, a keystroke must not pay for the discovery: the first attempt fails
    /// fast and the next ones are refused outright until the backoff expires.
    #[test]
    fn a_missing_engine_does_not_block_a_keystroke() {
        // Both transports have to be dead for this to mean anything: port 1 is reserved and nothing
        // listens there, and the pipe is named after nothing that exists.
        let mut engine = Engine::new(1);
        engine.pipe = r"\\.\pipe\likhi-engine-there-is-no-such-pipe".to_string();
        let first = Instant::now();
        assert!(engine.suggest("amar", &[], 5, TYPE_DEADLINE_MS).is_none());
        let first_took = first.elapsed();

        let second = Instant::now();
        assert!(engine.suggest("amar", &[], 5, TYPE_DEADLINE_MS).is_none());
        let second_took = second.elapsed();

        assert!(
            first_took < CONNECT_TIMEOUT * 3,
            "first attempt took {first_took:?}"
        );
        assert!(
            second_took < Duration::from_millis(5),
            "backoff should refuse instantly, took {second_took:?}"
        );
    }
}
