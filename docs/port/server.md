# Porting specification: `src/likhi/server.py`

Target audience: an implementer rewriting the Likhi engine server in Rust. The Rust shell in
`shell/src/engine.rs` is the client and **must keep working unchanged**.

Everything below was read from the source at the cited `file:line` and, where marked **verified**,
confirmed by running a throwaway engine (`.venv\Scripts\python.exe -m likhi.server --port <free>`
with `LOCALAPPDATA` redirected to a temp directory) and capturing the exact wire bytes.
Python in use: `3.12.10`.

Primary source: `C:\Users\khaled\src\likhi\src\likhi\server.py` (423 lines).
Supporting sources quoted where behaviour is defined there:

| File | Why it matters here |
| --- | --- |
| `src/likhi/pipe.py` | the named-pipe listener and its framing |
| `src/likhi/engine/core.py` | `suggest` / `fast_suggest` / `learn` semantics, `strong` |
| `src/likhi/telemetry.py` | what `learn` records, what `flush()` does |
| `src/likhi/engine/textnorm.py` | `normalize_roman`, `canonical` |
| `src/likhi/__init__.py` | `__version__ = "0.0.1"` |
| `shell/src/engine.rs` | the client contract that must not break |

---

## 1. Constants

Every number and string a port must reproduce exactly.

### 1.1 Declared in `server.py`

```python
DEFAULT_PORT = 47123                                                    # server.py:41
DEFAULT_DEADLINE_MS = 12.0                                              # server.py:42
WEAK_MATCH_DEADLINE_MS = float(os.environ.get("LIKHI_WEAK_WAIT_MS") or 0.0)   # server.py:55
COMMIT_DEADLINE_MS = 400.0                                              # server.py:56
```

- `DEFAULT_PORT = 47123` — also the CLI default (`server.py:415`) and `already_running`'s port.
- `DEFAULT_DEADLINE_MS = 12.0` — used **only** as the fallback when a `suggest` request omits
  `deadline_ms` (`server.py:167`).
- `WEAK_MATCH_DEADLINE_MS` — read from the environment **once, at import time**, from
  `LIKHI_WEAK_WAIT_MS`. `or 0.0` means an empty string or `"0"`… careful: `""` → `0.0`,
  but `"0"` is a non-empty string so `float("0")` → `0.0` too. Default **0.0**.
  Changing it at runtime has no effect.
- `COMMIT_DEADLINE_MS = 400.0` — **declared and never read anywhere in `server.py`**
  (verified: `grep COMMIT_DEADLINE_MS` finds it only at `server.py:56` plus the Rust side). It exists
  to document parity with `shell/src/engine.rs:39`. A port may keep it as a documented constant; it
  changes no behaviour.

### 1.2 Other hard-coded values in `server.py`

| Value | Where | Meaning |
| --- | --- | --- |
| `2048` | `server.py:62` | `SuggestService.cache_size`, the `full_cache` LRU bound. `serve()` uses the default (`server.py:268`). |
| `1` | `server.py:69` | `ThreadPoolExecutor(max_workers=1, ...)` — exactly one model worker. |
| `"likhi-model"` | `server.py:69` | `thread_name_prefix`. Cosmetic. |
| `5` | `server.py:166` | default `k` when `suggest` omits it. |
| `5` | `server.py:144` | default `k` of `top_candidate`. |
| `0` | `server.py:188` | default `index` for `learn`. |
| `""` | `server.py:177,178,189` | defaults for `roman`, `chosen`, `app`. |
| `False` | `server.py:190,191` | defaults for `retyped`, `secure`. |
| `2` | `server.py:174` | decimal places for the `ms` field. |
| `1.0` | `server.py:223` | `already_running` connect/recv timeout, seconds. |
| `256` | `server.py:225` | `already_running` `recv` size, bytes. |
| `b'{"op":"ping"}\n'` | `server.py:224` | exact probe bytes. |
| `b'"ok"'` | `server.py:225` | exact substring searched for in the probe reply. |
| `"127.0.0.1"` | `server.py:218,230` | default host for both `already_running` and `serve`. |
| `4` | `server.py:238` | `limit_blas_threads(4)`. |
| `"default"` | `server.py:243` | `personal_path`, i.e. `%LOCALAPPDATA%\Likhi\personal.sqlite`. |
| `10` | `server.py:247` | `model_scored`. |
| `"ami"` | `server.py:249` | the warm-up word. |
| `60.0` | `server.py:283` | the flusher's early round: `stop.wait(min(60.0, interval))`. |
| `900` | `server.py:381,402,406` | default telemetry sync seconds. |
| `1.0` | `pipe.py:180` | pipe accept-loop retry wait after a create failure. |

### 1.3 Exact strings printed to stdout

A port that is dropped into the existing installer should keep these, because
`tests/test_server_single_instance.py:65` asserts on `"already listening"` and the diagnostics
scripts read the rest.

```python
f"[likhi-server] an engine is already listening on {host}:{port}; nothing to do"   # server.py:232
f"[likhi-server] engine ready in {(time.perf_counter() - t0) * 1000:.0f} ms"       # server.py:250
f"[likhi-server] telemetry: {cfg['mode']} (files in {telemetry.dir}; ships to {where})"  # :258
f"[likhi-server] cannot listen on {host}:{port}: {e}"                             # server.py:265
f"[likhi-server] listening on {name}"                                             # server.py:302
f"[likhi-server] named pipe unavailable ({e}); socket only"                       # server.py:306
f"[likhi-server] listening on {host}:{port}"                                      # server.py:308
```

`where` is `", ".join(x for x in (cfg["drop"], cfg["endpoint"]) if x) or "local only"`
(`server.py:256`). Verified output of a live run:

```
[likhi-server] engine ready in 584 ms
[likhi-server] telemetry: full (files in C:\...\Likhi; ships to local only)
[likhi-server] listening on \\.\pipe\likhi-engine-s1
[likhi-server] listening on 127.0.0.1:56690
```

Note the ordering: the pipe line is printed **before** the socket line, and both after the
telemetry line.

---

## 2. Process lifecycle — `serve()` (`server.py:230-315`)

The ordering here is load-bearing. A port must keep it.

1. **Single-instance check.** `already_running(port, host)` (`server.py:231`). If true, print the
   "already listening" line and `return` — exit code 0, no listener, no engine load.
   `tests/test_server_single_instance.py:66` asserts `returncode == 0`: a second instance is a
   **clean no-op, not an error**.

2. **BLAS thread cap before NumPy is imported.**
   ```python
   from likhi.engine.threads import limit_blas_threads
   limit_blas_threads(4)            # server.py:235-238
   from likhi.engine.core import LikhiEngine   # server.py:239
   ```
   `limit_blas_threads` does `os.environ.setdefault(var, "4")` for
   `OPENBLAS_NUM_THREADS`, `OMP_NUM_THREADS`, `MKL_NUM_THREADS`, `NUMEXPR_NUM_THREADS`
   (`engine/threads.py:13-18`). **`setdefault`**: an already-set variable wins. The import of
   `likhi.engine.core` (and therefore NumPy) must come *after*. A Rust port with its own BLAS
   binding must apply the same cap before the first matmul.

3. **Engine construction.**
   ```python
   engine = LikhiEngine(personal_path="default", model_scored=10)   # server.py:242-248
   ```

4. **Warm-up.** `engine.suggest("ami")` (`server.py:249`) — full path, no context, `k=5`. Populates
   the engine's internal cache and forces BLAS to allocate its pools. Measured 584–1307 ms total
   startup on this machine.

5. **Telemetry.** `cfg = _telemetry_config()`, then
   `Telemetry(cfg["mode"], drop=cfg["drop"], endpoint=cfg["endpoint"], key=cfg["key"])`
   (`server.py:253-254`). Constructing `Telemetry` **writes `<dir>/install_id` if it is absent, even
   when the mode is `off`** (`telemetry.py:85-95`; verified — an `install_id` file appeared in a run
   with `LIKHI_TELEMETRY=off`).

6. **Bind.** `_Server((host, port), _Handler)` inside `try/except OSError` (`server.py:262-266`).
   On failure: print `cannot listen on …` and `return`. **The engine is already loaded at this
   point** — a bind failure costs the full model load.

7. **Inside `with srv_ctx as srv:`** (`server.py:267`), so `server_close()` runs on exit:
   - `srv.service = SuggestService(engine)` (`server.py:268`)
   - `srv.telemetry = telemetry` (`server.py:269`)
   - `stop = threading.Event()` (`server.py:270`)
   - `interval = float(cfg["sync_seconds"])` (`server.py:275`)
   - start the flusher thread, daemon (`server.py:277-288`)
   - on `win32` only, start the named-pipe listener (`server.py:294-306`)
   - print the socket line, `srv.serve_forever()` (`server.py:308-310`)
   - `except KeyboardInterrupt: pass` (`server.py:311-312`)
   - `finally: stop.set(); telemetry.flush()` (`server.py:313-315`)

`main()` (`server.py:409-418`) is `argparse` with one option, `--port` (int, default 47123), and
always returns `0`. The module is runnable as `python -m likhi.server` (`server.py:421-422`).

### 2.1 Single-instance locking — there is no lock

