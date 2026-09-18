# Porting specification: `likhi.telemetry`

Source of truth: `src/likhi/telemetry.py` (308 lines, read in full at commit state of 2026-09-18).
Secondary sources cited where they define behaviour the module only implies:
`src/likhi/server.py` (the only production caller and the owner of the sync thread),
`src/likhi/report.py` (`likhi-report`), `server/ingest_server.py` (the production collector),
`windows/pime/likhi/likhi_ime.py` (the client that supplies `app` / `index` / `retyped`),
`tests/test_telemetry.py`, `tests/test_telemetry_sync_state.py`, `docs/PILOT.md`.

**The hard requirement.** The collector in production (`server/ingest_server.py`, and
`likhi-report collect`) already parses these files. A Rust port must produce byte-compatible
output. Every byte-level rule is in §6 and §7 — **§6.1 (CRLF) and §6.3 (key insertion order) are
the two that a naive port gets wrong and that no test in this repo would catch.**

Everything in this document was verified by running the real module under
`C:\Users\khaled\src\likhi\.venv\Scripts\python.exe` (CPython 3.12.10, Windows) unless it appears
under §15 "Uncertain".

---

## 1. Module constants

Reproduce these exactly. `telemetry.py:36-45`:

```python
MAX_LEN = 32
_RE_UNSAFE = re.compile(r"[\d@:/\\]")
# Write a counter row every N committed words. Machines get shut down or killed without a clean
# exit, and TerminateProcess cannot be caught, so counters must not sit in memory for an hour.
# Rows carry their hour bucket, so several rows per hour simply sum at collection time.
FLUSH_EVERY = 20
MAX_CHUNK_BYTES = 256 * 1024  # cap one upload, so a machine offline for a week catches up in steps
HTTP_TIMEOUT_S = 10.0

DEFAULT_DIR = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Likhi"
```

| Constant | Value | Notes |
|---|---|---|
| `MAX_LEN` | `32` | Max length, in **Unicode code points** (Python `len(str)`), of `roman` and of the word. Not bytes, not grapheme clusters. |
| `_RE_UNSAFE` | `[\d@:/\\]` | Character class: any Unicode digit, `@`, `:`, `/`, `\`. |
| `FLUSH_EVERY` | `20` | Committed words per metrics row. |
| `MAX_CHUNK_BYTES` | `262144` | `256 * 1024`. Cap on one chunk/HTTP body. |
| `HTTP_TIMEOUT_S` | `10.0` | Passed to `urlopen(..., timeout=)`; covers connect and each read. |
| `DEFAULT_DIR` | `%LOCALAPPDATA%\Likhi` | Falls back to the home directory when `LOCALAPPDATA` is unset. |

`DEFAULT_DIR` is evaluated **once at module import**, not per instance. Verified:
`DEFAULT_DIR == WindowsPath('C:/Users/khaled/AppData/Local/Likhi')`. A Rust port that reads the
environment lazily per call will diverge if anything mutates `LOCALAPPDATA` mid-process. Bind it
once at initialisation.

Uncited but load-bearing: the collector's own caps (`server/ingest_server.py:54-58`) are
`MAX_BODY = 1024 * 1024` and `MAX_EXPORT = 64 * 1024 * 1024`, with
`RE_INSTALL = ^[0-9a-f]{8,32}$`, `RE_STREAM = ^(metrics|events)$`, `RE_SEQ = ^[0-9]{1,9}$`.
`MAX_CHUNK_BYTES` (256 KiB) sits comfortably under `MAX_BODY` (1 MiB); do not raise it without
raising the server's.

---

## 2. Modes

`telemetry.py:74`:

```python
self.mode = mode if mode in ("off", "metrics", "full") else "off"
```

Three modes, and **anything not exactly one of the three strings becomes `"off"`** — this is a
whitelist, not a parse. Case-sensitive at this layer; the caller lowercases
(`server.py:398`: `str(cfg.get("telemetry", "off")).lower()`).

| Mode | Counters (`metrics.jsonl`) | Struggle events (`events.jsonl`) |
|---|---|---|
| `off` | no | no |
| `metrics` | yes | no |
| `full` | yes | yes |

---

## 3. Identity: the install id

`telemetry.py:85-95`:

```python
def _install_id(self) -> str:
    path = self.dir / "install_id"
    try:
        if path.exists():
            return path.read_text(encoding="utf-8").strip()
        self.dir.mkdir(parents=True, exist_ok=True)
        new = uuid.uuid4().hex[:16]
        path.write_text(new, encoding="utf-8")
        return new
    except Exception:
        return "unknown"
```

Rules:

1. File name is `install_id`, **no extension**, directly in `self.dir`.
2. Content on creation: `uuid.uuid4().hex[:16]` — exactly 16 lowercase hex characters
   (`0-9a-f`), **no trailing newline**. Verified: `b'ed31e9fe3dae4e90'`, 16 bytes.
   Use UUIDv4 (122 random bits) and take the first 16 hex chars of the lowercase hex form.
3. Read path applies `.strip()` (Python `str.strip()` — strips Unicode whitespace both ends),
   so a hand-edited file with a trailing newline still works.
4. Written as UTF-8 via `Path.write_text`, which uses text mode. The content contains no `\n`, so
   no newline translation applies here (contrast §6.1).
5. On **any** exception the id is the literal string `"unknown"`.
6. It is read/created in `__init__` (`telemetry.py:83`) **before** any mode check.

### 3.1 `"unknown"` is rejected by the collector (confirmed defect to preserve or fix deliberately)

`"unknown"` does not match the server's `RE_INSTALL = ^[0-9a-f]{8,32}$`
(`server/ingest_server.py:56`). Verified: `re.match(r"^[0-9a-f]{8,32}$", "unknown")` is `None`.
So a machine that cannot write its state directory uploads chunks that the ingest endpoint answers
with `400 {"error":"bad headers"}` forever, and — because a failed send never advances the offset
(§8.5) — retries the same bytes on every sync round indefinitely. The folder drop accepts it
(it only builds a path), so drop-folder pilots silently accumulate an `unknown/` directory.
Preserve this literal if you want byte compatibility; flag it if you are allowed to fix it.

### 3.2 `off` mode still creates the directory and the id

`__init__` calls `self._install_id()` unconditionally (`telemetry.py:83`), so constructing a
`Telemetry("off", ...)` creates `self.dir` and writes `install_id` into it. Verified: after
`Telemetry("off", directory=local)`, `os.listdir(local) == ['install_id']`.
`tests/test_telemetry.py:15-20` only asserts that `metrics.jsonl` and `events.jsonl` are absent,
so this side effect is untested and easy to drop in a port. Keep it: the id must be stable across
a machine that is later switched from `off` to `metrics`.

---

## 4. On-disk layout

### 4.1 Local files (the source of truth; written by the client)

Directory: `self.dir`, i.e. the `directory=` constructor argument, else `DEFAULT_DIR`
(`telemetry.py:75`). In production that is `%LOCALAPPDATA%\Likhi\`.

```
%LOCALAPPDATA%\Likhi\
  install_id           16 hex chars, no newline        (§3)
  metrics.jsonl        append-only NDJSON, CRLF        (§7.1)
  events.jsonl         append-only NDJSON, CRLF        (§7.2)
  sync_state.json      one JSON object, no newline     (§8.2)
```

`metrics.jsonl` and `events.jsonl` are created lazily by `_write` (`telemetry.py:163-166`), which
calls `self.dir.mkdir(parents=True, exist_ok=True)` on **every** write:

```python
def _write(self, name: str, row: dict) -> None:
    self.dir.mkdir(parents=True, exist_ok=True)
    with open(self.dir / name, "a", encoding="utf-8") as f:
        f.write(json.dumps(row, ensure_ascii=False) + "\n")
```

The file handle is opened and closed per row. There is no buffering across rows and no `fsync`.

### 4.2 Rotation: there is none

**The local files are never rotated, size-capped, or trimmed by this module.** They grow without
bound for the life of the install. The only deletion path is `likhi-report purge`
(`report.py:148-157`), which unlinks `metrics.jsonl`, `events.jsonl` and `sync_state.json` and
deliberately leaves `install_id` alone.

What `sync` calls "rotated" (`telemetry.py:269`) is only **truncation detection** on the read side:

```python
if size < offset:  # file was rotated or deleted: start over
    offset = state[stream] = 0
