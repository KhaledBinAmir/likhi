"""Likhi telemetry ingest: a self-hostable HTTPS endpoint, Python standard library only.

Run it on any always-on machine. It stores exactly the same chunk layout the folder drop produces,
so ``likhi-report collect --drop <data dir>`` works against it unchanged:

    <data>/<install_id>/metrics-00001.jsonl
    <data>/<install_id>/events-00001.jsonl

Usage:
    python server/ingest_server.py --data D:\\likhi-telemetry --key SECRET --port 8098
    python server/ingest_server.py --data ... --key ... --certfile cert.pem --keyfile key.pem
    python server/ingest_server.py --gcs my-bucket --key SECRET        # Cloud Run + Cloud Storage

With --gcs the same object layout is written to a Cloud Storage bucket, so `gsutil -m cp -r
gs://my-bucket/* ./pilot` followed by `likhi-report collect --drop ./pilot` works unchanged.

Put it behind a reverse proxy (Caddy, nginx, Cloudflare Tunnel) for a real certificate, or pass
--certfile/--keyfile to serve TLS directly. Clients set:

    "telemetry_endpoint": "https://example.org/v1/ingest",
    "telemetry_key": "SECRET"

Endpoints:
    POST /v1/ingest   body = newline-delimited JSON, headers X-Likhi-Install / -Stream / -Seq / -Key
    GET  /v1/health   liveness, no auth (also /healthz when self-hosted; Cloud Run reserves that one)
    GET  /v1/export   read everything back as NDJSON, header X-Likhi-Admin-Key

Two separate keys on purpose. The ingest key ships inside the client, so anyone holding the client
can extract it; it can only append chunks and can never read anything. The admin key is never
distributed and is the only way to read the collected data. Export is disabled unless an admin key
is set.

Safety properties that matter here:
  * the install id and stream name are validated against strict patterns before touching a path,
    so a hostile header can never escape the data directory
  * a repeated sequence number is accepted and ignored, which makes a client retry after a
    half-finished request harmless
  * bodies are capped, and anything that is not newline-delimited JSON is rejected
  * client IP addresses are not recorded
"""

from __future__ import annotations

import argparse
import json
import os
import re
import ssl
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs

MAX_BODY = 1024 * 1024
MAX_EXPORT = 64 * 1024 * 1024
RE_INSTALL = re.compile(r"^[0-9a-f]{8,32}$")
RE_STREAM = re.compile(r"^(metrics|events)$")
RE_SEQ = re.compile(r"^[0-9]{1,9}$")


class Storage:
    """Where chunks land. Local disk by default; Cloud Storage when a bucket is given."""

    def __init__(self, data_dir: Path | None = None, bucket: str | None = None) -> None:
        self.data_dir = data_dir
        self.bucket_name = bucket
        self._bucket = None
        if bucket:
            from google.cloud import storage  # lazy: only the cloud deployment needs this

            self._bucket = storage.Client().bucket(bucket)

    def exists(self, name: str) -> bool:
        if self._bucket is not None:
            return self._bucket.blob(name).exists()
        return (self.data_dir / name).exists()

    def iter_chunks(self, stream: str | None = None):
        """Yield (install_id, object_name, bytes) for every stored chunk, oldest name first."""
        if self._bucket is not None:
            for blob in sorted(self._bucket.list_blobs(), key=lambda b: b.name):
                install, _, base = blob.name.partition("/")
                if base and (stream is None or base.startswith(stream + "-")):
                    yield install, base, blob.download_as_bytes()
            return
        for path in sorted(self.data_dir.glob("*/*.jsonl")):
            if stream is None or path.name.startswith(stream + "-"):
                yield path.parent.name, path.name, path.read_bytes()

    def put(self, name: str, body: bytes) -> None:
        if self._bucket is not None:
            # if_generation_match=0 fails if the object already exists, so two concurrent
            # deliveries of the same chunk cannot overwrite each other
            self._bucket.blob(name).upload_from_string(
                body, content_type="application/x-ndjson", if_generation_match=0
            )
            return
        path = self.data_dir / name
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".part")
        tmp.write_bytes(body)
        tmp.replace(path)

    def describe(self) -> str:
        return f"gs://{self.bucket_name}" if self._bucket is not None else str(self.data_dir)