There is no mutex, no lock file, no port reservation. Two mechanisms, both partial:

**(a) A ping probe.**

```python
def already_running(port: int, host: str = "127.0.0.1") -> bool:
    """True when a Likhi engine is already answering on this port."""
    import socket

    try:
        with socket.create_connection((host, port), timeout=1.0) as s:
            s.sendall(b'{"op":"ping"}\n')
            return b'"ok"' in s.recv(256)
    except OSError:
        return False
```
(`server.py:218-227`)

Exact semantics a port must keep:
- Timeout 1.0 s applies to **both** the connect and the `recv` (it is the socket timeout set by
  `create_connection`). A socket timeout is an `OSError` subclass, so it returns `False`.
- The test is a **substring search for the three bytes `"ok"`** in the first ≤256 bytes, not a JSON
  parse. Anything that echoes `"ok"` would pass.
- A port that accepts but never answers returns `False`
  (`tests/test_server_single_instance.py:30-38`, verified by that test).

**(b) No `SO_REUSEADDR` on Windows.**

```python
class _Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = sys.platform != "win32"
    daemon_threads = True
```
(`server.py:209-215`; verified at runtime: `allow_reuse_address = False`, `daemon_threads = True`)

The comment at `server.py:210-213` is the rationale: on Windows `SO_REUSEADDR` lets a second process
bind a port already being listened on, which would silently split the keyboard's traffic between two
engines with different personal dictionaries. The bind must fail instead.

Other `ThreadingTCPServer` defaults that are inherited and matter:
`request_queue_size = 5` (listen backlog), `address_family = AF_INET`, `socket_type = SOCK_STREAM`.

**The hole a port must be aware of: the named pipe is not covered.** `already_running` checks only
the TCP port. **Verified empirically**: with the installed engine
(`C:\Program Files\Likhi\runtime\python\pythonw.exe -m likhi.server`) running and holding
`\\.\pipe\likhi-engine-s1`, a second same-user process called `pipe._create_instance()` on the same
name and it **succeeded** ("SECOND INSTANCE CREATED OK"). Both test engines started for this spec
printed `listening on \\.\pipe\likhi-engine-s1` while the real engine was live. So two engines on
different ports do split pipe clients between them. Reproducing this is not required; reproducing
the TCP behaviour is.

### 2.2 Shutdown

`stop.set()` then `telemetry.flush()` (`server.py:314-315`). `stop` is what ends the flusher loop
(`server.py:283-286`) and the pipe accept loop's retry wait (`pipe.py:179,189`). Note that the pipe
accept thread is normally blocked inside `ConnectNamedPipe` (`pipe.py:185`), which `stop` does not
interrupt; the thread is a daemon and dies with the process.

---

## 3. Transports

Both transports carry the **same** protocol and share one function, `handle_request`
(`server.py:150`). They differ in framing at the edges — see §3.3.

### 3.1 TCP socket

```python
class _Handler(socketserver.StreamRequestHandler):
    def handle(self) -> None:  # one connection may send many lines
        svc: SuggestService = self.server.service
        tel = self.server.telemetry
        for raw in self.rfile:
            self.wfile.write(handle_request(svc, tel, raw) + b"\n")
            self.wfile.flush()
```
(`server.py:200-206`)

- Address `127.0.0.1:47123` by default. Loopback only.
- One thread per connection (`ThreadingTCPServer`, `daemon_threads = True`). Many requests per
  connection, answered strictly in order on that connection.
- `StreamRequestHandler` defaults, verified at runtime: `rbufsize = -1` (buffered reader),
  `wbufsize = 0` (unbuffered writer — `flush()` is a formality), `timeout = None`
  (**a connection is never timed out server-side**), `disable_nagle_algorithm = False`
  (the server does **not** set `TCP_NODELAY`; the Rust client sets it on its own socket,
  `engine.rs:328`).
- `for raw in self.rfile` iterates **lines including the trailing `b"\n"`**. That newline is passed
  straight into `json.loads`. Verified: sending an empty line yields
  `{"ok": false, "error": "JSONDecodeError: Expecting value: line 2 column 1 (char 1)"}` — "line 2",
  "char 1", proving the newline is part of the decoded text.
- A final line with no trailing newline is still yielded and answered; then the iterator ends and
  the connection closes.
- When the client closes, the loop ends and the handler returns. No error is logged.

### 3.2 Named pipe (Windows only)

Started from `serve()` at `server.py:294-306`, guarded by `sys.platform == "win32"` and wrapped in
`try/except Exception`. **A pipe failure is not fatal**: print `named pipe unavailable (…); socket
only` and carry on.

```python
name = pipe_transport.serve(
    lambda raw: handle_request(srv.service, telemetry, raw),
    stop,
)
```
(`server.py:298-301`)

Name: `rf"\\.\pipe\likhi-engine-s{session_id()}"` (`pipe.py:85-86`), where `session_id()` is
`ProcessIdToSessionId(GetCurrentProcessId())` and falls back to `0` if the call fails
(`pipe.py:76-82`). The Rust client builds the identical name (`engine.rs:98-104`) — **any port must
produce byte-identical names**, including the `s` prefix and no zero padding.

Creation parameters (`pipe.py:108-121`):

```python
kernel32.CreateNamedPipeW(
    name,
    PIPE_ACCESS_DUPLEX,                                    # 0x00000003
    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,       # 0 | 0 | 0 == 0
    PIPE_UNLIMITED_INSTANCES,                              # 255
    BUFFER_BYTES, BUFFER_BYTES,                            # 64 * 1024 each way
    0,                                                     # default timeout
    security_attributes,
)
```

Security descriptor — this is the whole point of the pipe and is trivially got wrong
(`pipe.py:56-63`):

```python
ALL_APPLICATION_PACKAGES = "AC"
ALL_RESTRICTED_APPLICATION_PACKAGES = "S-1-15-2-2"
SDDL = (
    "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)"
    f"(A;;GRGW;;;{ALL_APPLICATION_PACKAGES})"
    f"(A;;GRGW;;;{ALL_RESTRICTED_APPLICATION_PACKAGES})"
    "S:(ML;;NW;;;LW)"
)
```

Both the `AC` ACE *and* the literal `S-1-15-2-2` ACE are required (LPAC processes such as Edge do
not carry `AC`), and the `S:(ML;;NW;;;LW)` low mandatory label is required or a low-integrity
AppContainer is refused after passing every other check (`pipe.py:9-16, 51-55`). The descriptor is
deliberately never freed (`pipe.py:103-104`).

Accept loop (`pipe.py:178-196`):

```python
def accept_loop() -> None:
    while not stop.is_set():
        try:
            handle = _create_instance(name, sa)
        except OSError:
            stop.wait(1.0)
            continue
        connected = kernel32.ConnectNamedPipe(wintypes.HANDLE(handle), None)
        if not connected and ctypes.get_last_error() != ERROR_PIPE_CONNECTED:  # 535
            kernel32.CloseHandle(wintypes.HANDLE(handle))
            continue
        if stop.is_set():
            kernel32.CloseHandle(wintypes.HANDLE(handle))
            return
        threading.Thread(target=_serve_client, args=(handle, handle_line), daemon=True).start()
```

**Exactly one unconnected instance exists at a time**: the next instance is created only after the
previous one has been taken. The Rust client depends on this and handles `ERROR_PIPE_BUSY` with
`WaitNamedPipeW(name, 120)` (`engine.rs:138-148`). A port may pre-create more instances — the client
tolerates that — but it must never create *fewer*, and it must keep answering `ERROR_PIPE_BUSY`
rather than failing outright when instances are momentarily exhausted.

`serve()` returns the pipe name synchronously after starting the accept thread (`pipe.py:196-197`),
so the printed `listening on …` line does **not** prove an instance was created.

Per-connection framing (`pipe.py:147-165`):

```python
def _serve_client(handle, handle_line):
    pending = b""
    try:
        while True:
            chunk = _read(handle)          # ReadFile, up to 64 KiB
            if not chunk:
                return
            pending += chunk
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                if not line.strip():
                    continue
                if not _write(handle, handle_line(line) + b"\n"):
                    return
    finally:
        kernel32.FlushFileBuffers(handle)
        kernel32.DisconnectNamedPipe(handle)
        kernel32.CloseHandle(handle)
```

### 3.3 The two transports are not byte-identical — preserve both behaviours

| | socket (`server.py:204`) | pipe (`pipe.py:156-159`) |
| --- | --- | --- |
| what reaches `handle_request` | the line **including** its `b"\n"` | the line **without** its `b"\n"` |
| a blank / whitespace-only line | answered with a `JSONDecodeError` reply (**verified**) | silently skipped, **no reply at all** |
| read buffer | Python's default buffered reader | 64 KiB chunks, manual `\n` split |
| unbounded growth | reader handles it | `pending` grows without bound if a client never sends `\n` |

The blank-line difference is real and observable. Verified socket replies:

```
""     -> {"ok": false, "error": "JSONDecodeError: Expecting value: line 2 column 1 (char 1)"}
"   "  -> {"ok": false, "error": "JSONDecodeError: Expecting value: line 2 column 1 (char 4)"}
```

A port that unifies the two framings will change the socket's reply stream alignment for any client
that sends blank lines. The Rust shell never does, so unifying is *probably* safe — but it is a
deliberate deviation, not a free simplification.

