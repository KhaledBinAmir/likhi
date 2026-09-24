//! The Likhi engine server: one warm engine, queried over a named pipe and a local socket.
//! A port of `serve()` in `src/likhi/server.py`.
//!
//! Why a server at all: the text service loads inside every application that switches to the
//! keyboard. Keeping the engine in one long-lived process means one model in memory, no per-app
//! start-up cost, and a crash here never takes an application down.
//!
//! Protocol: JSON lines, one request per line.
//!
//! ```text
//! {"op": "suggest", "roman": "amr", "context": ["আমি"], "k": 5, "deadline_ms": 12}
//!   -> {"ok": true, "candidates": [...], "partial": false, "strong": false, "ms": 3.1}
//! {"op": "ping"}  -> {"ok": true, "version": "0.0.1"}
//! {"op": "learn", "roman": "amr", "chosen": "আমার", ...} -> {"ok": true}
//! ```
//!
//! Run: `likhi-server [--port 47123]`

// No console. This starts from the Run key at every sign-in, and as a console application Windows
// gave it a window: a black terminal appearing on the desktop at boot, on every machine, with the
// installation path as its title. Nobody wants that and nothing about it is dev-only.
//
// The console is still attached when there is one to attach to, so running it by hand from a
// terminal prints exactly as before -- see `attach_parent_console` below. Without that the
// subsystem change would have made the engine silent for whoever is debugging it, which is a poor
// trade for hiding a window.
#![windows_subsystem = "windows"]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use likhi_engine::core::{Engine, EngineOptions};
use likhi_engine::personal::PersonalStore;
use likhi_engine::service::{handle_request, SuggestService, DEFAULT_PORT};
use likhi_engine::telemetry::{Config as TelemetryConfig, Mode, Telemetry};

fn log(msg: &str) {
    println!("[likhi-server] {msg}");
    let _ = std::io::stdout().flush();
    log_to_file(msg);
}

/// Also to a file, because at sign-in there is no console to print to.
///
/// Without this the engine became silent on exactly the machines where something goes wrong: it
/// starts from the Run key with no terminal, so "engine ready in 444 ms" and every later warning
/// went nowhere. A pilot tester cannot send a log that was never written, and an evening was
/// already lost to a text service whose log existed but in a folder nobody was reading.
///
/// Opened and closed per line, like the text service's log: no handle is held, so a process that
/// dies loses nothing, and there is no lock on any hot path. Nothing here runs per keystroke.
fn log_to_file(msg: &str) {
    /// Past this the file is rotated to `engine.log.1`. A machine that runs for months should not
    /// fill a disk with startup lines.
    const MAX_BYTES: u64 = 1024 * 1024;

    let Some(base) = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("HOME")) else {
        return;
    };
    let dir = PathBuf::from(base).join("Likhi");
    let path = dir.join("engine.log");
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} [likhi-server] {msg}", local_stamp());
    }
}

/// Local wall-clock time to the second, matching the text service's log so the two can be read
/// side by side. Local rather than UTC because these are read next to someone saying "it broke
/// just now".
fn local_stamp() -> String {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::GetLocalTime;
        let t = unsafe { GetLocalTime() };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        String::from("--------- --:--:--")
    }
}

