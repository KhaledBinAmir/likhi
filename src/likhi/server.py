"""Likhi engine server: one warm engine process, queried over a local TCP socket.

Why a server: the Windows text service (PIME) runs its own Python 3.8 and loads inside every
application; keeping the engine in a separate long-lived process means one model in memory, no
per-app start-up cost, and a crash there never takes an application down.

Latency design: every request is answered within ``deadline_ms``. The trie/rule channels answer
in a few milliseconds; the transliteration model (tens of milliseconds in NumPy) runs on a worker
thread. If it finishes inside the deadline the full ranking is returned, otherwise the fast
ranking is returned with ``"partial": true`` and the full result is cached for the next request
(the shell re-asks with a longer deadline when the user commits, so commits always use the full
ranking). Typing therefore never waits on the model.

Protocol: JSON lines over 127.0.0.1:<port> (default 47123), one request per line:
    {"op": "suggest", "roman": "amr", "context": ["আমি"], "k": 5, "deadline_ms": 12}
    -> {"ok": true, "candidates": ["আমার", ...], "partial": false, "ms": 3.1}
    {"op": "ping"} -> {"ok": true, "version": "0.0.1"}
    {"op": "learn", "roman": "amr", "chosen": "আমার", "context": [...]} -> {"ok": true}

Run:  likhi-server [--port 47123]
"""

from __future__ import annotations

import argparse
import json
import os
import socketserver
import sys
import threading
import time
from collections import OrderedDict
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path

from likhi import __version__

DEFAULT_PORT = 47123
DEFAULT_DEADLINE_MS = 12.0
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
WEAK_MATCH_DEADLINE_MS = float(os.environ.get("LIKHI_WEAK_WAIT_MS") or 0.0)
COMMIT_DEADLINE_MS = 400.0


class SuggestService:
    """Deadline-aware wrapper around the engine (thread-safe)."""

    def __init__(self, engine, cache_size: int = 2048) -> None:
        self.engine = engine
        # Only `learn` takes this lock. Reads are lock-free: NumPy releases the GIL, marisa lookups
        # and lru_cache are thread-safe, and the SQLite store allows cross-thread use. Holding a
        # lock around the model call would make the fast path wait for the model, defeating the
        # deadline (measured: 120 ms round trips instead of 12).
        self.lock = threading.Lock()
        self.pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="likhi-model")
        self.pending: dict[tuple, Future] = {}
        self.full_cache: OrderedDict[tuple, list[str]] = OrderedDict()
        self.cache_size = cache_size
        self.latest_key: tuple | None = None  # most recent input; older jobs may abort
        self.waiting: set[tuple] = set()  # keys a request is currently blocked on

    def _key(self, roman: str, context: tuple[str, ...], k: int) -> tuple:
        return (roman, context[-1:] if context else (), k)

    def _full(self, roman: str, context: tuple[str, ...], k: int) -> list[str]:
        key = self._key(roman, context, k)
        # Abandon this job between model stages if a newer input has superseded it, unless someone
        # is still waiting for exactly this key (a commit re-ask).
        return self.engine.suggest(
            roman, context, k, abort=lambda: self.latest_key != key and key not in self.waiting
        )

    def suggest(
        self, roman: str, context: tuple[str, ...], k: int, deadline_ms: float
    ) -> tuple[list[str], bool]:
        key = self._key(roman, context, k)
        self.latest_key = key
        cached = self.full_cache.get(key)
        if cached is not None:
            self.full_cache.move_to_end(key)
            return cached, False
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
                return fut.result(timeout=max(0.0, deadline_ms) / 1000.0), False
            except Exception:
                pass  # timeout (or a model error / abort): fall back to the fast path
            fast, strong = self.engine.fast_suggest(roman, context, k)
            if not strong:
                # The trie channels have nothing convincing (typically an English loanword or a
                # name): a wrong-looking flash is worse than a slightly later answer, so wait.
                try:
                    return fut.result(timeout=WEAK_MATCH_DEADLINE_MS / 1000.0), False
                except Exception:
                    pass
        finally:
            self.waiting.discard(key)
        return fast, True

    def _done(self, key: tuple, fut: Future) -> None:
        self.pending.pop(key, None)
        if not fut.cancelled() and fut.exception() is None:  # aborted jobs raise, so are skipped
            self.full_cache[key] = fut.result()
            while len(self.full_cache) > self.cache_size:
                self.full_cache.popitem(last=False)

    def learn(self, roman: str, chosen: str, context: tuple[str, ...]) -> None:
        with self.lock:
            self.engine.learn(roman, chosen, context)
        self.full_cache.clear()

    def top_candidate(self, roman: str, context: tuple[str, ...], k: int = 5) -> str:
        """What we would have shown first, for telemetry. Cache only, never computes."""
        cached = self.full_cache.get(self._key(roman, context, k))
        return cached[0] if cached else ""