---

## 4. Wire protocol

JSON, one object per line, UTF-8, `\n`-terminated in both directions.

### 4.1 Request dispatch

```python
req = json.loads(raw.decode("utf-8"))
op = req.get("op", "suggest")
```
(`server.py:157-158`)

- The payload must decode as UTF-8 and parse as JSON. Anything else → error reply (§4.6).
- `op` defaults to `"suggest"` when the key is absent. **`{}` is a valid `suggest` request.**
  Verified: `{}` → `{"ok": true, "candidates": [], "partial": true, "strong": false, "ms": 15.11}`.
- `op` present but `null` is **not** the default — it becomes `unknown op None` (verified).
- Recognised ops, in the order tested: `"ping"`, `"suggest"`, `"learn"`. Anything else → §4.6.
- The whole body is inside one `try/except Exception` (`server.py:156,195`) — "Never raises: a
  malformed request from one keyboard must not take down the connection, let alone the engine that
  every other application is talking to" (`server.py:153-155`).

### 4.2 Response serialisation

```python
return json.dumps(resp, ensure_ascii=False).encode("utf-8")
```
(`server.py:197`)

- `ensure_ascii=False`: Bangla goes out as **raw UTF-8**, never `\uXXXX` escapes. Verified:
  `{"ok": true, "candidates": ["আমার", …]}`.
- Python's default separators produce `", "` and `": "` — i.e. `{"ok": true, "version": "0.0.1"}`
  **with spaces**. `serde_json::to_string` emits no spaces. The client parses JSON, so this is
  cosmetic; noted so byte-level comparisons between old and new are not a surprise.
- Key order is dict-insertion order; see the per-op sections.
- The result never contains a newline, so appending `b"\n"` is a valid frame terminator.
- The caller (socket handler `server.py:205`, pipe `pipe.py:160`) appends the `b"\n"`;
  `handle_request` itself does not.

### 4.3 `op: "ping"`

Request: `{"op": "ping"}`. No other field is read.

Response (`server.py:160`):

```json
{"ok": true, "version": "0.0.1"}
```

`version` is `likhi.__version__` (`__init__.py:3`), a plain string. Verified byte-for-byte.
`already_running` (§2.1) is the only in-tree caller. The Rust shell never sends `ping`.

### 4.4 `op: "suggest"`

Request fields (`server.py:162-168`):

| Field | Default | Coercion | On a bad value |
| --- | --- | --- | --- |
| `roman` | `""` | passed through verbatim | non-string → `AttributeError: 'int' object has no attribute 'casefold'` (verified; raised by `normalize_roman`, `textnorm.py:101`) |
| `context` | `()` | `tuple(req.get("context", ()))` | `null` → `TypeError: 'NoneType' object is not iterable` (verified); a **string** is iterated per character — `"abc"` becomes `("a","b","c")` (verified, answered normally) |
| `k` | `5` | `int(...)` | `"3"` → 3 (verified); `"x"` → `ValueError: invalid literal for int() with base 10: 'x'` (verified) |
| `deadline_ms` | `DEFAULT_DEADLINE_MS` = `12.0` | `float(...)` | non-numeric string → `ValueError` |

The timing window is exactly these lines and nothing else:

```python
t = time.perf_counter()
cands, partial, strong = svc.suggest(
    req.get("roman", ""),
    tuple(req.get("context", ())),
    int(req.get("k", 5)),
    float(req.get("deadline_ms", DEFAULT_DEADLINE_MS)),
)
resp = {
    "ok": True,
    "candidates": cands,
    "partial": partial,
    "strong": strong,
    "ms": round((time.perf_counter() - t) * 1000, 2),
}
```
(`server.py:162-175`)

Response fields, in this order:

| Field | Type | Meaning |
| --- | --- | --- |
| `ok` | `true` | always present |
| `candidates` | array of strings | ranked best-first, at most `k` entries, possibly empty |
| `partial` | bool | the answer came from the model-free fast path because the deadline expired — see §5 |
| `strong` | bool | that fast answer is an attested spelling seen more than once — see §6 |
| `ms` | number | `round(elapsed_ms, 2)`; **always a JSON float**, e.g. `0.0`, `3.17`, `322.81`. Measures only `svc.suggest`, excluding JSON parse and serialise. |

Verified samples:

```
{"ok": true, "candidates": ["আমার","আমি","আমরা","ভাই","ভাইয়া"], "partial": true,  "strong": true,  "ms": 29.68}
{"ok": true, "candidates": ["ভালবাসা","ভালোবাসা","ভালোবাসায়","ভালোবাশা","ভালোবাসে"], "partial": false, "strong": false, "ms": 322.81}
{"ok": true, "candidates": [], "partial": true, "strong": false, "ms": 12.58}     # roman ""
{"ok": true, "candidates": [], "partial": false, "strong": false, "ms": 241.82}   # k = 0
```

`k` edge cases (both verified against the live engine):
- `k = 0` → `[]`, because the engine slices `ranked[:0]` (`core.py:536`).
- `k = -1` → **every candidate except the last**, because `ranked[:-1]` is a Python negative slice.
  A run returned **82** candidates. A Rust port must reproduce this or the caller's `candidates`
  vector silently changes shape for a malformed request. See §11.

### 4.5 `op: "learn"`

```python
roman = req.get("roman", "")
chosen = req.get("chosen", "")
context = tuple(req.get("context", ()))
svc.learn(roman, chosen, context)
if tel is not None and tel.mode != "off":
    tel.commit(
        roman,
        chosen,
        index=int(req.get("index", 0)),
        top1=req.get("top1") or svc.top_candidate(roman, context),
        app=req.get("app", ""),
        retyped=bool(req.get("retyped", False)),
        secure=bool(req.get("secure", False)),
        latency_ms=req.get("latency_ms"),
    )
resp = {"ok": True}
```
(`server.py:176-192`)

| Field | Default | Coercion | Used for |
| --- | --- | --- | --- |
| `roman` | `""` | verbatim | learning + telemetry |
| `chosen` | `""` | verbatim | learning + telemetry |
| `context` | `()` | `tuple(...)` | learning + the `top_candidate` lookup |
| `index` | `0` | `int(...)` | telemetry only |
| `top1` | — | `req.get("top1") or svc.top_candidate(roman, context)` | telemetry only |
| `app` | `""` | verbatim | telemetry only |
| `retyped` | `False` | `bool(...)` | telemetry only |
| `secure` | `False` | `bool(...)` | telemetry only — suppresses the whole record |
| `latency_ms` | `None` | verbatim (`Telemetry` does `float(...)`) | telemetry only |

Response, always and only:

```json
{"ok": true}
```

Verified: `{"op":"learn"}` with **no** other field returns `{"ok": true}` — empty `roman`/`chosen`
makes `engine.learn` a no-op (`core.py:249-250`) but the reply is unchanged.

`svc.learn` (`server.py:139-142`):

```python
def learn(self, roman: str, chosen: str, context: tuple[str, ...]) -> None:
    with self.lock:
        self.engine.learn(roman, chosen, context)
    self.full_cache.clear()
```

**`full_cache.clear()` is unconditional and total** — every roman, every k, every context. Verified:
after `learn` on the unrelated roman `"nonsenseword"`, a previously-cached `"porikkha"` request went
from a 0.2 ms cache hit back to a 3.3 ms `partial: true` fast answer.

`top_candidate` (`server.py:144-147`):

```python
def top_candidate(self, roman: str, context: tuple[str, ...], k: int = 5) -> str:
    """What we would have shown first, for telemetry. Cache only, never computes."""
    cached = self.full_cache.get(self._key(roman, context, k))
    return cached[0] if cached else ""
```

**It can effectively never return anything on the `learn` path**, because `svc.learn` clears the
cache two lines earlier (`server.py:180` then `server.py:186`). **Verified**: with the cache freshly
warm for `bondhu` (confirmed by a 0.2 ms hit immediately before), a `learn` that omitted `top1`
wrote `"we_said": ""` to `events.jsonl`; the same `learn` *with* `top1` wrote
`"we_said": "বন্ধু"`. A port should reproduce the observable result (`""`) rather than the
apparent intent; if it "fixes" the ordering, telemetry rows change meaning and the pilot's
first-suggestion-accuracy numbers shift. Note also that `top_candidate`'s `k` is hard-wired to `5`,
so a client using `k != 5` would miss anyway. This is the single most likely place for a port to
diverge by accident.

### 4.6 Errors — the exact shape

Two producers, one shape. `ok` is always present and always `false`.

**Unknown op** (`server.py:193-194`):

```python
resp = {"ok": False, "error": f"unknown op {op!r}"}
```

`{op!r}` is Python `repr`.

| request | reply | how checked |
| --- | --- | --- |
| `{"op":"frobnicate"}` | `{"ok": false, "error": "unknown op 'frobnicate'"}` | over the wire |
| `{"op":null}` | `{"ok": false, "error": "unknown op None"}` | over the wire |
| `{"op":5}` | `{"ok": false, "error": "unknown op 5"}` | over the wire |
| `{"op":true}` | `{"ok": false, "error": "unknown op True"}` | f-string evaluated directly |
| `{"op":["x"]}` | `{"ok": false, "error": "unknown op ['x']"}` | f-string evaluated directly |
| `{"op":"it's"}` | `{"ok": false, "error": "unknown op \"it's\""}` | f-string evaluated directly |

