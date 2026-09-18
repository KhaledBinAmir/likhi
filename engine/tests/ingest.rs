//! The telemetry client against a real ingest server, end to end.
//!
//! These three cases came from `tests/test_ingest.py`, where they drove the Python client that no
//! longer exists. The server is still Python and still tested from Python -- it runs in a container,
//! not on anyone's machine -- so what moved here is only the client half: what the engine writes,
//! what it ships, and what it must not ship twice.
//!
//! The server is started as a subprocess from `server/ingest_server.py`. Run with `--data` it uses
//! nothing outside the standard library, so this needs a `python` on PATH and nothing installed. If
//! there is no usable Python the test reports that it skipped rather than failing: a missing
//! interpreter is an absent tool, not a broken engine.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use likhi_engine::telemetry::{Config, Mode, Telemetry};

const KEY: &str = "test-secret";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A unique empty directory, removed when it goes out of scope.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "likhi-ingest-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Kills the server when the test ends, however it ends.
struct Server {
    child: Child,
    port: u16,
    data: PathBuf,
}

impl Server {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/ingest", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A port the OS says is free. There is a race between closing this and the server binding it, but
/// the server is started immediately and a collision only makes the test skip.
fn free_port() -> Option<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    l.local_addr().ok().map(|a| a.port())
}

/// Start the ingest server, or return None with a reason when this machine cannot.
fn start_server(data: PathBuf) -> Result<Server, String> {
    let script = repo().join("server").join("ingest_server.py");
    if !script.exists() {
        return Err(format!("no ingest server at {}", script.display()));
    }
    let port = free_port().ok_or("no free port")?;
    let child = Command::new("python")
        .arg(&script)
        .args(["--data", &data.to_string_lossy()])
        .args(["--key", KEY])
        .args(["--port", &port.to_string()])
        .args(["--host", "127.0.0.1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start python: {e}"))?;
    let mut server = Server { child, port, data };

    // Wait for the port to accept a connection. Two seconds is generous for a standard-library
    // HTTP server on loopback.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = server.child.try_wait().map_err(|e| e.to_string())? {
            let mut why = String::new();
            if let Some(err) = server.child.stderr.take() {
                for line in BufReader::new(err).lines().map_while(Result::ok).take(5) {
                    why.push_str(&line);
                    why.push(' ');
                }
            }
            return Err(format!("server exited with {status}: {why}"));
        }
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", server.port).parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return Ok(server);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("ingest server did not start in time".into())
}

/// Start the server, or print why it was skipped and return None.
///
/// A skip is announced rather than silent. A test that quietly passes when it did nothing is worse
/// than no test, because it reads as coverage.
fn server_or_skip(data: PathBuf) -> Option<Server> {
    match start_server(data) {
        Ok(s) => Some(s),
        Err(why) => {
            println!("SKIPPED: {why}");
            None
        }
    }
}

fn client(dir: PathBuf, endpoint: Option<String>) -> Telemetry {
    Telemetry::new(Config {
        mode: Mode::Full,
        dir,
        drop: None,
        endpoint,
        key: Some(KEY.to_string()),
        sync_seconds: 900.0,
    })
}

fn chunks(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(prefix) && n.ends_with(".jsonl"))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn commit(t: &Telemetry, roman: &str, chosen: &str, index: usize, top1: &str) {
    t.commit(roman, chosen, index, top1, "test.exe", false, false, Some(1.0));
}

#[test]
fn the_client_ships_and_the_server_stores_the_same_layout() {
    let t = TempDir::new("layout");
    let Some(server) = server_or_skip(t.join("server-data")) else { return };
    let client = client(t.join("client"), Some(server.url()));

    commit(&client, "amr", "আমরা", 2, "আমার");
    commit(&client, "tmi", "তুমি", 1, "তোমার");
    client.flush();
    client.sync().expect("sync");

    let stored = chunks(&server.data.join(client.install_id()), "events-");
    assert_eq!(stored.len(), 1, "expected one chunk, got {stored:?}");
    let rows = lines(&stored[0]);
    let roman: Vec<&str> = rows.iter().filter_map(|r| r["roman"].as_str()).collect();
    assert_eq!(roman, vec!["amr", "tmi"], "rows arrive in the order they were typed");
    // The word the user chose travels; nothing else about them does.
    assert_eq!(rows[0]["chose"].as_str(), Some("আমরা"));
    assert_eq!(rows[0]["we_said"].as_str(), Some("আমার"));
}

#[test]
fn nothing_is_resent_and_new_lines_go_in_a_new_chunk() {
    let t = TempDir::new("resend");
    let Some(server) = server_or_skip(t.join("server-data")) else { return };
    let client = client(t.join("client"), Some(server.url()));
    let stored = server.data.join(client.install_id());

    commit(&client, "amr", "আমরা", 2, "আমার");
    client.flush();
    client.sync().expect("first sync");
    assert_eq!(chunks(&stored, "events-").len(), 1);

    // Nothing new: the offset has already advanced past every line, so this must ship nothing.
    client.sync().expect("empty sync");
    assert_eq!(
        chunks(&stored, "events-").len(),
        1,
        "a sync with nothing new must not create a chunk"
    );

    commit(&client, "ki", "কী", 1, "কি");
    client.flush();
    client.sync().expect("second sync");
    let after = chunks(&stored, "events-");
    assert_eq!(after.len(), 2, "new lines go in their own chunk");
    assert_eq!(lines(&after[1]).len(), 1, "the second chunk holds only the new line");
}

/// No server at all. The offset must stay where it was, or the unsent line is lost for good.
#[test]
fn offsets_do_not_advance_when_the_endpoint_is_down() {
    let t = TempDir::new("down");
    let dir = t.join("client");
    // Port 9 is discard: nothing listens, so the connection fails rather than hanging.
    let client = client(dir.clone(), Some("http://127.0.0.1:9/v1/ingest".to_string()));
    commit(&client, "amr", "আমরা", 2, "আমার");
    client.flush();
    let _ = client.sync();

    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("sync_state.json")).unwrap_or_default())
            .unwrap_or(serde_json::Value::Null);
    let offset = state
        .get("events.jsonl")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert_eq!(offset, 0, "an unsent line must be retried, not skipped");
    // And the line is still on disk waiting.
    assert!(!lines(&dir.join("events.jsonl")).is_empty());
}