class _Handler(socketserver.StreamRequestHandler):
    def handle(self) -> None:  # one connection may send many lines
        svc: SuggestService = self.server.service  # type: ignore[attr-defined]
        for raw in self.rfile:
            try:
                req = json.loads(raw.decode("utf-8"))
                op = req.get("op", "suggest")
                if op == "ping":
                    resp = {"ok": True, "version": __version__}
                elif op == "suggest":
                    t = time.perf_counter()
                    cands, partial = svc.suggest(
                        req.get("roman", ""),
                        tuple(req.get("context", ())),
                        int(req.get("k", 5)),
                        float(req.get("deadline_ms", DEFAULT_DEADLINE_MS)),
                    )
                    resp = {
                        "ok": True,
                        "candidates": cands,
                        "partial": partial,
                        "ms": round((time.perf_counter() - t) * 1000, 2),
                    }
                elif op == "learn":
                    roman = req.get("roman", "")
                    chosen = req.get("chosen", "")
                    context = tuple(req.get("context", ()))
                    svc.learn(roman, chosen, context)
                    tel = self.server.telemetry  # type: ignore[attr-defined]
                    if tel.mode != "off":
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
                else:
                    resp = {"ok": False, "error": f"unknown op {op!r}"}
            except Exception as e:  # never let one bad request kill the connection
                resp = {"ok": False, "error": f"{type(e).__name__}: {e}"}
            self.wfile.write((json.dumps(resp, ensure_ascii=False) + "\n").encode("utf-8"))
            self.wfile.flush()


class _Server(socketserver.ThreadingTCPServer):
    # Deliberately NOT allow_reuse_address on Windows: SO_REUSEADDR there lets a second process
    # bind a port another process is already listening on, so two engines would silently split the
    # keyboard's requests between them, each with a different personal dictionary. Binding must
    # fail instead, and `serve` turns that failure into a clear message.
    allow_reuse_address = sys.platform != "win32"
    daemon_threads = True


def already_running(port: int, host: str = "127.0.0.1") -> bool:
    """True when a Likhi engine is already answering on this port."""
    import socket

    try:
        with socket.create_connection((host, port), timeout=1.0) as s:
            s.sendall(b'{"op":"ping"}\n')
            return b'"ok"' in s.recv(256)
    except OSError:
        return False