Note `repr` switches to double quotes when the string contains an apostrophe — a Rust port that
always emits single quotes diverges on that one case.

**Any exception** (`server.py:195-196`):

```python
except Exception as e:  # never let one bad request kill the connection
    resp = {"ok": False, "error": f"{type(e).__name__}: {e}"}
```

Format is exactly `"<ExceptionClassName>: <str(exception)>"` — the bare class name, no module
prefix. Verified:

| request | reply `error` |
| --- | --- |
| `not json` | `JSONDecodeError: Expecting value: line 1 column 1 (char 0)` |
| `` (empty line, socket) | `JSONDecodeError: Expecting value: line 2 column 1 (char 1)` |
| `[1,2]` | `AttributeError: 'list' object has no attribute 'get'` |
| `{"op":"suggest","context":null,…}` | `TypeError: 'NoneType' object is not iterable` |
| `{"op":"suggest","k":"x",…}` | `ValueError: invalid literal for int() with base 10: 'x'` |
| `{"op":"suggest","roman":123,…}` | `AttributeError: 'int' object has no attribute 'casefold'` |

A Rust port cannot reproduce Python exception text verbatim for every case, and does not need to:
`shell/src/engine.rs:392-395` only logs `r.error` and returns `None`. **What must be preserved is
the shape**: `ok: false` plus a `error` string field, on the same connection, one line, with the
connection left open for the next request. See §9 for why `ok` must always be present.

Non-UTF-8 bytes on the wire: `raw.decode("utf-8")` raises `UnicodeDecodeError`, caught by the same
handler → `{"ok": false, "error": "UnicodeDecodeError: 'utf-8' codec can't decode byte …"}`.

---

## 5. The deadline mechanism

This is the heart of the module. Full source (`server.py:59-137`), then the rules.

### 5.1 State

```python
def __init__(self, engine, cache_size: int = 2048) -> None:
    self.engine = engine
    self.lock = threading.Lock()
    self.pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="likhi-model")
    self.pending: dict[tuple, Future] = {}
    self.full_cache: OrderedDict[tuple, list[str]] = OrderedDict()
    self.cache_size = cache_size
    self.latest_key: tuple | None = None  # most recent input; older jobs may abort
    self.waiting: set[tuple] = set()      # keys a request is currently blocked on
```
(`server.py:62-74`)

`self.lock` guards **only** `engine.learn` (`server.py:140-141`). The comment at `server.py:63-67`
is the design rule and a port must honour it:

> Only `learn` takes this lock. Reads are lock-free: NumPy releases the GIL, marisa lookups and
> lru_cache are thread-safe, and the SQLite store allows cross-thread use. Holding a lock around the
> model call would make the fast path wait for the model, defeating the deadline (measured: 120 ms
> round trips instead of 12).

`pending`, `full_cache`, `waiting` and `latest_key` are mutated from request threads **and** from the
model worker thread (`_done`) with **no lock at all**. CPython's GIL makes each individual dict/set
operation atomic; Rust has no such guarantee. See §11.

### 5.2 The cache key

```python
def _key(self, roman: str, context: tuple[str, ...], k: int) -> tuple:
    return (roman, context[-1:] if context else (), k)
```
(`server.py:76-77`)

- `roman` is the **raw** string from the request — not normalised, not case-folded.
- `context[-1:]` is a **tuple slice**: a 1-tuple holding the last context word, or `()` for an empty
  context. Only the last word participates. **Verified**: after warming `{"roman":"porikkha",
  "context":["ami"]}`, a request with `"context":["tumi","ami"]` was a 0.1 ms cache hit with
  identical candidates, while `"context":["ami","tumi"]` missed.
- `k` is the integer after coercion. **Verified**: `k=4` missed a `k=5` entry.
- The last context word is stored **raw**, while the engine canonicalises it
  (`core.py:593: prev = (canonical(context[-1]),)`). Two spellings that canonicalise to the same
  word therefore get two cache entries with identical contents. Likewise `"Amar"` and `"amar"` are
  two service-cache keys for one engine result, because `normalize_roman` case-folds
  (`textnorm.py:96-101`) but `_key` does not.

### 5.3 The full (model) computation and its abort callback

```python
def _full(self, roman: str, context: tuple[str, ...], k: int) -> list[str]:
    key = self._key(roman, context, k)
    # Abandon this job between model stages if a newer input has superseded it, unless someone
    # is still waiting for exactly this key (a commit re-ask).
    return self.engine.suggest(
        roman, context, k, abort=lambda: self.latest_key != key and key not in self.waiting
    )
```
(`server.py:79-85`)

- Runs on the single pool thread.
- `roman` is passed **raw**; `context` is passed **whole** (not the `[-1:]` slice). `engine.suggest`
  normalises and takes `prev = (canonical(context[-1]),)` itself (`core.py:592-593`).
- The abort predicate is exactly:
  **`self.latest_key != key AND key not in self.waiting`**.
  Abort when a newer input has arrived *and* nobody is currently blocked on this key.
- Because `abort is not None`, `engine.suggest` **bypasses the engine's own `_cache` entirely**
  (`core.py:594-596`):
  ```python
  if abort is not None:
      # abortable computations bypass the cache (they may be dropped part-way)
      return list(self._suggest(r, k, prev, not fast, abort))
  ```
  So the engine's 4096-entry `_cache` is dead weight in server use — only the warm-up `suggest("ami")`
  ever fills it. The `SuggestService.full_cache` is the only cache that matters.

**Where `abort` is actually checked** — only two places, both in `LikhiEngine.candidates`:

1. `core.py:391-392`, immediately before the encoder + beam search:
   ```python
   if abort is not None and abort():
       raise AbortedError(r)
   enc_kv = self.xlit.encode(r)
   ```
2. `core.py:433-434`, immediately before the batched teacher-forced scoring pass:
   ```python
   if abort is not None and abort():
       raise AbortedError(r)
   ```

So a job is abandoned at most twice: before the beam and before the rescoring. It is **not**
interruptible inside either stage. `AbortedError` (`core.py:150-151`) propagates out of `_full` and
lands in the `Future` as an exception, which `_done` uses to skip caching (§5.5).

### 5.4 `suggest` — the deadline algorithm

```python
def suggest(self, roman, context, k, deadline_ms) -> tuple[list[str], bool, bool]:
    key = self._key(roman, context, k)
    self.latest_key = key
    cached = self.full_cache.get(key)
    if cached is not None:
        self.full_cache.move_to_end(key)
        return cached, False, False
    fut = self.pending.get(key)
    if fut is None:
        # Earlier prefixes of the word being typed are stale: drop the ones not started yet so
        # the single model worker gets to the current input (and to commit-time re-asks) fast.
        for other_key, other in list(self.pending.items()):
            if other_key != key and other.cancel():
                self.pending.pop(other_key, None)
        fut = self.pool.submit(self._full, roman, context, k)
        self.pending[key] = fut
        fut.add_done_callback(lambda f, key=key: self._done(key, f))
    self.waiting.add(key)
    try:
        try:
            return fut.result(timeout=max(0.0, deadline_ms) / 1000.0), False, False
        except Exception:
            pass  # timeout (or a model error / abort): fall back to the fast path
        fast, strong = self.engine.fast_suggest(roman, context, k)
        if not strong:
            # The trie channels have nothing convincing (typically an English loanword or a
            # name): a wrong-looking flash is worse than a slightly later answer, so wait.
            try:
                return fut.result(timeout=WEAK_MATCH_DEADLINE_MS / 1000.0), False, False
            except Exception:
                pass
    finally:
        self.waiting.discard(key)
    return fast, True, strong
```
(`server.py:87-130`)

Step by step, with the rules a port must not reorder:

1. **`self.latest_key = key` happens first, before the cache check** (`server.py:99-100`). Even a
   pure cache hit re-stamps `latest_key`, which can make an unrelated in-flight job abortable.

2. **Cache hit → return immediately** with `partial=False, strong=False` and `move_to_end` for LRU
   recency (`server.py:100-103`). The deadline is not consulted at all. **Verified**: a cache hit
   with `deadline_ms: 0` still returned the full answer, `ms: 0.01`.

3. **Reuse an in-flight job for the same key** if one exists (`server.py:104`). Two concurrent
   requests for the same key share one future.

4. **Otherwise, cancel every other queued job first** (`server.py:106-110`). `Future.cancel()`
   returns `True` only for a job that has not started; a running job is untouched and stays in
   `pending`. Cancelled entries are popped from `pending` immediately. This is what stops the single
   worker from grinding through `a`, `am`, `ama` before reaching `amar`.

5. **Submit, register, attach the done callback** (`server.py:111-113`). The default-argument trick
   `lambda f, key=key:` binds the key at submit time.

6. **`self.waiting.add(key)` happens *after* the submit** (`server.py:114`). Between the submit and
   this line the abort predicate still returns `False`, because `latest_key` was set to this key in
   step 1 — unless another thread has raced ahead and changed `latest_key`.