/// Borrow the console of whatever started us, if there is one.
///
/// A windows-subsystem binary gets no console, which is the point: started from the Run key it must
/// not put a terminal on the desktop. But started by hand from a terminal it should still print, so
/// it attaches to the parent's console when there is one. Fails harmlessly when there is not, which
/// is the normal case at sign-in.
#[cfg(windows)]
fn attach_parent_console() {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return; // Started with no console, which is the normal case at sign-in.
        }
        // Attaching gives the process a console but leaves its standard handles as they were, which
        // for a windows-subsystem binary is usually nothing at all. Opening CONOUT$ and installing
        // it is what actually makes `println!` appear.
        //
        // Only where there is not a handle already. A run with its output redirected to a file
        // arrives with perfectly good handles, and replacing those would send the output to the
        // console instead of the file the caller asked for.
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let mut console = None;
        for slot in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let existing = GetStdHandle(slot);
            let already_set = existing.map(|h| !h.is_invalid() && !h.0.is_null()).unwrap_or(false);
            if already_set {
                continue;
            }
            if console.is_none() {
                console = CreateFileW(
                    PCWSTR(name.as_ptr()),
                    (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    Default::default(),
                    None,
                )
                .ok();
            }
            if let Some(h) = console {
                let _ = SetStdHandle(slot, h);
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// Where the engine's data lives: `models/rust` beside the executable when installed, or in the
/// repository during development.
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LIKHI_MODELS") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        for up in [1usize, 2, 3] {
            let mut p = exe.clone();
            for _ in 0..up {
                p.pop();
            }
            let candidate = p.join("models");
            if candidate.join("indicxlit").join("model.lkw").exists() {
                return candidate;
            }
        }
    }
    // Development: the crate sits beside models/rust.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("models").join("rust"))
        .unwrap_or_else(|| PathBuf::from("models/rust"))
}

fn serve_socket(stream: TcpStream, svc: Arc<SuggestService>, tel: Arc<Telemetry>) {
    let _ = stream.set_nodelay(true);
    let Ok(write_half) = stream.try_clone() else { return };
    let mut out = write_half;
    let reader = BufReader::new(stream);
    for line in reader.split(b'\n') {
        let Ok(line) = line else { return };
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let mut reply = handle_request(&svc, &tel, &line);
        reply.push(b'\n');
        if out.write_all(&reply).is_err() || out.flush().is_err() {
            return;
        }
    }
}