def serve(port: int = DEFAULT_PORT, host: str = "127.0.0.1") -> None:
    if already_running(port, host):
        print(f"[likhi-server] an engine is already listening on {host}:{port}; nothing to do")
        return

    from likhi.engine.threads import limit_blas_threads

    # 4 BLAS threads: batched candidate scoring gets ~1.5x faster; the beam is unaffected.
    limit_blas_threads(4)
    from likhi.engine.core import LikhiEngine

    t0 = time.perf_counter()
    engine = LikhiEngine(
        personal_path="default",  # learning on: %LOCALAPPDATA%\Likhi\personal.sqlite
        # 10 non-beam candidates scored by the model: identical accuracy to 16 on every set
        # (chat 93.60 vs 93.60, Dakshina 69.18 vs 69.23) at 26% lower latency, because the beam's
        # own hypotheses now carry their scores for free.
        model_scored=10,
    )
    engine.suggest("ami")  # warm up caches and BLAS
    print(f"[likhi-server] engine ready in {(time.perf_counter() - t0) * 1000:.0f} ms", flush=True)
    from likhi.telemetry import Telemetry

    cfg = _telemetry_config()
    telemetry = Telemetry(cfg["mode"], drop=cfg["drop"], endpoint=cfg["endpoint"], key=cfg["key"])
    if cfg["mode"] != "off":
        where = ", ".join(x for x in (cfg["drop"], cfg["endpoint"]) if x) or "local only"
        print(
            f"[likhi-server] telemetry: {cfg['mode']} (files in {telemetry.dir}; ships to {where})",
            flush=True,
        )

    try:
        srv_ctx = _Server((host, port), _Handler)
    except OSError as e:
        print(f"[likhi-server] cannot listen on {host}:{port}: {e}")
        return
    with srv_ctx as srv:
        srv.service = SuggestService(engine)  # type: ignore[attr-defined]
        srv.telemetry = telemetry  # type: ignore[attr-defined]
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
        print(f"[likhi-server] listening on {host}:{port}", flush=True)
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            stop.set()
            telemetry.flush()


def _config_paths() -> list[Path]:
    """Every place the shell's config.json may live, most specific first.

    The packaged layout is <app>\\runtime\\python\\Lib\\site-packages\\likhi for this module and
    <app>\\pime\\python\\input_methods\\likhi\\config.json for the config, so a path relative to
    this file is what an installed engine actually needs; the launcher also sets LIKHI_CONFIG.
    Missing that was a silent failure: telemetry simply stayed off on every fresh install.
    """
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
        / "PIME"
        / "python"
        / "input_methods"
        / "likhi"
        / "config.json"
    )
    paths.append(Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Likhi" / "config.json")
    return paths


def _telemetry_config() -> dict:
    """mode (off/metrics/full), drop folder, HTTPS endpoint, shared key.

    Environment wins (LIKHI_TELEMETRY, LIKHI_TELEMETRY_DROP, LIKHI_TELEMETRY_ENDPOINT,
    LIKHI_TELEMETRY_KEY), else the shell's config.json.
    """
    env = os.environ.get("LIKHI_TELEMETRY")
    if env:
        return {
            "mode": env.strip().lower(),
            "drop": os.environ.get("LIKHI_TELEMETRY_DROP") or None,
            "endpoint": os.environ.get("LIKHI_TELEMETRY_ENDPOINT") or None,
            "key": os.environ.get("LIKHI_TELEMETRY_KEY") or None,
            "sync_seconds": float(os.environ.get("LIKHI_TELEMETRY_SYNC_S") or 900),
        }
    for path in _config_paths():
        try:
            if path.exists():
                # utf-8-sig: administrators edit this file, and Notepad writes a byte-order mark
                # that plain utf-8 parsing rejects, which would silently disable telemetry.
                cfg = json.loads(path.read_text(encoding="utf-8-sig"))
                return {
                    "mode": str(cfg.get("telemetry", "off")).lower(),
                    "drop": cfg.get("telemetry_drop") or None,
                    "endpoint": cfg.get("telemetry_endpoint") or None,
                    "key": cfg.get("telemetry_key") or None,
                    "sync_seconds": float(cfg.get("telemetry_sync_seconds") or 900),
                }
        except Exception:
            pass
    return {"mode": "off", "drop": None, "endpoint": None, "key": None, "sync_seconds": 900.0}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="likhi-server",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    args = ap.parse_args(argv)
    serve(args.port)
    return 0


if __name__ == "__main__":
    sys.exit(main())