7. **First wait: `fut.result(timeout=max(0.0, deadline_ms) / 1000.0)`** (`server.py:117`).
   - The floor at `0.0` means a negative `deadline_ms` behaves as `0`. **Verified**:
     `deadline_ms: -50` returned a `partial: true` answer in 4.6 ms.
   - `timeout=0.0` on a not-yet-finished future raises `TimeoutError` immediately (verified) and on
     an already-finished future returns the value.
   - On success: `return value, False, False` — `partial=False`, `strong=False`.
   - Any `Exception` falls through: `concurrent.futures.TimeoutError` (which is the builtin
     `TimeoutError`, an `OSError`), `concurrent.futures.CancelledError`, or any error raised by the
     model including `AbortedError`.
   - **Verified**, and worth stating because it is easy to get wrong in a port:
     `concurrent.futures.CancelledError` in CPython 3.12 derives from `Exception`
     (`CancelledError → Error → Exception → BaseException`, checked against
     `Lib/concurrent/futures/_base.py:45-51`), so a future cancelled out from under a waiting thread
     by another request's step 4 is caught here and degrades to the fast path. It does **not** escape.

8. **Fast path**: `fast, strong = self.engine.fast_suggest(roman, context, k)` (`server.py:120`).
   Raw `roman`, whole `context`, the coerced `k`. This call is **not** covered by any timeout — it is
   pure trie/rule work and is assumed to be a few milliseconds. Measured under contention with the
   model worker it reached ~29 ms (see §12).

9. **Second wait, only when `not strong`** (`server.py:121-127`): another
   `fut.result(timeout=WEAK_MATCH_DEADLINE_MS / 1000.0)`. Note **no `max(0.0, …)` floor here** — a
   negative `LIKHI_WEAK_WAIT_MS` would reach `Future.result` as a negative timeout.
   On success: `return value, False, False`. On any `Exception`: fall through.
   **Verified** with `LIKHI_WEAK_WAIT_MS=150`: a weak word answered in 129 ms with
   `partial: false, strong: false` — i.e. the second wait returned the **full** answer and the
   `partial` flag is `false` even though the fast path had already been computed and discarded.

10. **`finally: self.waiting.discard(key)`** (`server.py:128-129`) — runs before the final return, so
    by the time the fast answer is returned the job is abortable again if a newer key exists.

11. **`return fast, True, strong`** (`server.py:130`) — the only place `partial=True` is produced,
    and the only place `strong` can be `True`.

### 5.5 Job completion — `_done`

```python
def _done(self, key: tuple, fut: Future) -> None:
    self.pending.pop(key, None)
    if not fut.cancelled() and fut.exception() is None:  # aborted jobs raise, so are skipped
        self.full_cache[key] = fut.result()
        while len(self.full_cache) > self.cache_size:
            self.full_cache.popitem(last=False)
```
(`server.py:132-137`)

- Runs as a `Future` done-callback: on the worker thread for a normal completion, and on whichever
  thread observes the cancellation for a cancelled one.
- `pending.pop(key, None)` is unconditional — including for cancelled futures.
- Only successful results are cached. `AbortedError` and any model error leave the cache untouched.
- Eviction is **oldest-first** (`popitem(last=False)`), with `cache_size = 2048`.
- `self.full_cache[key] = …` on an existing key does **not** move it to the end, so an overwrite
  does not refresh its LRU position. Only a read via `suggest` does (`server.py:102`).

### 5.6 Which thread does what

| Thread | Work |
| --- | --- |
| socket connection thread (one per TCP connection, daemon) | parse, dispatch, both `fut.result` waits, `fast_suggest`, serialise, write |
| pipe connection thread (one per pipe client, daemon, `pipe.py:192`) | identical work via the same `handle_request` |
| pipe accept thread (one, daemon, `pipe.py:196`) | `CreateNamedPipeW` / `ConnectNamedPipe` loop |
| `likhi-model` worker (exactly one, `server.py:69`) | `_full` → `engine.suggest(..., abort=…)`; usually runs `_done` too |
| flusher (one, daemon, `server.py:288`) | `telemetry.flush()` on a timer |
| main thread | `serve_forever()` |

The single model worker is the whole point: it serialises all model work so the fast path never
queues behind it, and it is why step 4's cancellation exists.

---

## 6. `partial` and `strong` — exactly how they are computed

### 6.1 `partial`

`partial` is `True` **only** at `server.py:130`. Complete truth table:

| Path | `partial` | `strong` | Source line |
| --- | --- | --- | --- |
| `full_cache` hit | `false` | `false` | `server.py:103` |
| first wait returned the full result inside `deadline_ms` | `false` | `false` | `server.py:117` |
| second (weak) wait returned the full result inside `WEAK_MATCH_DEADLINE_MS` | `false` | `false` | `server.py:125` |
| everything else — the fast answer | `true` | `fast_suggest`'s flag | `server.py:130` |

Consequences to preserve:

- `strong` is **always `false` whenever `partial` is `false`**, even when the fast path did run and
  did report strong evidence. The flag describes the answer being returned, not the input.
- A cache hit is indistinguishable from a fresh full answer.
- An empty `roman` still produces `partial: true` if the wait expired: verified,
  `{"roman":""}` → `{"candidates": [], "partial": true, "strong": false, "ms": 12.58}` — the request
  burned its whole 12 ms deadline waiting for a job that had been queued behind another.

### 6.2 `strong`

`strong` is whatever `LikhiEngine.fast_suggest` returns as its second value
(`server.py:120`, consumed at `server.py:130`). Its definition, reproduced from
`core.py:542-579`:

```python
def fast_suggest(self, roman, context=(), k=5) -> tuple[list[str], bool]:
    r = normalize_roman(roman)
    prev = (canonical(context[-1]),) if context else ()
    feats = self.candidates(r, use_model=False)
    if not feats:
        return [], False
    w = self.w["rom_exact_fast"]

    def base(word: str, ft: Feats) -> float:
        return self.score(word, ft, r, prev) + w * math.log1p(ft.rom_exact)

    demote = max(ft.rom_exact for ft in feats.values()) >= FAST_TRUST_ROM_EXACT
    ranked = sorted(
        feats.items(),
        key=lambda kv: (
            (0 if not demote or _fast_supported(kv[1]) else 1),
            -base(kv[0], kv[1]),
        ),
    )
    strong = any(ft.rom_exact >= 2 for ft in feats.values())
    return [to_output(word) for word, _ in ranked[:k]], strong
```

The one line that defines `strong`:

```python
strong = any(ft.rom_exact >= 2 for ft in feats.values())      # core.py:578
```

In words: **some candidate has an attested romanization count of at least 2 for exactly this typed
string.** Not "the top candidate" — *any* candidate in the feature set, including ones that never
reach the visible list. `rom_exact` is the summed attestation count for the exact roman key
(`core.py:322-326`), so `>= 2` means "seen more than once in the aligned training data".

Supporting constants that a port of `fast_suggest` must carry (`core.py:83`, `core.py:118-133`,
`core.py:63` weights):

```python
FAST_TRUST_ROM_EXACT = 5      # core.py:83 — demotion only kicks in at this attestation level

def _fast_supported(ft: "Feats") -> bool:                      # core.py:118-133
    return bool(
        ft.key_fine
        or ft.key_coarse
        or ft.key_fine_prefix
        or ft.key_coarse_prefix
        or ft.avro
        or ft.rom_prefix
        or ft.rom_exact >= 2
    )
```

`"rom_exact_fast": 2.5` is the extra weight applied as `w * math.log1p(ft.rom_exact)`
(`core.py:57`, applied at `core.py:568`). The sort key is a **tuple** — the demotion bucket first
(`0` or `1`), then `-base(...)` — so demoted candidates are pushed behind, never removed.

### 6.3 Why the client cares — do not "improve" this

`shell/src/service.rs:94-96`:

```rust
fn worth_refining(&self) -> bool {
    self.partial && !self.strong
}
```

That single expression decides whether the shell re-asks with the 400 ms commit deadline
(`service.rs:398-402`, `:453-457`) and whether the idle-pause refiner fires (`service.rs:671-678`).
The docstring at `server.py:90-97` records the measurement behind it:

> The fast path is *strong* when some candidate is an attested spelling of exactly what was typed,
> seen more than once, and measured over Dakshina, the chat set and the feedback words that beats
> the full ranking on those words: 90.2 against 85.7 top-1 on chat. Replacing a strong fast answer
> with the model's is how "rapid" showed র‍্যাপিড and then changed its mind to রাপিড.

and `service.rs:91-93`:

> refining everything scores 73.9 top-1, refining nothing 58.4, refining only the unconfident 75.7 —
> and on chat words alone, refining everything is a regression, 79.5 against 81.7.

If a port makes `strong` more (or less) generous, the shell silently changes how often it re-asks,
and the failure shows up as bad suggestions with no error anywhere. This is the "bad word
suggestions nobody can trace" risk in its purest form.

### 6.4 The tuning note reproduced verbatim

`server.py:43-54` — the measurement that justifies `WEAK_MATCH_DEADLINE_MS = 0`:

