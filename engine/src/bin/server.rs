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
}

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

/// True when a Likhi engine is already answering on this port.
fn already_running(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(1),
    ) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
    if s.write_all(b"{\"op\":\"ping\"}\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 256];
    use std::io::Read;
    match s.read(&mut buf) {
        Ok(n) => String::from_utf8_lossy(&buf[..n]).contains("\"ok\""),
        Err(_) => false,
    }
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

    if already_running(port) {
        log(&format!("an engine is already listening on 127.0.0.1:{port}; nothing to do"));
        return;
    }

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

    // Deliberately no SO_REUSEADDR: on Windows it lets a second process bind a port another is
    // already listening on, so two engines would silently split the keyboard's requests between
    // them, each with a different personal dictionary. Binding must fail instead.
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("cannot listen on 127.0.0.1:{port}: {e}"));
            return;
        }
    };
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
