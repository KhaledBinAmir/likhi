"""Likhi engine server: one warm engine process, queried over a local TCP socket.

Why a server: the Windows text service (PIME) runs its own Python 3.8 and loads inside every
application; keeping the engine in a separate long-lived process means one model in memory, no
per-app start-up cost, and a crash there never takes an application down.

Protocol: JSON lines over 127.0.0.1:<port> (default 47123), one request per line:
    {"op": "suggest", "roman": "amr", "context": ["আমি"], "k": 5}
    -> {"ok": true, "candidates": ["আমার", ...], "ms": 12.3}
    {"op": "ping"} -> {"ok": true, "version": "0.0.1"}
    {"op": "learn", "roman": "amr", "chosen": "আমার", "context": [...]}  (Stage 2; acknowledged now)

Run:  likhi-server [--port 47123]
"""

from __future__ import annotations

import argparse
import json
import socketserver
import sys
import threading
import time

from likhi import __version__

DEFAULT_PORT = 47123


class _Handler(socketserver.StreamRequestHandler):
    def handle(self) -> None:  # one connection may send many lines
        engine = self.server.engine  # type: ignore[attr-defined]
        lock = self.server.lock  # type: ignore[attr-defined]
        for raw in self.rfile:
            try:
                req = json.loads(raw.decode("utf-8"))
                op = req.get("op", "suggest")
                if op == "ping":
                    resp = {"ok": True, "version": __version__}
                elif op == "suggest":
                    t = time.perf_counter()
                    with lock:
                        cands = engine.suggest(
                            req.get("roman", ""),
                            tuple(req.get("context", ())),
                            int(req.get("k", 5)),
                        )
                    resp = {
                        "ok": True,
                        "candidates": cands,
                        "ms": round((time.perf_counter() - t) * 1000, 2),
                    }
                elif op == "learn":
                    learn = getattr(engine, "learn", None)
                    if learn is not None:
                        with lock:
                            learn(
                                req.get("roman", ""),
                                req.get("chosen", ""),
                                tuple(req.get("context", ())),
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
    engine = LikhiEngine()
    engine.suggest("ami")  # warm up caches and BLAS
    print(f"[likhi-server] engine ready in {(time.perf_counter() - t0) * 1000:.0f} ms", flush=True)
    with _Server((host, port), _Handler) as srv:
        srv.engine = engine  # type: ignore[attr-defined]
        srv.lock = threading.Lock()  # type: ignore[attr-defined]
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
