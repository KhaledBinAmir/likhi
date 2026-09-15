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
import socketserver
import sys
import threading
import time
from collections import OrderedDict
from concurrent.futures import Future, ThreadPoolExecutor

from likhi import __version__

DEFAULT_PORT = 47123
DEFAULT_DEADLINE_MS = 12.0
WEAK_MATCH_DEADLINE_MS = 60.0  # extra wait when the model-free answer has no strong evidence
COMMIT_DEADLINE_MS = 400.0


class SuggestService:
    """Deadline-aware wrapper around the engine (thread-safe)."""

    def __init__(self, engine, cache_size: int = 2048) -> None:
        self.engine = engine
        self.lock = (
            threading.Lock()
        )  # engine calls are serialized (NumPy + caches are not thread-safe)
        self.pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="likhi-model")
        self.pending: dict[tuple, Future] = {}
        self.full_cache: OrderedDict[tuple, list[str]] = OrderedDict()
        self.cache_size = cache_size

    def _key(self, roman: str, context: tuple[str, ...], k: int) -> tuple:
        return (roman, context[-1:] if context else (), k)

    def _full(self, roman: str, context: tuple[str, ...], k: int) -> list[str]:
        with self.lock:
            return self.engine.suggest(roman, context, k)

    def suggest(
        self, roman: str, context: tuple[str, ...], k: int, deadline_ms: float
    ) -> tuple[list[str], bool]:
        key = self._key(roman, context, k)
        cached = self.full_cache.get(key)
        if cached is not None:
            self.full_cache.move_to_end(key)
            return cached, False
        fut = self.pending.get(key)
        if fut is None:
            fut = self.pool.submit(self._full, roman, context, k)
            self.pending[key] = fut
            fut.add_done_callback(lambda f, key=key: self._done(key, f))
        try:
            full = fut.result(timeout=max(0.0, deadline_ms) / 1000.0)
            return full, False
        except Exception:
            pass  # timeout (or a model error): fall back to the fast path
        with self.lock:
            strong = self.engine.has_strong_match(roman)
        if not strong:
            # The trie channels have nothing convincing (typically an English loanword or a name):
            # a wrong-looking flash is worse than a slightly later answer, so wait a bit longer.
            try:
                return fut.result(timeout=WEAK_MATCH_DEADLINE_MS / 1000.0), False
            except Exception:
                pass
        with self.lock:
            fast = self.engine.suggest(roman, context, k, fast=True)
        return fast, True

    def _done(self, key: tuple, fut: Future) -> None:
        self.pending.pop(key, None)
        if fut.exception() is None:
            self.full_cache[key] = fut.result()
            while len(self.full_cache) > self.cache_size:
                self.full_cache.popitem(last=False)

    def learn(self, roman: str, chosen: str, context: tuple[str, ...]) -> None:
        with self.lock:
            self.engine.learn(roman, chosen, context)
        self.full_cache.clear()


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
                    svc.learn(
                        req.get("roman", ""), req.get("chosen", ""), tuple(req.get("context", ()))
                    )
                    resp = {"ok": True}
                else:
                    resp = {"ok": False, "error": f"unknown op {op!r}"}
            except Exception as e:  # never let one bad request kill the connection
                resp = {"ok": False, "error": f"{type(e).__name__}: {e}"}
            self.wfile.write((json.dumps(resp, ensure_ascii=False) + "\n").encode("utf-8"))
            self.wfile.flush()


class _Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def serve(port: int = DEFAULT_PORT, host: str = "127.0.0.1") -> None:
    from likhi.engine.threads import limit_blas_threads

    limit_blas_threads()
    from likhi.engine.core import LikhiEngine

    t0 = time.perf_counter()
    engine = LikhiEngine(
        personal_path="default"
    )  # learning on: %LOCALAPPDATA%\Likhi\personal.sqlite
    engine.suggest("ami")  # warm up caches and BLAS
    print(f"[likhi-server] engine ready in {(time.perf_counter() - t0) * 1000:.0f} ms", flush=True)
    with _Server((host, port), _Handler) as srv:
        srv.service = SuggestService(engine)  # type: ignore[attr-defined]
        print(f"[likhi-server] listening on {host}:{port}", flush=True)
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            pass


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