class Handler(BaseHTTPRequestHandler):
    server_version = "likhi-ingest/1"
    storage: Storage
    shared_key: str | None
    admin_key: str | None = None

    def log_message(self, fmt: str, *args) -> None:  # no IP addresses in logs
        sys.stderr.write("[ingest] " + (fmt % args).replace(self.address_string(), "-") + "\n")

    def _reply(self, code: int, body: str = "") -> None:
        payload = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        if payload:
            self.wfile.write(payload)

    def do_GET(self) -> None:  # noqa: N802 (stdlib naming)
        # /healthz is answered by Google's frontend on Cloud Run and never reaches the container
        # (measured 2026-09-16: it returns a 1568-byte HTML 404 while every other path arrives
        # here), so the real health path is namespaced. /healthz still works when self-hosted.
        path, _, query = self.path.partition("?")
        if path in ("/v1/health", "/healthz"):
            return self._reply(200, '{"ok":true}')
        if path == "/v1/export":
            return self._export(query)
        self._reply(404, '{"error":"not found"}')

    def _export(self, query: str) -> None:
        """Every stored line as NDJSON, each with the install id it came from.

        Disabled unless an admin key is configured, and that key is never shipped to clients.
        """
        if not self.admin_key:
            return self._reply(404, '{"error":"export not enabled"}')
        if self.headers.get("X-Likhi-Admin-Key") != self.admin_key:
            return self._reply(401, '{"error":"bad admin key"}')
        params = parse_qs(query)
        stream = (params.get("stream") or [None])[0]
        if stream is not None and not RE_STREAM.match(stream):
            return self._reply(400, '{"error":"bad stream"}')
        since = (params.get("since") or [""])[0]  # hour bucket, e.g. 2026-09-16T00

        out = bytearray()
        try:
            for install, name, body in self.storage.iter_chunks(stream):
                for line in body.decode("utf-8", "replace").splitlines():
                    if not line.strip():
                        continue
                    try:
                        row = json.loads(line)
                    except Exception:
                        continue
                    if since and str(row.get("h", "")) < since:
                        continue
                    row["_install"] = install
                    row["_chunk"] = name
                    out += json.dumps(row, ensure_ascii=False).encode("utf-8") + b"\n"
                    if len(out) > MAX_EXPORT:
                        out += b'{"_truncated":true}\n'
                        raise StopIteration
        except StopIteration:
            pass
        except Exception as e:
            self.log_message("export failed: %s", type(e).__name__)
            return self._reply(503, '{"error":"export failed"}')
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson; charset=utf-8")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(bytes(out))

    def do_POST(self) -> None:  # noqa: N802
        if self.path.split("?")[0] != "/v1/ingest":
            return self._reply(404, '{"error":"not found"}')
        if self.shared_key and self.headers.get("X-Likhi-Key") != self.shared_key:
            return self._reply(401, '{"error":"bad key"}')

        install = (self.headers.get("X-Likhi-Install") or "").strip()
        stream = (self.headers.get("X-Likhi-Stream") or "").strip()
        seq = (self.headers.get("X-Likhi-Seq") or "").strip()
        if not (RE_INSTALL.match(install) and RE_STREAM.match(stream) and RE_SEQ.match(seq)):
            return self._reply(400, '{"error":"bad headers"}')

        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            return self._reply(400, '{"error":"bad length"}')
        if length <= 0 or length > MAX_BODY:
            return self._reply(413, '{"error":"empty or too large"}')
        body = self.rfile.read(length)

        lines = [ln for ln in body.decode("utf-8", "replace").splitlines() if ln.strip()]
        try:
            for ln in lines:
                if not isinstance(json.loads(ln), dict):
                    raise ValueError("not an object")
        except Exception:
            return self._reply(400, '{"error":"body must be newline-delimited JSON objects"}')

        name = f"{install}/{stream}-{int(seq):05d}.jsonl"
        try:
            if self.storage.exists(name):  # duplicate delivery of a chunk we already stored
                return self._reply(200, json.dumps({"ok": True, "stored": 0, "duplicate": True}))
            self.storage.put(name, body)
        except Exception as e:
            if "conditionNotMet" in str(e) or "412" in str(e):  # lost a concurrent race: fine
                return self._reply(200, json.dumps({"ok": True, "stored": 0, "duplicate": True}))
            self.log_message("store failed: %s", type(e).__name__)
            return self._reply(503, '{"error":"store failed"}')
        self._reply(200, json.dumps({"ok": True, "stored": len(lines)}))


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--data", help="directory to store chunks in")
    ap.add_argument("--gcs", help="Cloud Storage bucket name (instead of --data)")
    ap.add_argument("--host", default="0.0.0.0")
    ap.add_argument("--port", type=int, default=int(os.environ.get("PORT") or 8098))
    ap.add_argument("--key", default=os.environ.get("LIKHI_INGEST_KEY"), help="client write key")
    ap.add_argument(
        "--admin-key",
        default=os.environ.get("LIKHI_ADMIN_KEY"),
        help="key for GET /v1/export; never ship this to clients. Export is off when unset.",
    )
    ap.add_argument("--certfile", help="PEM certificate to serve TLS directly")
    ap.add_argument("--keyfile", help="PEM private key")
    args = ap.parse_args(argv)

    bucket = args.gcs or os.environ.get("LIKHI_INGEST_BUCKET")
    if not bucket and not args.data:
        ap.error("one of --data or --gcs is required")
    data_dir = Path(args.data) if args.data and not bucket else None
    if data_dir:
        data_dir.mkdir(parents=True, exist_ok=True)
    Handler.storage = Storage(data_dir, bucket)
    Handler.shared_key = args.key
    Handler.admin_key = args.admin_key
    if not args.key:
        print("[ingest] WARNING: no key set, anyone who can reach this port can post", flush=True)
    print(f"[ingest] export endpoint: {'enabled' if args.admin_key else 'disabled'}", flush=True)

    httpd = ThreadingHTTPServer((args.host, args.port), Handler)
    scheme = "http"
    if args.certfile:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(args.certfile, args.keyfile)
        httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
        scheme = "https"
    print(
        f"[ingest] {scheme}://{args.host}:{args.port}/v1/ingest -> {Handler.storage.describe()}",
        flush=True,
    )
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
