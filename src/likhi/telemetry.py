"""Opt-in usage telemetry for pilot deployments.

Design rule: **never record what the user wrote.** Two streams, both local files the user owns:

* ``metrics``  — counters only, one row per hour: words committed, how often candidate 1 was taken,
  which candidate position was chosen, latency percentiles, per foreground application. Answers
  "is it working", contains no text at all.
* ``events``   — *struggle events* only: the user did not take the first suggestion. One row holds
  the typed roman string, the word they chose, and the word we wrongly ranked first. Words the user
  accepted first time are never recorded, because they teach us nothing.

Redaction happens before anything is written:
  * anything with a digit, '@', ':' or '/' is dropped (IDs, passwords, URLs, times, money)
  * strings longer than MAX_LEN are dropped (pasted or concatenated text)
  * the caller can mark a field as secure (password box) and nothing is recorded at all
  * no timestamps finer than the hour, no context words, no sentences, no user or machine name

Identity is a random install id generated once, so rows from one machine can be grouped without
naming anyone. ``likhi-report`` shows exactly what would be shared and copies it to a drop folder.

Modes (config ``telemetry``): "off" (default), "metrics" (counters only), "full" (counters +
struggle events).
"""

from __future__ import annotations

import json
import os
import re
import threading
import time
import uuid
from collections import Counter
from pathlib import Path

MAX_LEN = 32
_RE_UNSAFE = re.compile(r"[\d@:/\\]")
# Write a counter row every N committed words. Machines get shut down or killed without a clean
# exit, and TerminateProcess cannot be caught, so counters must not sit in memory for an hour.
# Rows carry their hour bucket, so several rows per hour simply sum at collection time.
FLUSH_EVERY = 20
MAX_CHUNK_BYTES = 256 * 1024  # cap one upload, so a machine offline for a week catches up in steps
HTTP_TIMEOUT_S = 10.0

DEFAULT_DIR = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Likhi"


def _hour_bucket(when: float | None = None) -> str:
    return time.strftime("%Y-%m-%dT%H", time.localtime(when if when is not None else time.time()))


def is_recordable(roman: str, word: str) -> bool:
    """True when this pair is safe to write: no digits, symbols, or over-long strings."""
    if not roman or not word:
        return False
    if len(roman) > MAX_LEN or len(word) > MAX_LEN:
        return False
    return not (_RE_UNSAFE.search(roman) or _RE_UNSAFE.search(word))


class Telemetry:
    """Thread-safe, best-effort. Any failure here must never affect typing."""

    def __init__(
        self,
        mode: str = "off",
        directory: Path | str | None = None,
        drop: Path | str | None = None,
        endpoint: str | None = None,
        key: str | None = None,
    ) -> None:
        """``drop``: a folder (LAN share, synced folder). ``endpoint``: an HTTPS ingest URL.
        Either, both, or neither. Both use the same offset bookkeeping, so a chunk is sent once."""
        self.mode = mode if mode in ("off", "metrics", "full") else "off"
        self.dir = Path(directory) if directory else DEFAULT_DIR
        self.drop = Path(drop) if drop else None
        self.endpoint = endpoint or None
        self.key = key or None
        self.lock = threading.Lock()
        self.counters: Counter[str] = Counter()
        self.latencies: list[float] = []
        self.bucket = _hour_bucket()
        self.install_id = self._install_id()

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

    # ------------------------------------------------------------------ recording

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
        """One committed word. ``index`` is the candidate position taken (0 = first suggestion)."""
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
                if self.counters["words"] >= FLUSH_EVERY:
                    # Local write only. Syncing to the drop folder is network I/O and stays on the
                    # background timer, never on the path of a committed keystroke.
                    self._flush_locked()
        except Exception:
            pass  # telemetry must never break typing

    def note(self, counter: str) -> None:
        if self.mode == "off":
            return
        try:
            with self.lock:
                self.counters[re.sub(r"[^a-z0-9_]", "", counter)[:32]] += 1
        except Exception:
            pass

    # ------------------------------------------------------------------ output

    def _write(self, name: str, row: dict) -> None:
        self.dir.mkdir(parents=True, exist_ok=True)
        with open(self.dir / name, "a", encoding="utf-8") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

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

    # ------------------------------------------------------------------ shipping

    def _send_folder(self, target: Path, stream: str, seq: int, chunk: bytes) -> None:
        out_dir = target / self.install_id
        out_dir.mkdir(parents=True, exist_ok=True)
        name = f"{stream.split('.')[0]}-{seq:05d}.jsonl"
        tmp = out_dir / (name + ".part")
        tmp.write_bytes(chunk)
        tmp.replace(out_dir / name)  # atomic publish: readers never see a half file

    def _send_http(self, url: str, stream: str, seq: int, chunk: bytes) -> None:
        """POST one chunk as newline-delimited JSON. Raises on any non-2xx or transport error.

        The install id, stream and sequence number go in headers so the receiver can store chunks
        under the same layout as the folder drop and ignore a duplicate sequence number, which is
        what makes a retry after a half-finished request safe.
        """
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

    def sync(self, drop: Path | str | None = None, endpoint: str | None = None) -> dict:
        """Ship new lines to the configured destinations.

        Only the bytes written since the last successful sync are sent, as immutable numbered
        chunks. Immutable chunks mean no append races between machines, no locking, and a retry
        can never duplicate or corrupt anything: if a destination is unreachable the offset is not
        advanced and the next attempt sends exactly the same bytes under the same sequence number.

        Runs on a background thread only. Never call it from a keystroke path.
        """
        target = Path(drop) if drop else self.drop
        url = endpoint or self.endpoint
        if not target and not url:
            return {"sent": 0, "error": "no drop folder or endpoint configured"}
        state_path = self.dir / "sync_state.json"
        try:
            state = (
                json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}
            )
        except Exception:
            state = {}
        sent = 0
        errors: list[str] = []
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
        try:
            state_path.write_text(json.dumps(state), encoding="utf-8")
        except Exception:
            pass
        out: dict = {"sent": sent}
        if target:
            out["drop"] = str(target)
        if url:
            out["endpoint"] = url
        if errors:
            out["error"] = "; ".join(errors)
        return out
