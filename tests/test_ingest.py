"""Integration tests for the HTTPS telemetry path against a real ingest server."""

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from likhi.telemetry import Telemetry

REPO = Path(__file__).resolve().parents[1]
SERVER = REPO / "server" / "ingest_server.py"
KEY = "test-secret"


def _free_port() -> int:
    import socket

    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


ADMIN_KEY = "test-admin"


@pytest.fixture
def ingest(tmp_path):
    data = tmp_path / "server-data"
    port = _free_port()
    proc = subprocess.Popen(
        [
            sys.executable,
            str(SERVER),
            "--data",
            str(data),
            "--key",
            KEY,
            "--admin-key",
            ADMIN_KEY,
            "--port",
            str(port),
            "--host",
            "127.0.0.1",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    url = f"http://127.0.0.1:{port}"
    for _ in range(100):
        try:
            with urllib.request.urlopen(url + "/v1/health", timeout=1) as r:
                if r.status == 200:
                    break
        except Exception:
            time.sleep(0.1)
    else:
        proc.kill()
        pytest.fail("ingest server did not start")
    yield url, data
    proc.terminate()
    proc.wait(timeout=10)


def _post(url, body=b'{"a":1}\n', **headers):
    hdrs = {
        "X-Likhi-Install": "abc123def456",
        "X-Likhi-Stream": "events",
        "X-Likhi-Seq": "1",
        "X-Likhi-Key": KEY,
    }
    hdrs.update(headers)
    req = urllib.request.Request(url + "/v1/ingest", data=body, method="POST", headers=hdrs)
    return urllib.request.urlopen(req, timeout=5)


def test_client_ships_and_server_stores_same_layout(ingest):
    url, data = ingest
    local = data.parent / "client"
    t = Telemetry("full", directory=local, endpoint=url + "/v1/ingest", key=KEY)
    t.commit("amr", "আমরা", index=2, top1="আমার")
    t.commit("tmi", "তুমি", index=1, top1="তোমার")
    t.flush()
    stored = sorted((data / t.install_id).glob("events-*.jsonl"))
    assert len(stored) == 1
    rows = [json.loads(x) for x in stored[0].read_text(encoding="utf-8").splitlines()]
    assert [r["roman"] for r in rows] == ["amr", "tmi"]


def test_nothing_is_resent_and_new_lines_go_in_a_new_chunk(ingest):
    url, data = ingest
    local = data.parent / "client"
    t = Telemetry("full", directory=local, endpoint=url + "/v1/ingest", key=KEY)
    t.commit("amr", "আমরা", index=2, top1="আমার")
    t.flush()
    t.sync()  # nothing new
    assert len(sorted((data / t.install_id).glob("events-*.jsonl"))) == 1
    t.commit("ki", "কী", index=1, top1="কি")
    t.flush()
    chunks = sorted((data / t.install_id).glob("events-*.jsonl"))
    assert len(chunks) == 2
    assert len(chunks[1].read_text(encoding="utf-8").strip().splitlines()) == 1


def test_offsets_do_not_advance_when_the_endpoint_is_down(tmp_path):
    local = tmp_path / "client"
    t = Telemetry("full", directory=local, endpoint="http://127.0.0.1:9/v1/ingest")
    t.commit("amr", "আমরা", index=2, top1="আমার")
    t.flush()
    state = json.loads((local / "sync_state.json").read_text(encoding="utf-8"))
    assert state.get("events.jsonl", 0) == 0  # unsent, so it will be retried


def test_bad_key_is_rejected(ingest):
    url, _ = ingest
    with pytest.raises(urllib.error.HTTPError) as e:
        _post(url, **{"X-Likhi-Key": "wrong"})
    assert e.value.code == 401


def test_path_traversal_in_headers_is_rejected(ingest):
    url, data = ingest
    for bad in ("../../etc", "a/b", "..", "abc123$%^"):
        with pytest.raises(urllib.error.HTTPError) as e:
            _post(url, **{"X-Likhi-Install": bad})
        assert e.value.code == 400
    for bad in ("../metrics", "passwords", "events/../x"):
        with pytest.raises(urllib.error.HTTPError) as e:
            _post(url, **{"X-Likhi-Stream": bad})
        assert e.value.code == 400
    assert list(data.rglob("*.jsonl")) == []


def test_non_json_body_is_rejected(ingest):
    url, _ = ingest
    with pytest.raises(urllib.error.HTTPError) as e:
        _post(url, body=b"not json at all\n")
    assert e.value.code == 400


def test_health_paths(ingest):
    url, _ = ingest
    for path in ("/v1/health", "/healthz"):
        with urllib.request.urlopen(url + path, timeout=5) as r:
            assert r.status == 200 and json.loads(r.read())["ok"] is True
    with pytest.raises(urllib.error.HTTPError) as e:
        urllib.request.urlopen(url + "/", timeout=5)
    assert e.value.code == 404


def _export(url, admin=ADMIN_KEY, query=""):
    req = urllib.request.Request(url + "/v1/export" + query, headers={"X-Likhi-Admin-Key": admin})
    return urllib.request.urlopen(req, timeout=10)


def test_export_needs_the_admin_key_not_the_ingest_key(ingest):
    url, _ = ingest
    _post(url)
    for bad in (KEY, "", "guess"):
        with pytest.raises(urllib.error.HTTPError) as e:
            _export(url, admin=bad)
        assert e.value.code == 401


def test_export_returns_every_line_tagged_with_its_install(ingest):
    url, _ = ingest
    _post(url, body=b'{"h":"2026-09-16T10","roman":"amr"}\n', **{"X-Likhi-Install": "aaaa1111bbbb"})
    _post(url, body=b'{"h":"2026-09-16T11","roman":"tmi"}\n', **{"X-Likhi-Install": "cccc2222dddd"})
    rows = [json.loads(x) for x in _export(url).read().decode("utf-8").splitlines()]
    assert {r["_install"] for r in rows} == {"aaaa1111bbbb", "cccc2222dddd"}
    assert {r["roman"] for r in rows} == {"amr", "tmi"}


def test_export_since_filters_by_hour(ingest):
    url, _ = ingest
    _post(url, body=b'{"h":"2026-09-16T10","roman":"old"}\n{"h":"2026-09-16T20","roman":"new"}\n')
    rows = [
        json.loads(x)
        for x in _export(url, query="?since=2026-09-16T15").read().decode().splitlines()
    ]
    assert [r["roman"] for r in rows] == ["new"]


def test_export_is_disabled_without_an_admin_key(tmp_path):
    port = _free_port()
    proc = subprocess.Popen(
        [
            sys.executable,
            str(SERVER),
            "--data",
            str(tmp_path / "d"),
            "--key",
            KEY,
            "--port",
            str(port),
            "--host",
            "127.0.0.1",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    url = f"http://127.0.0.1:{port}"
    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(url + "/v1/health", timeout=1)
                break
            except Exception:
                time.sleep(0.1)
        with pytest.raises(urllib.error.HTTPError) as e:
            _export(url)
        assert e.value.code == 404
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def test_duplicate_chunk_is_accepted_once(ingest):
    url, data = ingest
    assert json.loads(_post(url).read())["stored"] == 1
    again = json.loads(_post(url).read())
    assert again["ok"] is True and again["stored"] == 0 and again["duplicate"] is True
    assert len(list((data / "abc123def456").glob("events-*.jsonl"))) == 1