```
# Extra wait when the model-free answer has no strong evidence, measured 2026-09-16 over 140
# keystrokes of realistic typing:
#
#   wait   full answers   p50    p90    p99
#      0          3.6%   16ms   28ms   31ms
#     10          5.0%   27ms   31ms   42ms
#     25          7.9%   27ms   48ms   59ms
#     60         40.0%   27ms   75ms   89ms
#
# Waiting mid-word buys almost nothing, because the model is answering about a prefix the user
# will never commit, and each new keystroke supersedes it. Correctness comes from the commit
# re-ask (long deadline, by then the model has finished), not from blocking the typist. Default 0.
```

---

## 7. Telemetry and the background flusher

### 7.1 The flusher thread

```python
interval = float(cfg["sync_seconds"])                  # server.py:275

def _flusher() -> None:
    if not stop.wait(min(60.0, interval)):
        telemetry.flush()
    while not stop.wait(interval):
        telemetry.flush()

threading.Thread(target=_flusher, daemon=True).start()  # server.py:277-288
```

Exact timing rules:

- **First round after `min(60.0, interval)` seconds**, not after a full interval. The rationale is
  at `server.py:278-282`: a machine switched off before the first tick would never report at all,
  which is the normal life of an office PC.
- `Event.wait(t)` returns `True` if the event was set, `False` on timeout. So `if not stop.wait(...)`
  means "the wait timed out, nobody asked us to stop → flush".
- If `stop` is set during the early wait, `_flusher` **returns without entering the loop** — no
  steady-state flush at all. The shutdown `telemetry.flush()` at `server.py:315` covers it.
- Steady state: flush every `interval` seconds, forever, until `stop` is set.
- The thread is started **regardless of telemetry mode**, including `off`. `Telemetry.flush()`
  returns immediately when the mode is `off` (`telemetry.py:181-183`), so the loop just spins on a
  timer doing nothing.
- `interval` can never be `0` from config, because `_telemetry_config` uses `or 900`
  (`server.py:381, 402`) and `0` is falsy. It *can* be a small number if explicitly set to e.g.
  `5` — **verified**: with `LIKHI_TELEMETRY_SYNC_S=5` a `metrics.jsonl` row appeared about 5 s in.
- Sync-interval rationale, `server.py:272-274`: "Each round is at most one upload per stream, so
  this sets the request rate: 15 minutes keeps a pilot inside Cloud Storage's 5,000 free writes a
  month, and nothing is lost in between because the local files are the source of truth."

### 7.2 What `flush()` does (`telemetry.py:181-190`)

```python
def flush(self) -> None:
    if self.mode == "off":
        return
    try:
        with self.lock:
            self._flush_locked()
    except Exception:
        pass
    if self.drop or self.endpoint:
        self.sync()
```

`_flush_locked` writes one aggregated row to `metrics.jsonl` and resets the counters, latencies and
hour bucket (`telemetry.py:168-179`). `sync()` ships only the bytes written since the last
successful sync, in `MAX_CHUNK_BYTES = 256 * 1024` chunks, never splitting a line
(`telemetry.py:228-307`).

### 7.3 What a `learn` op records (`telemetry.py:99-150`)

Relevant because the `learn` op's extra fields exist only for this:

- `secure=True` → the whole record is skipped, **including the counters**
  (`telemetry.py:112-113`). **Verified**: a `secure` learn did not increment `words`.
- Counters incremented: `words`, `pos_{min(index, 9)}`, `top1_taken` when `index == 0`,
  `retyped` when retyped, `app_{re.sub(r'[^a-zA-Z0-9._-]', '', app)[:24]}` when `app` is non-empty.
- `latency_ms` is appended to a list capped at 20000 entries, summarised as `lat_p50`, `lat_p95`,
  `lat_n` at flush time.
- A struggle event is written to `events.jsonl` only when
  `mode == "full" and (index > 0 or retyped) and is_recordable(roman, chosen) and (not top1 or is_recordable(roman, top1))`
  (`telemetry.py:128-133`).
- `is_recordable` (`telemetry.py:52-58`): both strings non-empty, both ≤ `MAX_LEN = 32`, and neither
  matching `[\d@:/\\]`. **Verified**: `roman: "pass123"` produced no event row but still counted.

Verified live output:

```json
{"h": "2026-09-18T05", "roman": "bondhu", "chose": "বন্ধুর", "we_said": "",       "pos": 2, "retyped": false}
{"h": "2026-09-18T05", "roman": "bondhu", "chose": "বন্ধুর", "we_said": "বন্ধু", "pos": 3, "retyped": false}
{"h": "2026-09-18T05", "id": "d25f6266c9854410", "words": 5, "pos_2": 1, "app_notepad.exe": 3, "pos_3": 1, "pos_0": 1, "top1_taken": 1, "pos_1": 2, "lat_p50": 42.5, "lat_p95": 42.5, "lat_n": 1}
```

---

## 8. Configuration resolution (`server.py:318-406`)

### 8.1 Search order

```python
def _config_paths() -> list[Path]:
    paths: list[Path] = []
    explicit = os.environ.get("LIKHI_CONFIG")
    if explicit:
        paths.append(Path(explicit))
    here = Path(__file__).resolve()
    for up in (5, 3):  # packaged runtime, then a plain checkout
        if len(here.parents) > up:
            paths.append(
                here.parents[up] / "pime" / "python" / "input_methods" / "likhi" / "config.json"
            )
    paths.append(
        Path(os.environ.get("PROGRAMFILES(X86)", r"C:\Program Files (x86)"))
        / "PIME" / "python" / "input_methods" / "likhi" / "config.json"
    )
    paths.append(_user_config_path())
    return paths
```
(`server.py:318-345`)

`_user_config_path()` is
`Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Likhi" / "config.json"`
(`server.py:348-354`).

**Verified resolution on this checkout** (`server.py` at
`C:\Users\khaled\src\likhi\src\likhi\server.py`, `len(parents) == 7`):

```
-     C:\Users\pime\python\input_methods\likhi\config.json          (parents[5] = C:\Users)
-     C:\Users\khaled\src\pime\python\input_methods\likhi\config.json (parents[3] = C:\Users\khaled\src)
-     C:\Program Files (x86)\PIME\python\input_methods\likhi\config.json
EXIST C:\Users\khaled\AppData\Local\Likhi\config.json
```

The `up = 5` entry is aimed at the packaged layout
`<app>\runtime\python\Lib\site-packages\likhi\server.py`, where `parents[5]` is `<app>`
(**verified**: `C:\Program Files\Likhi\runtime\python\Lib\site-packages\likhi\server.py` exists).
The `up = 3` entry, commented "a plain checkout", resolves to a directory **outside** the repository
in this layout and matches nothing.

### 8.2 `_telemetry_config` (`server.py:368-406`)

Precedence, first match wins:

1. **Environment.** If `LIKHI_TELEMETRY` is set and non-empty, return immediately — the config files
   are not read at all, and the per-user override is **not** merged (`server.py:374-382`):
   ```python
   return {
       "mode": env.strip().lower(),
       "drop": os.environ.get("LIKHI_TELEMETRY_DROP") or None,
       "endpoint": os.environ.get("LIKHI_TELEMETRY_ENDPOINT") or None,
       "key": os.environ.get("LIKHI_TELEMETRY_KEY") or None,
       "sync_seconds": float(os.environ.get("LIKHI_TELEMETRY_SYNC_S") or 900),
   }
   ```
2. **First existing file** from `_config_paths()` (`server.py:383-403`):
   - read with `encoding="utf-8-sig"` — a Notepad byte-order mark must not disable telemetry
     (`server.py:386-387`);
   - if the per-user file exists **and this is not the per-user file**, merge it over the machine
     config with `cfg = {**cfg, **user}` (`server.py:394-396`). Only the keys the user actually set
     are overridden, so turning reporting off in the Likhi window does not erase the endpoint and
     key that the installer stamped in;
   - map to the result:
     ```python
     return {
         "mode": str(cfg.get("telemetry", "off")).lower(),
         "drop": cfg.get("telemetry_drop") or None,
         "endpoint": cfg.get("telemetry_endpoint") or None,
         "key": cfg.get("telemetry_key") or None,
         "sync_seconds": float(cfg.get("telemetry_sync_seconds") or 900),
     }
     ```
   - any exception while reading a path is swallowed and the loop moves on (`server.py:404-405`).
3. **Fallback**: `{"mode": "off", "drop": None, "endpoint": None, "key": None, "sync_seconds": 900.0}`
   (`server.py:406`).

Asymmetry to preserve: the env branch does `env.strip().lower()`; the file branch does
`str(cfg.get("telemetry", "off")).lower()` with **no `.strip()`**. A config value of `" full "`
therefore stays `" full "`, and `Telemetry.__init__` clamps any unrecognised mode to `"off"`
(`telemetry.py:74`) — while the `[likhi-server] telemetry: …` line prints the *unclamped*
`cfg['mode']` (`server.py:258`). A file with a stray space silently reports "telemetry:  full " and
records nothing.

`or None` / `or 900` mean empty strings and `0` are treated as unset.

Pinned by `tests/test_config_discovery.py` and `tests/test_user_config_override.py`.

---

## 9. What the Rust shell sends and reads — the must-preserve list

From `shell/src/engine.rs`. **Every item here is a hard constraint on the port.**

### 9.1 Fields the shell SENDS

`suggest` (`engine.rs:369-376`):