fn main() {
    // Before anything is printed, so a run from a terminal still shows its output.
    attach_parent_console();

    let mut port = DEFAULT_PORT;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--port" && i + 1 < args.len() {
            port = args[i + 1].parse().unwrap_or(DEFAULT_PORT);
            i += 2;
        } else {
            i += 1;
        }
    }

    #[cfg(windows)]
    if likhi_engine::pipe::engine_present() {
        log(&format!("an engine is already serving {}; nothing to do", likhi_engine::pipe::pipe_name()));
        return;
    }

    // Bound before anything is loaded, because holding the port is what makes this the only engine.
    // Deliberately no SO_REUSEADDR: on Windows it lets a second process bind a port another is
    // already listening on, so two engines would silently split the keyboard's requests between
    // them, each with a different personal dictionary. Binding must fail instead.
    //
    // It used to be bound last. Then an engine started while another was still loading -- the Run
    // key at sign-in and the keyboard's own restart, a few hundred milliseconds apart -- loaded the
    // whole model and put up a second tray icon before finding out it was not needed. Connections
    // that arrive before loading finishes wait in the backlog and are answered once it does.
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("cannot listen on 127.0.0.1:{port} ({e}); another engine is probably starting"));
            return;
        }
    };

    let started = Instant::now();
    let dir = data_dir();
    let personal = match PersonalStore::open(None) {
        Ok(p) => Some(p),
        Err(e) => {
            // Learning is a feature, not a prerequisite: a locked or corrupt personal file must not
            // stop the keyboard from suggesting anything at all.
            log(&format!("personal dictionary unavailable ({e}); learning is off this session"));
            None
        }
    };
    let engine = match Engine::open(
        &dir,
        EngineOptions {
            // 10 non-beam candidates scored by the model: identical accuracy to 16 on every set
            // (chat 93.60 against 93.60, Dakshina 69.18 against 69.23) at 26% lower latency,
            // because the beam's own hypotheses carry their scores for free.
            model_scored: 10,
            personal,
            ..Default::default()
        },
    ) {
        Ok(e) => Arc::new(e),
        Err(e) => {
            log(&format!("cannot load the engine from {}: {e}", dir.display()));
            std::process::exit(1);
        }
    };
    // Warm the mapped pages before announcing readiness, so the first real keystroke does not pay
    // for the page faults.
    let _ = engine.suggest("ami", &[], 5, false);
    log(&format!("engine ready in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0));

    // Build the candidate window now rather than when the first sandboxed application asks for it.
    // Built lazily, the first request paid for creating a thread, a window, and the Direct2D and
    // DirectWrite factories -- while a text service sat blocked on the reply inside a keystroke.
    // Off the startup path too, so this does not delay the engine answering.
    likhi_engine::uihost::prewarm();

    // The daily update check. It sleeps ten minutes or more before its first look, so it never
    // competes with sign-in, and does nothing at all in a development build.
    log(&format!(
        "version {}",
        likhi_engine::update::PRODUCT_VERSION.unwrap_or("dev (updates off)")
    ));
    likhi_engine::notify::set_log(log);
    // The icon beside the clock: open Likhi, check for updates, exit. Also clears the marker a
    // previous "Exit Likhi" left, because starting the engine is how someone undoes an exit.
    likhi_engine::notify::start(log);
    likhi_engine::update::spawn(log, likhi_engine::notify::offer);

    let cfg = TelemetryConfig::discover();
    let mode = cfg.mode;
    let sync_seconds = cfg.sync_seconds;
    let where_to = {
        let mut parts = Vec::new();
        if let Some(d) = &cfg.drop {
            parts.push(d.display().to_string());
        }
        if let Some(e) = &cfg.endpoint {
            parts.push(e.clone());
        }
        if parts.is_empty() { "local only".to_string() } else { parts.join(", ") }
    };
    let telemetry = Arc::new(Telemetry::new(cfg));
    {
        let telemetry = Arc::clone(&telemetry);
        likhi_engine::notify::before_exit(move || telemetry.flush_local());
    }
    if mode != Mode::Off {
        log(&format!(
            "telemetry: {} (files in {}; ships to {})",
            mode.as_str(),
            telemetry.dir.display(),
            where_to
        ));
    }

    let svc = Arc::new(SuggestService::new(Arc::clone(&engine), 2048));
    let stop = Arc::new(AtomicBool::new(false));

    // One early round, then the steady interval. Waiting a full interval for the first upload means
    // a new install is invisible for that long, so there is no way to tell a working installation
    // from a silently disabled one; worse, a machine switched off before the first tick never
    // reports at all, which is the normal life of an office PC.
    {
        let telemetry = Arc::clone(&telemetry);
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("likhi-flush".into())
            .spawn(move || {
                let interval = Duration::from_secs_f64(sync_seconds.max(1.0));
                let first = std::cmp::min(Duration::from_secs(60), interval);
                std::thread::sleep(first);
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    telemetry.flush();
                    std::thread::sleep(interval);
                }
            })
            .ok();
    }

    // The named pipe, alongside the socket. A Store application runs in an AppContainer and cannot
    // open a loopback socket at all, so for Telegram, WhatsApp and Mail this is the only way the
    // keyboard can reach the engine. The socket stays for development tools and anything already
    // speaking it.
    #[cfg(windows)]
    {
        let svc = Arc::clone(&svc);
        let tel = Arc::clone(&telemetry);
        match likhi_engine::pipe::serve(
            move |raw| handle_request(&svc, &tel, raw),
            Arc::clone(&stop),
        ) {
            // Not fatal: desktop applications keep working over the socket, and the diagnostics
            // show which transport each one used.
            Ok(name) => log(&format!("listening on {name}")),
            Err(e) => log(&format!("named pipe unavailable ({e}); socket only")),
        }
    }

    log(&format!("listening on 127.0.0.1:{port}"));

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let svc = Arc::clone(&svc);
        let tel = Arc::clone(&telemetry);
        let _ = std::thread::Builder::new()
            .name("likhi-conn".into())
            .spawn(move || serve_socket(stream, svc, tel));
    }
}