```

That is: if the file is now shorter than the recorded offset, restart from byte 0 of the current
file. The `.seq` counter is **not** reset (§8.4). Verified: deleting `events.jsonl` and writing one
new line moved state from `{'events.jsonl': 120, 'events.jsonl.seq': 1}` to
`{'events.jsonl': 123, 'events.jsonl.seq': 2}` — offset restarted, seq continued.

A port must not introduce rotation. Rotating locally would either replay already-shipped rows
(offset reset to 0 on a file that starts with old content) or, worse, silently skip rows.

### 4.3 Shipped chunk layout (folder drop, and the identical layout the HTTP collector writes)

`telemetry.py:194-200`:

```python
def _send_folder(self, target: Path, stream: str, seq: int, chunk: bytes) -> None:
    out_dir = target / self.install_id
    out_dir.mkdir(parents=True, exist_ok=True)
    name = f"{stream.split('.')[0]}-{seq:05d}.jsonl"
    tmp = out_dir / (name + ".part")
    tmp.write_bytes(chunk)
    tmp.replace(out_dir / name)  # atomic publish: readers never see a half file
```

```
<drop>\
  3f9a12c7d4e5b601\        <- install_id, one directory per machine
    metrics-00001.jsonl
    metrics-00002.jsonl
    events-00001.jsonl
  a71b93ff02c4d5e8\
    metrics-00001.jsonl
    events-00001.jsonl
```

Naming rules, exactly:

- Stream base name is `stream.split('.')[0]` — `"metrics.jsonl"` → `"metrics"`,
  `"events.jsonl"` → `"events"`. Split on the **first** `.`, take element 0.
- `f"{seq:05d}"`: zero-padded to a **minimum** width of 5. It is not truncated.
  Verified: `1 → metrics-00001.jsonl`, `99999 → metrics-99999.jsonl`,
  `100000 → metrics-100000.jsonl`, `1234567 → metrics-1234567.jsonl`.
  (The collector's `RE_SEQ = ^[0-9]{1,9}$` accepts up to 9 digits; `likhi-report collect` globs
  `metrics-*.jsonl` and `sorted()`s lexicographically, so past 99999 the sort order stops being
  numeric. Not a correctness problem for `collect`, which only sums.)
- Temp file is `name + ".part"`, i.e. `metrics-00001.jsonl.part` — the suffix is **appended**, the
  `.jsonl` is kept. (The server uses `path.with_suffix(".part")` at `ingest_server.py:100`, which
  *replaces* it, giving `metrics-00001.part`. Different, and irrelevant to the client port, but do
  not "harmonise" them.)
- `tmp.replace(dest)` is an atomic rename over an existing destination (Windows
  `MoveFileEx(..., MOVEFILE_REPLACE_EXISTING)`). In Rust: `std::fs::rename`, which has the same
  replace-existing semantics on Windows and POSIX. Do **not** use copy-then-delete.
- The chunk file is written with `write_bytes` — **raw bytes, no newline translation**, no
  re-encoding. The chunk is a verbatim byte slice of the local file (§8.3), CRLF and all.

### 4.4 The metrics/events split, and why it matters

Two independent streams with independent offsets, independent sequence counters and independent
chunk name prefixes. They are always iterated in this fixed order (`telemetry.py:263`):

```python
for stream in ("metrics.jsonl", "events.jsonl"):
```

`metrics` first, then `events`. Keep that order: it decides which stream gets its chunk published
first when a destination fails partway, and therefore what a collector sees during a partial sync.

---

## 5. Time: the hour bucket

`telemetry.py:48-49`:

```python
def _hour_bucket(when: float | None = None) -> str:
    return time.strftime("%Y-%m-%dT%H", time.localtime(when if when is not None else time.time()))
```

- Format: `%Y-%m-%dT%H` → e.g. `2026-09-16T15`. Always 13 characters for 4-digit years.
  No minutes, no seconds, no timezone offset, no `Z`.
- **Local time**, via `time.localtime`. Verified on this machine: local bucket `2026-09-18T05`
  while UTC would have been `2026-09-17T23`. A Rust port using `Utc::now()` will produce rows the
  collector groups into the wrong hour and, worse, will break `likhi-report pull --since`
  (`report.py:247`, `ingest_server.py:162` compares `row["h"] < since` as **strings**).
  Use the machine's local timezone, with whatever DST rule the OS applies.
- Because buckets are local and unqualified, a machine that changes timezone or crosses a DST
  boundary can emit two rows with the same `h` covering different real hours, or skip a bucket.
  The collector sums rows per bucket, so this degrades gracefully. Do not "fix" it by adding an
  offset to the string — the collector does a prefix/lexicographic comparison.

`self.bucket` (`telemetry.py:82`) is a **snapshot**, set at construction and re-set at the end of
every flush (`telemetry.py:179`). It is the value written into rows, not the current hour.

---

## 6. Byte-exact serialisation rules

These four rules together are what "byte-compatible" means. Verified end to end by running the real
module and dumping raw bytes.

### 6.1 Line terminator is CRLF on Windows, not LF

`_write` opens the file in **text mode** (`telemetry.py:165`):

```python
with open(self.dir / name, "a", encoding="utf-8") as f:
    f.write(json.dumps(row, ensure_ascii=False) + "\n")
```

Python text mode with the default `newline=None` translates every `"\n"` written into `os.linesep`.
On Windows `os.linesep == '\r\n'`. Verified raw bytes of a real `metrics.jsonl`:

```
b'{"h": "2026-09-18T05", "id": "3f9a12c7d4e5b601", "words": 2, ... "lat_n": 2}\r\n'
```

**Consequences a port must honour:**

- Local `metrics.jsonl` / `events.jsonl` contain `\r\n` line endings on Windows.
- Byte offsets in `sync_state.json` therefore count 2 bytes per terminator, not 1. A Rust port
  writing `\n` produces offsets that are off by one byte per line — and if a Rust build is ever
  dropped onto a machine with an existing Python-written `sync_state.json`, the saved offset will
  land mid-line and the first chunk will begin with a JSON fragment.
- The CRLF survives verbatim into the shipped chunks (`write_bytes` of a raw slice), into Cloud
  Storage objects, and into the ingest server's stored bodies. Verified drop-folder chunk:
  `b'{"h": ... "retyped": true}\r\n'`.
- Downstream consumers already tolerate it: `report._read_jsonl` does `line.strip()`
  (`report.py:52`), `ingest_server` uses `.splitlines()` (`ingest_server.py:155, 201`).
  Tolerance is not equivalence — you still must emit CRLF to be byte-identical.
- The **same code on Linux/macOS emits LF** (`os.linesep == '\n'`). If the port is ever built for
  a non-Windows host, match the host's `os.linesep`, not a hardcoded `\r\n`.

Rust: this must be explicit. `writeln!` and `"\n"` both emit LF on every platform. Emit
`format!("{json}\r\n")` (or the platform line separator) and open the file with
`OpenOptions::new().append(true).create(true)`.

### 6.2 JSON separators are Python's defaults: `", "` and `": "`

`json.dumps(obj)` with no `separators=` argument uses `(', ', ': ')` — **a space after every comma
and after every colon**. Verified:

```
{"h": "x", "roman": "amr", "chose": "আমরা", "pos": 2, "retyped": false}
```

`serde_json::to_string` produces the compact form `{"h":"x",...}` with no spaces. That is *not*
byte-compatible. Options: a custom `serde_json::ser::Formatter` overriding `begin_object_value`
(write `b": "`) and `begin_object_key`/`begin_array_value` (write `b", "` when not first), or
build the line by hand. There is no whitespace before `{`, after `}`, or around the outermost
braces.

### 6.3 Key order is counter **insertion order**, not sorted, not fixed

`json.dumps` does **not** sort keys (`sort_keys` defaults to `False`), and `dict`/`Counter`
preserve insertion order. `_flush_locked` builds the row as (`telemetry.py:170`):

```python
row = {"h": self.bucket, "id": self.install_id, **dict(self.counters)}
```

so the key order in a metrics row is:

1. `"h"`
2. `"id"`
3. every counter key, **in the order that key was first incremented since the last clear**
4. `"lat_p50"`, `"lat_p95"`, `"lat_n"` (only when `self.latencies` is non-empty, always in that
   order, appended after all counters)

Verified with a real two-word session (first word `index=0, app=chrome.exe`, second word
`index=2, retyped=True, app=WINWORD.EXE`, then `note("pipe_fallback")`):

```
{"h": "2026-09-18T05", "id": "3f9a12c7d4e5b601", "words": 2, "pos_0": 1, "top1_taken": 1,
 "app_chrome.exe": 1, "pos_2": 1, "retyped": 1, "app_WINWORD.EXE": 1, "pipe_fallback": 1,
 "lat_p50": 31.5, "lat_p95": 31.5, "lat_n": 2}