```rust
let request = serde_json::json!({
    "op": "suggest",
    "roman": roman,
    "context": context,      // &[String] -> JSON array of strings
    "k": k,                  // usize
    "deadline_ms": deadline_ms,  // u32
})
.to_string();
```

`learn` (`engine.rs:406-416`):

```rust
let request = serde_json::json!({
    "op": "learn",
    "roman": commit.roman,
    "chosen": commit.chosen,
    "top1": commit.top1,
    "index": commit.index,      // usize
    "retyped": commit.retyped,  // bool
    "app": commit.app,
    "context": commit.context,  // &[String]
})
.to_string();
```

The shell **never** sends `secure` or `latency_ms`, and never sends `ping`. Both must keep their
server-side defaults (`False` and `None`). `k` and `index` arrive as JSON integers, `deadline_ms` as
an integer, `retyped` as a JSON boolean.

### 9.2 Fields the shell READS

```rust
#[derive(Deserialize)]
struct Reply {
    ok: bool,
    #[serde(default)] candidates: Vec<String>,
    #[serde(default)] partial: bool,
    #[serde(default)] strong: bool,
    #[serde(default)] error: Option<String>,
}
```
(`engine.rs:83-94`)

- **`ok` has no `#[serde(default)]`.** A reply missing `ok`, or with `ok` as anything but a JSON
  boolean, fails to deserialise and the shell logs "engine sent something unreadable" and returns
  `None` — i.e. no suggestions at all. **Every reply on every path must carry a boolean `ok`.**
- `candidates` must be a JSON array of strings. `null` would fail (`#[serde(default)]` covers a
  *missing* field, not an explicit `null`). Today it is always present on a `suggest` reply.
- `partial` and `strong` must be JSON booleans when present.
- `error` may be any string or absent.
- There is no `deny_unknown_fields`, so `ms` and `version` are ignored harmlessly. Extra fields may
  be added by a port; existing ones may not be renamed or retyped.
- The shell only uses the reply when `ok` is `true` (`engine.rs:387`); otherwise it logs `error` and
  returns `None` (`engine.rs:392-395`).
- For `learn` the reply is read purely "to keep the stream in step" (`engine.rs:404-405`) and its
  contents are discarded — but **a reply must still be sent**, exactly one line, or the next
  request's read will pick up the wrong line.

### 9.3 Framing and timing constraints

- **One reply line per request line, in order, on the same connection.** The shell writes
  `request` then `b"\n"` and reads one line (`engine.rs:264-285`). It has no request ids and no way
  to resynchronise; a missing or extra line desynchronises the connection permanently.
- The reply line must end with `\n`. The pipe reader scans for `b'\n'` and strips it
  (`engine.rs:218-222`); the socket reader uses `BufRead::read_line` (`engine.rs:276`).
- **Read timeout**: `read_timeout_for(deadline_ms) = deadline_ms * 2 + 80` milliseconds
  (`engine.rs:52-54`). Budgets the server must fit inside:
  - `TYPE_DEADLINE_MS = 12` → **104 ms**
  - `COMMIT_DEADLINE_MS = 400` → **880 ms**

  The server's own deadline bounds only the model wait. The real worst case is
  `deadline_ms + fast_suggest_time + WEAK_MATCH_DEADLINE_MS`. With the default
  `WEAK_MATCH_DEADLINE_MS = 0` there is plenty of head-room, but **setting `LIKHI_WEAK_WAIT_MS`
  above about 60 ms makes a 12 ms typing request exceed the client's 104 ms budget**, at which point
  the shell cancels the I/O, drops the connection and shows nothing. A port must keep the same
  relationship or raise the client budget in lock-step.
- On a transport error the shell drops the connection and retries **once** (`engine.rs:344-360`);
  after two failures it backs off for `RETRY_AFTER = 2 s` (`engine.rs:47, 336`).
- The shell prefers the pipe and falls back to the socket (`engine.rs:315-339`), with
  `CONNECT_TIMEOUT_MS = 120`.
- `ERROR_PIPE_BUSY` must remain a *recoverable* condition: the client waits
  `WaitNamedPipeW(name, 120)` and retries once (`engine.rs:141-148`).
- Pipe name `\\.\pipe\likhi-engine-s{session}` must match byte for byte (`engine.rs:103`,
  `pipe.py:86`).
- Default port 47123 (`Engine::new(port)`; the shell's config default is `server_port: 47123`).

### 9.4 The PIME Python client (secondary, still live)

`windows/pime/likhi/likhi_ime.py` sends `suggest` with `op`, `roman`, `k`, `deadline_ms` and
**no `context`** (`likhi_ime.py:338-372`), and `learn` with `op`, `roman`, `chosen`, `index`,
`top1`, `retyped`, and `app` only when telemetry is on (`likhi_ime.py:390-400`). It reads
`resp.get("ok")` and `resp.get("candidates", [])`. So: a `suggest` with no `context` key at all must
keep working, and `learn` with a missing `app` must keep working.

---

## 10. Behaviour matrix — verified request/response pairs

Captured from a live engine. Use these as the port's acceptance tests.

| Request | Response |
| --- | --- |
| `{"op":"ping"}` | `{"ok": true, "version": "0.0.1"}` |
| `{"op":"suggest","roman":"amar","context":[],"k":5,"deadline_ms":12}` (cold) | `{"ok": true, "candidates": ["আমার","আমি","আমরা","ভাই","ভাইয়া"], "partial": true, "strong": true, "ms": 29.68}` |
| `{"op":"suggest","roman":"bhalobasha","context":[],"k":5,"deadline_ms":400}` | `{"ok": true, "candidates": ["ভালবাসা","ভালোবাসা","ভালোবাসায়","ভালোবাশা","ভালোবাসে"], "partial": false, "strong": false, "ms": 322.81}` |
| `{"op":"suggest","roman":"rapid",…,"deadline_ms":12}` | `{"ok": true, "candidates": ["র‍্যাপিড","রিপড","রাপিড","রাপিডও","রৌপ্যপদক"], "partial": true, "strong": false, "ms": 21.22}` |
| `{"op":"suggest","roman":"","context":[],"k":5,"deadline_ms":12}` | `{"ok": true, "candidates": [], "partial": true, "strong": false, "ms": 12.58}` |
| `{"op":"suggest","roman":"amar","context":"abc",…}` | answered normally — the string is iterated per character |
| `{"op":"suggest","roman":"amar","context":null,…}` | `{"ok": false, "error": "TypeError: 'NoneType' object is not iterable"}` |
| `{"op":"suggest","roman":"amar","k":0,"deadline_ms":400}` | `{"ok": true, "candidates": [], "partial": false, "strong": false, "ms": 241.82}` |
| `{"op":"suggest","roman":"amar","k":-1,"deadline_ms":400}` | 82 candidates (everything but the last) |
| `{"op":"suggest","roman":"amar","k":"3",…}` | 3 candidates |
| `{"op":"suggest","roman":"amar","k":"x",…}` | `{"ok": false, "error": "ValueError: invalid literal for int() with base 10: 'x'"}` |
| `{"op":"suggest","roman":"amar","k":5,"deadline_ms":-50}` | `{"ok": true, …, "partial": true, "strong": true, "ms": 4.44}` |
| `{"op":"suggest","roman":123,…}` | `{"ok": false, "error": "AttributeError: 'int' object has no attribute 'casefold'"}` |
| `{}` | `{"ok": true, "candidates": [], "partial": true, "strong": false, "ms": 15.11}` |
| `{"op":"frobnicate"}` | `{"ok": false, "error": "unknown op 'frobnicate'"}` |
| `[1,2]` | `{"ok": false, "error": "AttributeError: 'list' object has no attribute 'get'"}` |
| `not json` | `{"ok": false, "error": "JSONDecodeError: Expecting value: line 1 column 1 (char 0)"}` |
| `` (blank, socket) | `{"ok": false, "error": "JSONDecodeError: Expecting value: line 2 column 1 (char 1)"}` |
| `` (blank, pipe) | **no reply at all** |
| `{"op":"learn","roman":"amr","chosen":"আমার","index":0,"top1":"x","retyped":false,"app":"notepad.exe","context":[]}` | `{"ok": true}` |
| `{"op":"learn"}` | `{"ok": true}` |
| cache hit for any warmed key, even `deadline_ms: 0` | `partial: false, strong: false, ms ≈ 0.01` |

---

## 11. Gotchas a naive port will get wrong

Ranked by how badly a mistake corrupts suggestions.

1. **`strong` is `any(rom_exact >= 2)` over *all* candidates, not the top one** (`core.py:578`).
   Narrowing it to the returned list changes `worth_refining()` in the shell and silently changes
   which words get re-asked at commit. This is the "bad suggestions nobody can trace" failure.

2. **`strong` must be forced to `false` on every non-partial path** (`server.py:103,117,125`), even
   when the fast path ran and said otherwise. The flag describes the answer, not the input.

3. **Only the last context word is in the cache key** — `context[-1:]` (`server.py:77`). Verified:
   `["tumi","ami"]` hits an entry cached for `["ami"]`. Keying on the whole context makes the cache
   miss constantly and typing gets slower; keying on nothing makes context-sensitive answers leak
   across contexts.

4. **The cache key uses the raw `roman` and raw last context word; the engine normalises them.**
   `"Amar"` and `"amar"` are two keys, one result (`server.py:77` vs `textnorm.py:96-101` and
   `core.py:593`). Normalising the key "for cleanliness" changes the hit rate and, with different
   `k`, the answers returned.

5. **`svc.learn` clears the entire `full_cache`, every entry** (`server.py:142`) — not just the
   romans that changed, unlike the engine's own cache (`core.py:253-254`). Verified.

6. **`top_candidate` on the `learn` path always returns `""`** because the clear happens first
   (`server.py:180` then `:186`). Verified: `we_said: ""`. Do not "fix" the ordering without
   deciding, explicitly, to change what the telemetry means.

7. **`latest_key` is set before the cache check** (`server.py:99-100`), so even a cache hit makes
   older in-flight jobs abortable.

8. **`max(0.0, deadline_ms)` on the first wait, but no floor on the second**
   (`server.py:117` vs `:125`).

9. **`abort` is checked at exactly two points** (`core.py:391, 433`). It is not preemption. A port
   that adds checks changes how much work a stale job does; one that drops them makes the single
   worker fall behind the typist.

10. **The abort predicate needs both halves**: `latest_key != key AND key not in waiting`
    (`server.py:84`). Dropping the `waiting` half makes commit-time re-asks abort themselves.

11. **`waiting` is a set, not a counter** (`server.py:74, 114, 129`). Two threads waiting on one key,
    one returning, removes the key while the other is still blocked — and the job may then abort
    under it. Preserve the set semantics or accept a behaviour change.

12. **Negative `k` is a Python negative slice** (`core.py:536`): `k = -1` returns everything but the
    last candidate, verified at 82 entries. Rust's `&v[..k]` would panic. Clamping to `0` is a
    deviation; decide deliberately.

13. **A `context` that is a JSON string is iterated per character** (`server.py:165`,
    `tuple("abc") == ("a","b","c")`). Verified: answered normally, no error.

14. **Blank lines differ per transport** (§3.3): socket replies with a JSON error, pipe stays silent.

15. **Socket lines arrive with their `\n` attached**, pipe lines do not (`server.py:204` vs
    `pipe.py:157`). Visible in the `JSONDecodeError` column/char numbers.

16. **`ms` is always a JSON float**, including `0.0` (`server.py:174`). An integer would be a
    (harmless) wire change.

17. **`ensure_ascii=False`**: Bangla goes out as raw UTF-8 (`server.py:197`).

18. **All shared state is lock-free under the GIL.** `pending`, `full_cache`, `waiting` and
    `latest_key` (`server.py:70-74`) have no lock. A Rust port needs explicit synchronisation but
    **must not hold any lock across the model call** — `server.py:63-67` records that doing so was
    measured at 120 ms round trips instead of 12.

19. **`_done` pops `pending[key]` unconditionally** (`server.py:133`), including for cancelled
    futures. A late `_done` from a cancelled job can evict a *newer* future submitted under the same
    key in the meantime, causing a duplicate submit later. Harmless, but a port that "tightens" this
    changes scheduling.

20. **`full_cache[key] = …` does not refresh LRU order** (`server.py:135`); only a read does
    (`server.py:102`).

21. **`already_running` searches for the literal bytes `"ok"`**, it does not parse JSON
    (`server.py:225`).

22. **`allow_reuse_address` must stay `False` on Windows** (`server.py:214`). Setting `SO_REUSEADDR`
    there lets two engines split the keyboard between two personal dictionaries.

23. **`limit_blas_threads(4)` uses `setdefault` and must run before NumPy is imported**
    (`server.py:238-239`, `threads.py:16-18`).

24. **Cancelling queued jobs happens only when a *new* key is submitted** (`server.py:106-110`), not
    on every request. A request that joins an existing future cancels nothing.

25. **`Telemetry.__init__` writes `install_id` even when the mode is `off`**
    (`telemetry.py:85-95`). Verified.

26. **The flusher's first round is `min(60.0, interval)`**, and if `stop` fires during it the loop
    is never entered (`server.py:283-286`).

27. **The engine's own 4096-entry cache is bypassed for every server request**, because `abort` is
    always non-`None` (`core.py:594-596`). Every fast-path call re-does the full trie work
    (`fast_suggest` → `candidates(use_model=False)`, `core.py:562`), which is why `fast_suggest` was
    measured at ~29 ms under contention rather than the "few milliseconds" the docstring promises.

---

## 12. Latency reference (measured on this machine, loopback socket)

Round-trip times from the verification runs, for a port to compare against:

| Request | round trip | server `ms` |
| --- | --- | --- |
| `ping` | 0.4 ms | — |
| `suggest amar`, d=12, cold | 29.9 ms | 29.68 |
| `suggest amar`, d=12, job still running | 41.4 ms | 41.27 |
| `suggest bhalobasha`, d=12 | 16.7 ms | 16.50 |
| `suggest bhalobasha`, d=400 | 323.0 ms | 322.81 |
| `suggest porikkha`, d=12, first | 23.4 ms | 23.18 |
| `suggest porikkha`, d=12, cache hit | 0.2 ms | 0.01 |
| `suggest`, d=0, fresh word | 7.0 ms | 3.17 |
| weak word, d=12, `LIKHI_WEAK_WAIT_MS=150` | 129.0 ms | 128.71 |
| `learn` | 1.4–11.9 ms | — |
| engine start-up (load + warm-up) | 584–1307 ms | — |

Note the second row: a repeat request while the model job is still running is **not** a cache hit and
costs a full deadline plus the fast path. Only a *completed* job populates `full_cache`.
Note also that `suggest amar` at a 12 ms deadline took 29.7 ms wall — the deadline bounds the wait,
not the response.

---

## 13. Uncertain

Flagged rather than guessed.

1. **Whether the pipe's single-instance hole matters in practice.** Verified that a second same-user
   process *can* create another instance of `\\.\pipe\likhi-engine-s1` while the installed engine
   holds it. Not verified: which instance a client actually lands on, or whether a second engine's
   accept loop ever wins a connection in a real session. The mechanism is confirmed; the frequency
   is not.

2. **Whether a *different* user's process can create an instance of the same pipe.** The SDDL grants
   `GA` to `SY`, `BA` and `OW` and `GRGW` to the two package SIDs (`pipe.py:58-63`). `OW` resolves to
   the existing object's owner, so a second user should lack `FILE_CREATE_PIPE_INSTANCE` — but the
   pipe name already carries the session id (`pipe.py:86`), so the case may not arise. Untested.

3. **Config discovery looks stale relative to the shipping installer.** The installed app on this
   machine keeps its config at `C:\Program Files\Likhi\config.json` (verified to exist, with
   `"telemetry": "full"` and a live endpoint), and the Rust shell reads exactly that path
   (`shell/src/config.rs:86-88`). `server.py::_config_paths()` **never looks there**. The launcher
   sets `LIKHI_CONFIG=%ProgramFiles(x86)%\PIME\python\input_methods\likhi\config.json`
   (`scripts/build_runtime.py:173`), which does **not exist** on this machine (verified `False`), so
   the running engine falls through to `%LOCALAPPDATA%\Likhi\config.json`, which contains only
   `{"font_name": "Noto Sans Bengali"}` → telemetry `off`, sync 900 s. The shell therefore believes
   telemetry is `full` while the engine records nothing. I do not know whether this is a known,
   accepted state or a live bug; a port should reproduce the documented algorithm and raise the
   discrepancy separately rather than quietly adding the `%ProgramFiles%\Likhi\config.json` path.

4. **The `up = 3` config path.** Commented "a plain checkout" (`server.py:331`) but resolving to
   `C:\Users\khaled\src\pime\python\input_methods\likhi\config.json` — outside the repository, and
   not where this repo keeps its config (`windows\pime\likhi\config.json`). I could not find a
   layout in which it matches. It may be vestigial.

5. **`concurrent.futures.CancelledError` across Python versions.** Verified as an `Exception`
   subclass on **3.12.10** (this venv), so `except Exception` catches it and a cancelled wait
   degrades to the fast path. The PIME text service runs its own Python 3.8 (`server.py:3-5`), and I
   did not check 3.8's class hierarchy. If it derived from `BaseException` there, the escape would
   reach `_Handler` and kill a connection. The engine process itself uses the bundled 3.12 runtime,
   so this is a note about reading older tracebacks, not about the port.

6. **Exact `AbortedError` frequency in production.** The mechanism is clear; I did not instrument how
   often jobs actually abort during real typing, so I cannot say how much of the cache stays cold.

7. **What happens if a pipe client sends a very large line with no newline.** `pending` in
   `_serve_client` (`pipe.py:150-158`) grows without bound. No limit is enforced anywhere. Untested;
   a port should probably impose a cap, which would be a deliberate deviation.

8. **`fast_suggest` under GIL contention.** Measured at ~29 ms in one cold run against a documented
   expectation of "a few milliseconds" (`server.py:8`). Whether that is GIL contention with the
   model worker, cold trie pages, or both, I did not isolate. A Rust port without a GIL should be
   faster, which would change the `partial` rate — probably for the better, but it *will* change the
   measured numbers in §6.4 and §12.