```

Note `words, pos_0, top1_taken, app_chrome.exe` (the first commit's increments, in source order),
then `pos_2, retyped, app_WINWORD.EXE` (the second commit's *new* keys only — `words` was already
present and keeps its original position), then `pipe_fallback` from `note()`.

Rust: `HashMap` iteration order is randomised per process; `BTreeMap` sorts. **Neither matches.**
Use `indexmap::IndexMap<String, u64>` (or a `Vec<(String, u64)>` with linear/side-index lookup) and
insert on first increment. `self.counters.clear()` (`telemetry.py:177`) resets the order for the
next row, so order does not carry across rows.

Critically: **reading a `Counter` key does not insert it.** `collections.Counter.__missing__`
returns `0` without storing. `telemetry.py:145` reads `self.counters["words"]` as the flush test and
`telemetry.py:112-127` reads nothing else. Verified: reading an untouched key left
`list(c.keys()) == ['words', 'pos_0', 'app_chrome.exe']`. A Rust `entry().or_insert(0)` used for a
*read* would inject a phantom zero-valued key into the row. Read through a method that does not
insert.

### 6.4 `ensure_ascii=False` — Bangla is written as raw UTF-8

`_write` passes `ensure_ascii=False` (`telemetry.py:166`), so `আমরা` is emitted as the 12 UTF-8
bytes `\xe0\xa6\x86\xe0\xa6\xae\xe0\xa6\xb0\xe0\xa6\xbe`, not as `\u0986\u09ae\u09b0\u09be`.
Verified in both the local file and the drop chunk. Files are UTF-8 with **no BOM**.

`serde_json` already emits raw UTF-8 by default — but it also escapes some characters Python does
not, and vice versa. Both escape `"`, `\`, and C0 controls; Python emits `\uXXXX` for C0 controls
other than `\b \f \n \r \t`, serde_json emits `\uXXXX` too. Neither escapes `/`. Since redaction
(§7.3) already rejects anything containing `\`, `/`, `:` or a digit, and control characters cannot
reach here through the IME, the practical risk is low — but if you ever relax redaction, re-verify.

**Exception:** `sync_state.json` is written with plain `json.dumps(state)` (`telemetry.py:295`),
i.e. `ensure_ascii=True`. Its keys are always ASCII (`"metrics.jsonl"`, `"events.jsonl"`,
`"metrics.jsonl.seq"`, `"events.jsonl.seq"`), so this never shows — but a port should not
gratuitously change it. Verified: `{"metrics.jsonl": 226, "metrics.jsonl.seq": 1, "events.jsonl":
119, "events.jsonl.seq": 1}` — default separators, no trailing newline.

### 6.5 Number formatting

- Counters are Python `int` → plain integers, no decimal point: `"words": 2`.
- `lat_p50` / `lat_p95` are Python `float` (see §9.3) → **always carry a decimal point**:
  `round(12.0, 1)` serialises as `12.0`, never `12`. Verified.
  Rust `serde_json` serialises `f64` `12.0` as `12.0`, so this matches — but only if you keep the
  value typed as float. Do not "tidy" an integral percentile to an integer.
- `lat_n` is an `int` → `2`.
- `pos` in an events row is an `int`.
- Python uses `repr(float)` (shortest round-trip); `serde_json` uses ryu (also shortest
  round-trip). These agree for every value `round(x, 1)` can produce in the normal latency range.

---

## 7. Record schemas

### 7.1 `metrics.jsonl` row

Written only by `_flush_locked` (`telemetry.py:168-179`), and only when
`if self.counters or self.latencies:` — i.e. **a row is skipped entirely when both are empty**
(no empty `{"h":..,"id":..}` heartbeat rows).

```python
def _flush_locked(self) -> None:
    if self.counters or self.latencies:
        row = {"h": self.bucket, "id": self.install_id, **dict(self.counters)}
        if self.latencies:
            s = sorted(self.latencies)
            row["lat_p50"] = round(s[len(s) // 2], 1)
            row["lat_p95"] = round(s[min(len(s) - 1, int(len(s) * 0.95))], 1)
            row["lat_n"] = len(s)
        self._write("metrics.jsonl", row)
    self.counters.clear()
    self.latencies.clear()
    self.bucket = _hour_bucket()
```

| Key | Type | Always present | Meaning |
|---|---|---|---|
| `h` | string | yes | Hour bucket, `%Y-%m-%dT%H`, local time (§5) |
| `id` | string | yes | Install id, 16 hex chars, or `"unknown"` (§3) |
| `words` | int | when >0 | Committed words counted in this row |
| `top1_taken` | int | when >0 | Commits with `index == 0` |
| `pos_0` … `pos_9` | int | per-position | Candidate position taken, clamped (§8.1) |
| `retyped` | int | when >0 | Commits where `retyped` was true — **an int here** |
| `app_<sanitised>` | int | per app | Per-foreground-application commit count (§8.1) |
| `<free-form>` | int | per `note()` | Anything `note()` recorded (§8.2) |
| `lat_p50` | float | only with latencies | See §9.3 |
| `lat_p95` | float | only with latencies | See §9.3 |
| `lat_n` | int | only with latencies | Number of latency samples in this row |

The metrics row schema is **open**: a collector must tolerate unknown keys, because `app_*` and
`note()` keys are unbounded. `likhi-report collect` only reads `words`, `top1_taken`, `retyped`,
`lat_p50`, `lat_p95` (`report.py:176-180`).

**`retyped` appears in both streams with different types**: integer count in `metrics.jsonl`,
JSON boolean in `events.jsonl`. A Rust port with one shared struct or one `Retyped` newtype will
get this wrong. Keep them separate types.

### 7.2 `events.jsonl` row (struggle event)

Written only from `commit` (`telemetry.py:134-144`), inline, under the lock:

```python
self._write(
    "events.jsonl",
    {
        "h": self.bucket,
        "roman": roman.lower(),
        "chose": chosen,
        "we_said": top1,
        "pos": index,
        "retyped": bool(retyped),
    },
)
```

Exactly six keys, **always all six, always in this order**:

| Key | Type | Value |
|---|---|---|
| `h` | string | Hour bucket (§5) |
| `roman` | string | `roman.lower()` — **lowercased**, full Unicode `str.lower()` |
| `chose` | string | `chosen` **verbatim**, not lowercased, not normalised |
| `we_said` | string | `top1` verbatim; **`""` when the caller passed no `top1`** |
| `pos` | int | `index` **raw, not clamped to 9** (contrast the `pos_N` counter) |
| `retyped` | bool | `bool(retyped)` → JSON `true` / `false` |

No `id` field: the install is identified by the containing directory / the `X-Likhi-Install`
header, never inside the events row. Do not add one.

`roman.lower()` is Python's full Unicode lowercasing, which differs from ASCII-lowercase for
non-ASCII input. In practice `roman` is the composition buffer, which `likhi_ime.py:307`
(`if ch and ch.isalpha()`) admits non-ASCII letters into on an unusual keyboard layout. Rust:
`str::to_lowercase()` (full Unicode), **not** `to_ascii_lowercase()`. They differ for e.g. `İ`
(Python `'i̇'`, two code points; Rust `to_lowercase` also yields `i̇`). Close enough to match in
practice, but if you need exactness the rule is "Unicode simple+special lowercase mapping", and
Python applies `str.lower()` which is the Unicode default full lowercase mapping without
locale tailoring. Rust's `to_lowercase` is the same specification.

Byte-exact golden row (verified):

```
b'{"h": "2026-09-18T05", "roman": "amr", "chose": "\xe0\xa6\x86\xe0\xa6\xae\xe0\xa6\xb0\xe0\xa6\xbe", "we_said": "\xe0\xa6\x86\xe0\xa6\xae\xe0\xa6\xbe\xe0\xa6\xb0", "pos": 2, "retyped": true}\r\n'
```

which is the row documented for testers in `docs/PILOT.md:20`:

```json
{"h": "2026-09-16T15", "roman": "amr", "chose": "আমরা", "we_said": "আমার", "pos": 2, "retyped": false}
```

### 7.3 `sync_state.json`

One JSON object, four keys at most, written with `write_text` (no trailing newline, UTF-8, default
separators). Keys are literal stream file names, so they contain a `.`:

```
{"metrics.jsonl": 226, "metrics.jsonl.seq": 1, "events.jsonl": 119, "events.jsonl.seq": 1}
```

| Key | Type | Meaning |
|---|---|---|
| `"metrics.jsonl"` | int | Byte offset into the local file up to which everything has been shipped |
| `"metrics.jsonl.seq"` | int | Highest chunk sequence number successfully published |
| `"events.jsonl"` | int | idem |
| `"events.jsonl.seq"` | int | idem |

Key insertion order follows the stream loop order (`metrics` first) and, within a stream, offset
then `.seq` (`telemetry.py:287-288`). Values are read back with `int(state.get(..., 0))`
(`telemetry.py:267, 281`) — a string value in the file would be coerced. A parse failure of the
whole file is swallowed and treated as `{}` (`telemetry.py:255-260`), which means **everything is
re-shipped from byte 0 with seq restarting at 1**, overwriting existing chunk files in the drop
folder and being deduplicated by the HTTP collector. Preserve the swallow; do not make it fatal.

---

## 8. Recording API

### 8.1 `commit()` — the only hot path

`telemetry.py:99-150`. Full signature:

```python
def commit(
    self,
    roman: str,
    chosen: str,
    *,
    index: int = 0,
    top1: str = "",
    app: str = "",
    retyped: bool = False,
    secure: bool = False,
    latency_ms: float | None = None,
) -> None:
```

Body, in exact execution order:

```python
if self.mode == "off" or secure:
    return
try:
    with self.lock:
        if _hour_bucket() != self.bucket:
            self._flush_locked()
        self.counters["words"] += 1
        self.counters[f"pos_{min(index, 9)}"] += 1
        if index == 0:
            self.counters["top1_taken"] += 1
        if retyped:
            self.counters["retyped"] += 1
        if app:
            self.counters[f"app_{re.sub(r'[^a-zA-Z0-9._-]', '', app)[:24]}"] += 1
        if latency_ms is not None and len(self.latencies) < 20000:
            self.latencies.append(float(latency_ms))
        if (
            self.mode == "full"
            and (index > 0 or retyped)
            and is_recordable(roman, chosen)
            and (not top1 or is_recordable(roman, top1))
        ):
            self._write("events.jsonl", {...})
        if self.counters["words"] >= FLUSH_EVERY:
            self._flush_locked()
except Exception:
    pass  # telemetry must never break typing
```

**Ordering rules a port must preserve (this order *is* the key order of §6.3):**

1. **Guard first**: `mode == "off"` or `secure` → return with **no side effect at all**, not even
   the word count. Confirmed by `tests/test_telemetry.py:49-54`.
2. **Hour-rollover check before counting.** If the wall clock has moved into a new hour bucket
   since `self.bucket` was set, flush *first* (writing the previous hour's row with the previous
   `h`), which also advances `self.bucket` to now. Only then is this word counted — into the new
   bucket. This also means the events row written later in the same call carries the **new**
   bucket. Verified: a run that pre-set `t.bucket = "2026-09-16T15"` emitted `"h": "2026-09-18T05"`
   because the first `commit` detected the mismatch and rolled over before doing anything else.
3. `words` is incremented **unconditionally** (after the guard), so it is always the first counter
   key inserted after a clear.
4. `pos_{min(index, 9)}` — clamped at 9. `index=0 → pos_0`, `index=12 → pos_9`. Negative indices
   are **not** clamped: `min(-3, 9) == -3` → key `pos_-3`. No caller produces one
   (`server.py:185` does `int(req.get("index", 0))` from a client that sets `self.cursor`, which
   is non-negative), but a port that uses an unsigned type will silently differ on malformed input.
5. `top1_taken` only when `index == 0` exactly.
6. `retyped` counter only when `retyped` is truthy.
7. `app` counter only when the **raw** `app` string is truthy. The sanitisation is applied *after*
   that test, so an app name that sanitises to the empty string still produces the key `app_`
   (with a trailing underscore and nothing else). Verified: `"!!!"` → `app_`.
8. Latency append only when `latency_ms is not None` **and** `len(self.latencies) < 20000`.
   The cap is a hard ceiling of 20000 samples per row; further samples in the same row are
   dropped silently, and `lat_n` therefore saturates at 20000 while `words` keeps climbing.
   `float(latency_ms)` coerces ints to float.
9. Events row (see §8.4 for the predicate).
10. **Flush check last**: `if self.counters["words"] >= FLUSH_EVERY`. Note `>=` not `==` — a
    port that resets differently could overshoot; with this code the clear makes it fire exactly
    on the 20th word. Reading `counters["words"]` here must not insert the key (§6.3).
    This flush is **local only** — it writes `metrics.jsonl` and does not sync. Comment at
    `telemetry.py:146-147`: *"Local write only. Syncing to the drop folder is network I/O and stays
    on the background timer, never on the path of a committed keystroke."*
11. The whole body is wrapped in `try/except Exception: pass`. **Every** failure — disk full,
    permission denied, encoding error — is swallowed. `telemetry.py:150`:
    `pass  # telemetry must never break typing`. A Rust port must use `let _ = ...` /
    `.ok()` at the same boundary, never `?` or `unwrap`. This is a hard requirement of the
    project's typing-latency rule.

#### `app` sanitisation, exactly

```python
f"app_{re.sub(r'[^a-zA-Z0-9._-]', '', app)[:24]}"
```

Delete every character **not** in `[A-Za-z0-9._-]`, **then** truncate the result to 24 characters,
then prefix `app_`. Order matters: delete-then-truncate, so a name full of removed characters
yields a longer surviving tail than truncate-then-delete would. Case is **preserved** (no
lowercasing). Verified:

| Input `app` | Counter key |
|---|---|
| `chrome.exe` | `app_chrome.exe` |
| `WINWORD.EXE` | `app_WINWORD.EXE` (case kept) |
| `Ap p.exe` | `app_App.exe` (space removed, then 7 chars) |
| `Microsoft.Teams_8wekyb3d8bbwe!MSTeams` | `app_Microsoft.Teams_8wekyb3d` (24 chars) |
| `averyveryverylongapplicationname.exe` | `app_averyveryverylongapplica` (24 chars) |
| `!!!` | `app_` |

The surviving character set is exactly: `A-Z`, `a-z`, `0-9`, `.`, `_`, `-`. Everything else —
including spaces, `!`, `+`, `(`, `)`, and every non-ASCII character — is deleted. Verified above:
the underscore in `Microsoft.Teams_8wekyb3d8bbwe` survives, the `!` does not.

`app` is truncated on **characters**, not bytes; the input is a Windows executable basename from
`GetModuleFileNameExW` (`likhi_ime.py:80-107`), so it may be non-ASCII, in which case the
non-ASCII characters are stripped entirely by the class.

### 8.2 `note()` — free-form counters

`telemetry.py:152-159`:

```python
def note(self, counter: str) -> None:
    if self.mode == "off":
        return
    try:
        with self.lock:
            self.counters[re.sub(r"[^a-z0-9_]", "", counter)[:32]] += 1
    except Exception:
        pass
```

- Active in **both** `metrics` and `full` (only `off` returns early).
- Sanitisation is **lowercase ASCII letters, digits and underscore only** — uppercase letters are
  **deleted, not lowercased**. Verified: `"Model_Timeout"` → `"odel_imeout"`,
  `"UPPER"` → `""`. A name that sanitises to empty produces the JSON key `""`.
  This is almost certainly not the intent, but it is the behaviour on disk; a port must reproduce
  it or the collector's per-counter aggregation changes.
- Truncated to 32 characters after deletion.
- No `app_`/`pos_` prefix; the key lands in the same flat counter map and therefore in the same
  metrics row, interleaved by insertion order (§6.3).
- It is **not** guarded by `secure`, and it does not touch `words`, so it never triggers a flush
  by itself.
- **No production call site exists.** `grep` for `\.note\(` across the repo (excluding `dist/`)
  returns nothing but this definition. The set of `note()` counter names in real data is therefore
  currently empty; the port should still implement it, because a collector must tolerate the keys.

### 8.3 `flush()` — public, and the only thing that syncs

`telemetry.py:181-190`:

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

- `off` returns immediately — a machine in `off` mode never syncs, even if it has old files from a
  period when telemetry was on. (Those files stay on disk until `likhi-report purge`.)
- `_flush_locked` runs **under the lock**; `sync()` runs **outside it** (§11).
- `sync()` is called only when a destination is configured. `sync()` itself re-checks and returns
  an error dict if not (`telemetry.py:249-250`).
- `_flush_locked` always resets `self.bucket = _hour_bucket()` and clears both containers, even
  when no row was written.

### 8.4 The struggle-event predicate, term by term

```python
self.mode == "full"
and (index > 0 or retyped)
and is_recordable(roman, chosen)
and (not top1 or is_recordable(roman, top1))
```

1. `self.mode == "full"` — `metrics` never writes `events.jsonl`. Confirmed by
   `tests/test_telemetry.py:23-31` (asserts the file does not exist).
2. `(index > 0 or retyped)` — **only struggles**. A word accepted at position 0 with no backspace
   is never recorded. This is the central privacy property: the text of correctly-predicted words
   never leaves memory. `docs/PILOT.md:16-24`, `telemetry.py:8-10`.
3. `is_recordable(roman, chosen)` (§9).
4. `(not top1 or is_recordable(roman, top1))` — short-circuits when `top1` is empty (so an empty
   `we_said` is allowed through), otherwise re-checks. Note it re-tests `roman` as the first
   argument — redundant with term 3, harmless, and it means the effective test on `top1` is
   `top1` non-empty, `len(top1) <= 32`, and no unsafe character in `top1`. Reproduce the call
   shape anyway: if you inline it as "check top1 only", behaviour is identical today but diverges
   if `is_recordable` ever gains a cross-field rule.

Evaluation is Python's short-circuit `and`, so `is_recordable` is not called when the mode or
struggle test already failed.

Confirmed by `tests/test_telemetry.py:34-46`: of three commits (`index=0` accepted,
`index=2` struggle, `index=1` with digits), exactly one line is written.

---

## 9. Privacy filters — exactly what is suppressed

### 9.1 `is_recordable`

`telemetry.py:52-58`:

```python
def is_recordable(roman: str, word: str) -> bool:
    """True when this pair is safe to write: no digits, symbols, or over-long strings."""
    if not roman or not word:
        return False
    if len(roman) > MAX_LEN or len(word) > MAX_LEN:
        return False
    return not (_RE_UNSAFE.search(roman) or _RE_UNSAFE.search(word))
```

Three rules, in order:

1. **Empty rejected.** Either side empty (or `None`-ish falsy) → `False`.
2. **Length.** Either side strictly longer than `MAX_LEN = 32` **code points** → `False`.
   `len(roman) > 32`, i.e. 32 is allowed, 33 is not.
3. **Unsafe characters.** `re.search(r"[\d@:/\\]")` on **either** string → `False`.
   The class is: any Unicode decimal digit, `@`, `:`, `/`, `\`.

The rationale is in the module docstring (`telemetry.py:13`) and `docs/PILOT.md:28`: *"anything
with a digit, '@', ':' or '/' is dropped (IDs, passwords, URLs, times, money)"*.

**`\d` is Unicode-aware in Python 3.** It matches Bangla digits `০-৯` (U+09E6–U+09EF) and every
other Unicode `Nd` character, not just ASCII `0-9`. Verified: `'২'` (Bangla 2) and `'٣'`
(Arabic-Indic 3) both match `_RE_UNSAFE`. This matters enormously here — the IME converts typed
digits to Bangla digits (`likhi_ime.py:109`: `BANGLA_DIGITS = "০১২৩৪৫৬৭৮৯"`), and an ASCII-only
digit test would leak PINs and phone numbers written in Bangla numerals.

Rust: the `regex` crate's `\d` is Unicode-aware by default (`\p{Nd}`) — **do not** enable
`unicode(false)` or use `is_ascii_digit()`. If hand-coding, test `char::is_numeric()`… careful:
`char::is_numeric()` covers `Nd | Nl | No`, which is *broader* than `\d`'s `Nd`. Use
`\p{Nd}` semantics exactly, or the divergence goes the safe direction only by accident.

Verified table:

| Input | Recordable? | Reason |
|---|---|---|
| `amar` | safe | — |
| `pin1234` | rejected | ASCII digit |
| `me@x` | rejected | `@` |
| `a/b` | rejected | `/` |
| `a\b` | rejected | `\` |
| `12:30` | rejected | digit and `:` |
| `full-stop.` | safe | `.` and `-` are not in the class |
| `a_b` | safe | `_` is not in the class |
| `কথা` | safe | Bangla letters |
| `২` | rejected | Bangla digit matches `\d` |
| `٣` | rejected | Arabic-Indic digit matches `\d` |
| `"x" * 40` | rejected | length |
| `""` | rejected | empty |

`is_recordable` is a **module-level public function** (imported by `tests/test_telemetry.py:3`).
Export it as such in the port.

### 9.2 `secure` — the password-box flag

`telemetry.py:112`: `if self.mode == "off" or secure: return`.

When `secure=True`, **nothing at all is recorded** — not the event, not the counters, not the
latency, not even `words`. Confirmed by `tests/test_telemetry.py:49-54`, which asserts neither
file exists after a secure commit.

**No shipped client ever sets it.** `server.py:189` reads `secure=bool(req.get("secure", False))`
from the request, and the PIME client (`likhi_ime.py:382-400`) builds its `learn` message with
only `op, roman, chosen, index, top1, retyped` and conditionally `app`. It never sends `secure`.
So in production `secure` is always `False`. It is a caller-facing API contract, not an active
filter. `docs/PILOT.md:36-38` states the reasoning honestly:

> Note on password boxes: Windows normally does not route password fields through a text service,
> so composition does not happen there. The digit and symbol rules are the real safety net, not
> that behaviour, which varies by application.

A Rust port must keep the parameter and its semantics, and must not assume it is dead.

### 9.3 Which app contexts are suppressed: **none**

There is **no allowlist, blocklist, or special-casing of any application** anywhere in
`telemetry.py`. `app` is used for exactly one thing: incrementing the `app_<name>` counter
(`telemetry.py:124-125`). It never affects whether an event is written, never suppresses anything,
and is never written into `events.jsonl`.

I checked for this specifically (`grep` for `secure|password|PASSWORD|is_password|elevated` across
the repo excluding `dist/`): the only hits are UI copy in `app/LikhiApp.cs:448-450`, the docs
quoted above, and unrelated TSF/elevation notes in `docs/PLAN.md`. **If the porting brief assumed
password managers or browsers are excluded by name, that assumption is wrong** — the digit/symbol
rules and `MAX_LEN` are the entire content filter, plus the "struggles only" rule.

### 9.4 What is suppressed by construction (not by a filter)

From the module docstring (`telemetry.py:12-16`) and confirmed by the schemas in §7:

- **No timestamps finer than the hour.** Only `h`, format `%Y-%m-%dT%H`.
- **No context words, no sentences.** `commit` receives one word; `context` is passed to
  `svc.learn` (`server.py:180`) and deliberately not to `tel.commit` (`server.py:182-191`).
- **No user name or machine name.** Identity is the random install id only.
- **No IP addresses** — the collector strips them from its own logs
  (`ingest_server.py:114-115`), and the client sends none.
- **Accepted words are never written** (§8.4 term 2). This is the single largest reduction in what
  is recorded.
- **`metrics.jsonl` contains no text at all** — enforced by
  `tests/test_telemetry.py:31`: `assert "amar" not in json.dumps(row) and "আমার" not in
  json.dumps(row)`. The one text-ish key is `app_<exe name>`, which is an executable basename.

---

## 10. Latency percentiles — reproduce the formula, do not "improve" it

`telemetry.py:171-175`:

```python
s = sorted(self.latencies)
row["lat_p50"] = round(s[len(s) // 2], 1)
row["lat_p95"] = round(s[min(len(s) - 1, int(len(s) * 0.95))], 1)
row["lat_n"] = len(s)
```

- Sort ascending (`sorted` on floats; total order, no NaN handling — a NaN would poison the sort).
- **p50 index** = `len(s) // 2` — integer floor division. For even `n` this is the **upper**
  median, not the mean of the two middle values. Verified: for `n=2` samples `[12.0, 31.5]`,
  `lat_p50` came out as `31.5`.
- **p95 index** = `min(len(s) - 1, int(len(s) * 0.95))` — `int()` truncates toward zero;
  `len(s) * 0.95` is float multiplication, so float representation matters at the boundary.
- Verified index table:

| n | p50 index | p95 index |
|---|---|---|
| 1 | 0 | 0 |
| 2 | 1 | 1 |
| 3 | 1 | 2 |
| 4 | 2 | 3 |
| 10 | 5 | 9 |
| 20 | 10 | 19 |
| 21 | 10 | 19 |
| 100 | 50 | 95 |

- **`round(x, 1)` is Python's round-half-to-even**, applied to the decimal representation of the
  double. Verified: `round(0.25, 1) == 0.2`, `round(0.35, 1) == 0.3`, `round(12.45, 1) == 12.4`,
  `round(12.55, 1) == 12.6`, `round(2.5, 1) == 2.5`, `round(1.05, 1) == 1.1`.
  Rust's `f64::round` is half-**away-from-zero**, so `(x * 10.0).round() / 10.0` gives
  `0.25 → 0.3` where Python gives `0.2`. To match, implement Python's algorithm: CPython's
  `round(float, ndigits)` uses `_Py_dg_dtoa`/`_Py_dg_strtod` — correctly-rounded decimal
  string conversion with round-half-to-even — which is equivalent to formatting the double to
  1 fractional digit with banker's rounding and re-parsing. In Rust: format with
  `format!("{:.1}", x)` (Rust's `Display` for floats rounds half-to-even on the exact decimal
  value, matching CPython) and parse back to `f64`, then serialise that. **Verify this against
  the table above before shipping** — this is the single most likely silent numeric divergence.
- `lat_n` is the sample count in this row, capped at 20000 by the append guard (§8.1 item 8).
  It is **not** the number of words: a caller may pass `latency_ms=None`.
- The whole block is skipped when `self.latencies` is empty, so the three keys are absent rather
  than null/zero.

Consumer note: `likhi-report collect` averages `lat_p50` and `lat_p95` **across rows, unweighted**
(`report.py:179-185`), which is statistically wrong but is what production does. Not the port's
problem, but do not change `lat_n` semantics hoping to fix it.

---

## 11. Concurrency and failure model

- One `threading.Lock` per instance (`telemetry.py:79`), guarding `counters`, `latencies`,
  `bucket`, and the inline `events.jsonl` write inside `commit`.
- `sync()` takes **no lock at all**. It reads the local files independently, in binary, while
  `commit()` may be appending to them. This is deliberate: the only shared state is the file
  content, and the partial-line guard (§12.3) handles a torn append. A Rust port must not
  "tidy" this by taking the same mutex — doing so would put network I/O behind the keystroke
  lock, violating the project's typing-latency budget.
- `flush()` releases the lock before calling `sync()` (`telemetry.py:185-190`).
- `_write` is called from inside the lock in both paths (`commit` → events; `_flush_locked` →
  metrics), so appends from this process are serialised. Appends from a *second* process sharing
  the same directory are not, and are not guarded against. Not a scenario in production (one
  engine process per machine).
- Every public entry point swallows all exceptions: `commit` (`telemetry.py:149-150`),
  `note` (`158-159`), `flush` (`187-188`). `sync()` does **not** swallow at the top level — but
  every per-stream operation inside its loop is wrapped (`telemetry.py:290-292`) and the state
  write is wrapped (`294-297`), so it also never raises in practice. `sync()` is the one method
  that reports failure, through its return dict.
- `_install_id` swallows (`telemetry.py:94-95`).

---

## 12. `sync()` — shipping

`telemetry.py:228-307`. Signature: `sync(self, drop=None, endpoint=None) -> dict`.

### 12.1 Destination resolution and the ad-hoc rule

```python
target = Path(drop) if drop else self.drop
url = endpoint or self.endpoint
if not target and not url:
    return {"sent": 0, "error": "no drop folder or endpoint configured"}
ad_hoc = (drop is not None and Path(drop) != self.drop) or (
    endpoint is not None and endpoint != self.endpoint
)
```

- Arguments override configuration. Both may be active in one call — the chunk goes to **both**.
- No destination at all → early return `{"sent": 0, "error": "no drop folder or endpoint
  configured"}`. Verified verbatim.
- `ad_hoc` is true when an argument was passed **and differs from** the configured value. Passing
  the same destination explicitly is *not* ad hoc — verified by
  `tests/test_telemetry_sync_state.py:75-80`.
- The drop comparison is `Path(drop) != self.drop`. On Windows `PurePath.__eq__` compares a
  case-folded, separator-normalised form. Verified: `Path("C:/Foo/Bar") == Path("c:/foo/bar")` is
  `True`, and `Path("C:/Foo/") == Path("C:/Foo")` is `True`. A Rust port comparing `PathBuf` or
  `&str` directly will classify a differently-cased or trailing-slashed path as ad hoc and
  therefore silently stop recording progress. Normalise: lowercase (Windows), strip trailing
  separators, normalise `/` to `\`.
  The endpoint comparison is a plain **string** comparison, case-sensitive, no URL normalisation.
- When `self.drop` is `None` and `drop` is `None`, `Path(drop) != self.drop` is not evaluated
  (short-circuit on `drop is not None`), so `ad_hoc` stays `False`.

**Why ad-hoc matters** (`telemetry.py:238-245`, and the whole of
`tests/test_telemetry_sync_state.py`, whose docstring records that this was found the hard way
when a test consumed a live install's telemetry): the offset is **one position per stream, not one
per destination**. An ad-hoc sync therefore ships the bytes but must not record progress, or those
bytes never reach the real destination. Sequence numbers still come from the recorded state, so an
ad-hoc copy lands under names the real destination will later use — which is fine, because an
override is only for inspection.

### 12.2 State load

```python
state_path = self.dir / "sync_state.json"
try:
    state = (
        json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}
    )
except Exception:
    state = {}
```

Missing file or any parse error → `{}` (see §7.3 for the consequence).

### 12.3 The per-stream loop

```python
for stream in ("metrics.jsonl", "events.jsonl"):
    src = self.dir / stream
    if not src.exists():
        continue
    offset = int(state.get(stream, 0))
    size = src.stat().st_size
    if size < offset:  # file was rotated or deleted: start over
        offset = state[stream] = 0
    while offset < size:
        try:
            with open(src, "rb") as f:
                f.seek(offset)
                chunk = f.read(MAX_CHUNK_BYTES)
            # never split a line across chunks
            cut = chunk.rfind(b"\n")
            if cut == -1:
                break  # a partial line is still being written; wait for the next round
            chunk = chunk[: cut + 1]
            seq = int(state.get(stream + ".seq", 0)) + 1
            if target:
                self._send_folder(target, stream, seq, chunk)
            if url:
                self._send_http(url, stream, seq, chunk)
            offset += len(chunk)
            state[stream] = offset
            state[stream + ".seq"] = seq
            sent += chunk.count(b"\n")
        except Exception as e:
            errors.append(f"{stream}: {type(e).__name__}: {e}")
            break
```

Rules, each verified:

- **Fixed stream order**: `metrics.jsonl` then `events.jsonl`.
- Missing source file → skip the stream entirely (no error recorded).
- `size` is sampled **once**, before the inner loop. Bytes appended during the loop are picked up
  on the next `sync()` call, not this one.
- **Truncation reset** (§4.2): `size < offset` → offset and `state[stream]` set to 0; `.seq` kept.
- The inner `while` **drains the whole backlog in one call**, one chunk per iteration. Verified by
  appending ~285 KB to `metrics.jsonl` and calling `sync()` once: it produced
  `metrics-00003.jsonl` (262119 bytes) **and** `metrics-00004.jsonl` (23500 bytes) in that single
  call, reporting `{'sent': 6077, ...}`. The comment at `telemetry.py:42` — *"cap one upload, so a
  machine offline for a week catches up in steps"* — describes the per-request cap, **not** a
  per-round cap. A port must loop, not send one chunk and stop. (If you wanted the comment's
  behaviour, that is a behaviour change, not a fix.)
- Each iteration **re-opens the file**, seeks, and reads at most `MAX_CHUNK_BYTES`. Binary mode:
  no newline translation on read, so the CRLFs come through.
- **Line integrity**: `cut = chunk.rfind(b"\n")`; if `-1`, `break` out of the *stream's* loop (the
  other stream is still processed). Otherwise truncate to `chunk[:cut + 1]`, i.e. include the
  final `\n`. With CRLF data the chunk ends `...}\r\n` and never splits a `\r` from its `\n`,
  because the cut is taken at the `\n`. Chunk size is therefore ≤ 262144 and ends on a line
  boundary — the 262119-byte chunk above is exactly that cut.
  Verified partial-line handling: appending `b'{"h": "partial"'` (no newline) produced
  `{'sent': 0, 'drop': ...}` and left the state untouched.
- **Sequence number** is `state[stream + ".seq"] + 1`, computed **before** sending and committed
  **after** both destinations succeed. Per stream, monotonically increasing, never reused on
  success, starting at 1.
- **Destination order**: folder first, then HTTP. If the folder write succeeds and the HTTP POST
  fails, `offset` and `.seq` are **not** advanced, so the next round re-sends the identical bytes
  under the **same** sequence number. The folder file is rewritten identically (atomic replace) and
  the HTTP collector deduplicates by object name (`ingest_server.py:211-212` returns
  `{"ok": true, "stored": 0, "duplicate": true}`). This is the retry-safety property; it only holds
  because the bytes and the seq are both stable.
- `offset += len(chunk)` uses the **truncated** chunk length, so the offset always lands on a line
  boundary.
- `sent += chunk.count(b"\n")` — a cumulative count of newlines, i.e. rows, across both streams.
- **Any** exception in an iteration appends `f"{stream}: {type(e).__name__}: {e}"` to `errors` and
  `break`s out of that stream's loop. Note this leaves `offset`/`state` at the last **successful**
  position, which is the whole point.

### 12.4 State persistence

```python
if not ad_hoc:
    try:
        state_path.write_text(json.dumps(state), encoding="utf-8")
    except Exception:
        pass
```

Written **once, at the end**, not per chunk. So a process killed mid-`sync()` re-sends everything
it shipped since the call started, under the same sequence numbers — safe by the same
immutable-chunk argument. Written with `write_text` (default separators, `ensure_ascii=True`, no
trailing newline; see §7.3). **Not** atomic — no `.part`/rename here, unlike the chunks.

### 12.5 Return value

```python
out: dict = {"sent": sent}
if target:
    out["drop"] = str(target)
if url:
    out["endpoint"] = url
if ad_hoc:
    out["ad_hoc"] = True
if errors:
    out["error"] = "; ".join(errors)
return out
```

Key order is exactly `sent`, `drop`, `endpoint`, `ad_hoc`, `error`. `drop` is `str(Path)` — on
Windows that is a backslash path, e.g. `C:\Users\...\drop`. `likhi-report sync` prints this dict as
JSON (`report.py:98`) and exits non-zero when `error` is present (`report.py:99`), so the key names
are part of the CLI contract. Errors are joined with `"; "` (semicolon + space).

---

## 13. HTTP upload

`telemetry.py:202-226`:

```python
import urllib.request

req = urllib.request.Request(
    url,
    data=chunk,
    method="POST",
    headers={
        "Content-Type": "application/x-ndjson",
        "X-Likhi-Install": self.install_id,
        "X-Likhi-Stream": stream.split(".")[0],
        "X-Likhi-Seq": str(seq),
        "X-Likhi-Version": "1",
        **({"X-Likhi-Key": self.key} if self.key else {}),
    },
)
with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT_S) as resp:
    if not 200 <= resp.status < 300:
        raise OSError(f"ingest returned {resp.status}")
```

| Header | Value | Notes |
|---|---|---|
| `Content-Type` | `application/x-ndjson` | Fixed. |
| `X-Likhi-Install` | `self.install_id` | 16 hex chars, or `"unknown"` (§3.1). Server validates `^[0-9a-f]{8,32}$`. |
| `X-Likhi-Stream` | `metrics` or `events` | `stream.split(".")[0]` — **note this uses `split(".")[0]`, while `_send_folder` uses `split('.')[0]`; identical.** Server validates `^(metrics\|events)$`. |
| `X-Likhi-Seq` | `str(seq)` | Decimal, **no zero padding** (unlike the filename). Server validates `^[0-9]{1,9}$` and re-pads with `:05d` itself (`ingest_server.py:209`). |
| `X-Likhi-Version` | `"1"` | Literal string. Not read by the current server; a protocol marker. |
| `X-Likhi-Key` | `self.key` | **Only present when `self.key` is truthy.** Omitted entirely otherwise — not sent empty. |

Method: `POST`. Path: whatever the configured endpoint URL is; production uses
`https://<host>/v1/ingest` (`ingest_server.py:182` rejects anything else with 404).
Body: the raw chunk bytes, verbatim, including CRLFs. `Content-Length` is set by urllib from
`len(data)`. No `Transfer-Encoding: chunked`, no compression, no `Expect: 100-continue`.

**Header name casing on the wire.** `urllib.request.Request.add_header` applies
`key.capitalize()`, so the headers are actually transmitted as `Content-type`, `X-likhi-install`,
`X-likhi-stream`, `X-likhi-seq`, `X-likhi-version`, `X-likhi-key`. Verified:

```
{'Content-type': 'application/x-ndjson', 'X-likhi-install': 'aaaaaaaaaaaaaaaa',
 'X-likhi-stream': 'metrics', 'X-likhi-seq': '1', 'X-likhi-version': '1',
 'X-likhi-key': 'SECRET'}
```

HTTP header names are case-insensitive and `http.server` matches them case-insensitively, so the
production collector is unaffected. A Rust port using `reqwest`/`hyper` will send lowercase
(`x-likhi-install`), which is equally correct. **Do not treat the capitalised form as part of the
contract**, but do check any reverse proxy or WAF in front of the endpoint that might match
case-sensitively.

Timeout: `HTTP_TIMEOUT_S = 10.0` seconds, passed to `urlopen`. In urllib this is the socket
timeout, applied to the connection and to each socket operation — not a total-request deadline.
`reqwest`'s `.timeout()` is a total deadline, which is stricter; use `.connect_timeout(10s)` plus a
read timeout if you want equivalence, or accept the stricter behaviour deliberately.

**TLS**: `urllib.request.urlopen` verifies certificates against the system/OpenSSL trust store by
default on Python 3. Keep verification on.

**Redirects**: urllib follows 3xx automatically, and raises `HTTPError` for any 4xx/5xx. So the
`if not 200 <= resp.status < 300` check is effectively unreachable — a non-2xx has already raised
before it runs. The raised exception is caught by `sync()`'s per-iteration handler and recorded as
`f"{stream}: HTTPError: HTTP Error 400: Bad Request"` or similar. A Rust port that does *not* auto-
raise must check the status and produce an error; the message format only affects the
`likhi-report sync` output, not the data.

---

## 14. The sync thread: timing and lifecycle

**`telemetry.py` contains no thread.** `sync()`'s docstring (`telemetry.py:235`) says *"Runs on a
background thread only. Never call it from a keystroke path."* — the thread lives in the caller.
`src/likhi/server.py:270-288`:

```python
stop = threading.Event()

# Sync interval. Each round is at most one upload per stream, so this sets the request
# rate: 15 minutes keeps a pilot inside Cloud Storage's 5,000 free writes a month, and
# nothing is lost in between because the local files are the source of truth.
interval = float(cfg["sync_seconds"])

def _flusher() -> None:
    # One early round, then the steady interval. Waiting a full interval for the first
    # upload means a new install is invisible for that long, so there is no way to tell a
    # working installation from a silently disabled one; worse, a machine switched off
    # before the first tick never reports at all, which is the normal life of an office PC.
    # The early round is cheap because sync sends only bytes written since last time.
    if not stop.wait(min(60.0, interval)):
        telemetry.flush()
    while not stop.wait(interval):
        telemetry.flush()

threading.Thread(target=_flusher, daemon=True).start()
```

Exact schedule:

1. **First round** at `min(60.0, interval)` seconds after the thread starts — at most 60 s, so a
   fresh install reports within a minute.
2. **Then every `interval` seconds**, forever, calling `telemetry.flush()` (which writes the
   metrics row *and* syncs — §8.3).
3. `stop.wait(t)` returns `True` if the event was set during the wait; both loops therefore exit
   immediately on shutdown without firing another flush.
4. The thread is a **daemon** thread: it does not keep the process alive.
5. On shutdown (`server.py:313-315`): `stop.set()` then a final synchronous `telemetry.flush()` in
   the main thread. So the last round happens on the main thread, not the timer thread.

`interval` comes from `_telemetry_config()` (`server.py:368-406`):

| Source | Key | Default |
|---|---|---|
| Environment (wins entirely) | `LIKHI_TELEMETRY_SYNC_S` | `900` (`server.py:381`) |
| Config JSON | `telemetry_sync_seconds` | `900.0` (`server.py:402`) |
| No config found at all | — | `900.0` (`server.py:406`) |

So the production default is **900 seconds = 15 minutes**. The same function supplies `mode`
(`telemetry`), `drop` (`telemetry_drop`), `endpoint` (`telemetry_endpoint`), `key`
(`telemetry_key`). Environment variables `LIKHI_TELEMETRY`, `LIKHI_TELEMETRY_DROP`,
`LIKHI_TELEMETRY_ENDPOINT`, `LIKHI_TELEMETRY_KEY` short-circuit the file search entirely when
`LIKHI_TELEMETRY` is set. Config files are read with `encoding="utf-8-sig"` (`server.py:388`) to
tolerate the BOM Notepad writes — a bug that *"would silently disable telemetry"* (`server.py:387`).

There is also a **flush-on-count** path independent of the timer: every 20 committed words,
`commit()` writes a metrics row locally without syncing (§8.1 item 10). The timer is what turns
local rows into shipped chunks.

`docs/PILOT.md:186` says *"Sync runs every five minutes"*. **That is wrong** — the code default is
900 s. `docs/PILOT.md:52` separately recommends `"telemetry_sync_seconds": 3600` for the Cloud
Run deployment. Do not take the five-minute figure into the port.

---

## 15. Uncertain

Flagged rather than guessed.

1. **Whether the 20 000-latency cap is ever reached in practice.** With `FLUSH_EVERY = 20`, a
   metrics row normally holds ≤ 20 latency samples, so the cap only bites if `commit` is called
   with `latency_ms` far more often than words are counted — which cannot happen, since both are
   in the same call. I believe the cap is dead code that exists to bound memory if `_flush_locked`
   somehow stops running. I have not proven no path reaches it. Implement the cap regardless.

2. **`round()` equivalence between CPython and `format!("{:.1}", x)` in Rust.** I verified seven
   values by hand (§10) and reasoned from CPython's use of `_Py_dg_dtoa`. I did **not** run a
   differential test against a Rust implementation. Before shipping, run a randomised differential
   test over the realistic latency range (say 0–500 ms, plenty of values near `.x5`) comparing the
   two serialisations. This is the highest-risk numeric item in the port.

3. **Non-Windows line endings.** I verified `os.linesep == '\r\n'` on this machine only. The CRLF
   behaviour is a property of Python text mode plus the platform, so a Linux build of the same
   Python code writes LF. Which behaviour the Rust port should have depends on whether the port is
   Windows-only (it is, today — `shell/src/` is a Windows TSF service) and whether any historical
   collector data was produced on Linux. I assume Windows-only and CRLF; confirm before relying
   on offsets being comparable across platforms.

4. **Exact Unicode lowercasing equivalence** between Python `str.lower()` and Rust
   `str::to_lowercase()` for every input the composition buffer can hold. Both implement the
   Unicode default full lowercase mapping without locale tailoring, so they should agree, but I
   did not test the tricky cases (Turkish dotted I, final sigma, Cherokee). In practice `roman` is
   ASCII letters from `likhi_ime.py:307`'s `ch.isalpha()` on layout-derived characters, so the
   exposure is small.

5. **Behaviour under a negative `index`.** `min(index, 9)` does not clamp below, so `pos_-3` is a
   reachable key shape. No caller produces one today; I did not find a validation layer that
   would reject it. Decide deliberately whether the Rust port uses a signed type (matching) or an
   unsigned one (diverging on malformed input).

6. **Whether any deployed collector depends on the capitalised HTTP header names** produced by
   urllib (`X-likhi-install` etc.). The self-hosted server does not. A Cloudflare Tunnel, Caddy or
   nginx configuration in front of a production endpoint could, in principle, match
   case-sensitively. I could not inspect the live deployment.

7. **Whether `note()` will gain callers.** It has none today. If the Rust port ships before any
   caller exists, its sanitisation bug (uppercase deleted, not lowercased — §8.2) could be fixed
   without breaking any existing data. That is a product decision, not a porting one; I have
   specified the current behaviour.

---

## 16. Port checklist

Ordered by how badly a mistake hurts, worst first. Every item is verified above.

1. **CRLF line terminators** in `metrics.jsonl` and `events.jsonl`, and therefore in every shipped
   chunk and in every byte offset. (§6.1)
2. **Counter key insertion order** in the metrics row — `IndexMap`, not `HashMap`/`BTreeMap`; and
   a read of a counter must not insert it. (§6.3)
3. **JSON separators `", "` and `": "`** — a custom `serde_json` formatter or hand-built lines.
   (§6.2)
4. **Local time** hour buckets, `%Y-%m-%dT%H`. (§5)
5. **`round(x, 1)` half-to-even** for `lat_p50`/`lat_p95`, and keep them floats so they serialise
   with a decimal point. (§10, §6.5)
6. **Percentile indices** `n // 2` and `min(n - 1, int(n * 0.95))` — verbatim. (§10)
7. **`\d` must be Unicode-aware** so Bangla numerals are rejected. (§9.1)
8. **Commit ordering**: guard → hour rollover → `words` → `pos_N` → `top1_taken` → `retyped` →
   `app_*` → latency → event → flush check. This order *is* the serialised key order. (§8.1)
9. **Struggle predicate** including the `(not top1 or ...)` short-circuit and the raw (unclamped)
   `pos` in the event row. (§8.4, §7.2)
10. **`retyped` is an int in metrics and a bool in events.** (§7.1)
11. **Chunk naming** `{stream}-{seq:05d}.jsonl`, `.part` **appended** then atomic rename. (§4.3)
12. **One offset per stream, sequence not reset on truncation, state written once at the end of
    `sync()`.** (§4.2, §12.3, §12.4)
13. **Ad-hoc destinations must not record progress**, with case-insensitive path comparison on
    Windows. (§12.1)
14. **Drain the whole backlog in one `sync()` call** — loop, do not send one chunk. (§12.3)
15. **Swallow every exception** on `commit`, `note`, `flush`, and per-iteration inside `sync`.
    (§11)
16. **`off` mode still creates the directory and `install_id`.** (§3.2)
17. **`X-Likhi-Key` omitted entirely when no key is configured**; the other five headers always
    sent. (§13)
18. **No rotation, no size cap, no `fsync`.** (§4.2)

---

## 17. Golden test vectors

Produced by running the real module (`install_id` forced to `3f9a12c7d4e5b601`). Use these as
byte-level fixtures for the Rust port.

Input sequence, `mode="full"`, one hour bucket, all in one flush:

```python
commit("amar", "আমার", index=0, app="chrome.exe", latency_ms=12.0)
commit("amr",  "আমরা", index=2, top1="আমার", app="WINWORD.EXE", latency_ms=31.5, retyped=True)
note("pipe_fallback")
flush()
```

`metrics.jsonl` (226 bytes, exact):

```
{"h": "2026-09-18T05", "id": "3f9a12c7d4e5b601", "words": 2, "pos_0": 1, "top1_taken": 1, "app_chrome.exe": 1, "pos_2": 1, "retyped": 1, "app_WINWORD.EXE": 1, "pipe_fallback": 1, "lat_p50": 31.5, "lat_p95": 31.5, "lat_n": 2}<CR><LF>
```

`events.jsonl` (119 bytes, exact — only the second commit is a struggle):

```
{"h": "2026-09-18T05", "roman": "amr", "chose": "আমরা", "we_said": "আমার", "pos": 2, "retyped": true}<CR><LF>
```

`sync_state.json` after one successful sync to a drop folder (no trailing newline):

```
{"metrics.jsonl": 226, "metrics.jsonl.seq": 1, "events.jsonl": 119, "events.jsonl.seq": 1}
```

Drop folder tree, with chunk contents byte-identical to the local files above:

```
<drop>\3f9a12c7d4e5b601\metrics-00001.jsonl
<drop>\3f9a12c7d4e5b601\events-00001.jsonl
```

Note `lat_p50 == lat_p95 == 31.5` for samples `[12.0, 31.5]`: `n = 2`, p50 index `2 // 2 = 1`,
p95 index `min(1, int(1.9)) = 1`. Both point at the larger value. That is correct behaviour, and
it is exactly the kind of thing a port "fixing" the median would break.

Additional verified vectors:

- Empty destination: `sync()` on an instance with neither drop nor endpoint returns
  `{'sent': 0, 'error': 'no drop folder or endpoint configured'}`.
- Partial line: appending `{"h": "partial"` (no terminator) to `events.jsonl` then syncing returns
  `{'sent': 0, 'drop': '<path>'}` and leaves `sync_state.json` byte-identical.
- Truncation: deleting `events.jsonl` and writing one new 123-byte line moved state from
  `{"events.jsonl": 120, "events.jsonl.seq": 1}` to `{"events.jsonl": 123, "events.jsonl.seq": 2}`
  and published `events-00002.jsonl`.
- Backlog: ~285 KB appended to `metrics.jsonl`, one `sync()` call → two chunks of 262119 and
  23500 bytes, `{'sent': 6077}`.
