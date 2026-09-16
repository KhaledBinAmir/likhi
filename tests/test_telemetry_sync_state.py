"""Shipping telemetry somewhere ad hoc must not cost the real destination its data.

sync_state.json holds one offset per stream rather than one per destination, because the configured
destinations are always shipped together. That makes a one-off sync elsewhere -- a debug folder, a
second endpoint -- destructive unless it refuses to record progress: the offset would advance and
those bytes would never reach the destination the pilot actually reads.

Found the hard way. A test that shipped to a temporary folder consumed part of a live install's
telemetry, and the developer machine's own words never reached the collector.
"""

import json

from likhi.telemetry import Telemetry


def _make(tmp_path, **kw):
    """Local files written, nothing shipped yet, so sync_state starts empty.

    The destination is attached after the flush on purpose: constructing with one makes flush()
    ship immediately, and then every assertion below would be comparing against an already-advanced
    offset rather than a clean slate.
    """
    t = Telemetry(mode="full", directory=tmp_path / "state")
    # index=1: a struggle word is only written when the first suggestion was not the one taken,
    # which is the only time the text of what someone typed is recorded at all.
    t.commit("amr", "আমার", index=1, top1="আমি")
    t.flush()
    for name, value in kw.items():
        setattr(t, name, value)
    return t


def _state(t):
    p = t.dir / "sync_state.json"
    return json.loads(p.read_text(encoding="utf-8")) if p.exists() else {}


def test_configured_sync_records_progress(tmp_path):
    drop = tmp_path / "drop"
    t = _make(tmp_path, drop=drop)
    result = t.sync()
    assert result.get("ad_hoc") is not True
    assert _state(t).get("events.jsonl", 0) > 0
    # A second round has nothing left to send.
    assert t.sync()["sent"] == 0


def test_ad_hoc_drop_does_not_consume_the_configured_destination(tmp_path):
    drop = tmp_path / "real"
    t = _make(tmp_path, drop=drop)
    elsewhere = tmp_path / "elsewhere"

    result = t.sync(drop=elsewhere)
    assert result["ad_hoc"] is True
    assert result["sent"] > 0
    assert any(elsewhere.rglob("*.jsonl")), "the ad-hoc copy should still be written"
    assert _state(t) == {}, "an ad-hoc sync must not record progress"

    # The real destination still receives everything.
    assert t.sync()["sent"] > 0
    assert any(drop.rglob("*.jsonl"))
    assert _state(t).get("events.jsonl", 0) > 0


def test_ad_hoc_endpoint_does_not_consume_progress(tmp_path):
    drop = tmp_path / "real"
    t = _make(tmp_path, drop=drop)
    # Unreachable on purpose: the point is that the offset is untouched either way.
    t.sync(endpoint="https://127.0.0.1:9/v1/ingest")
    assert _state(t) == {}
    assert t.sync()["sent"] > 0


def test_same_destination_passed_explicitly_is_not_ad_hoc(tmp_path):
    drop = tmp_path / "real"
    t = _make(tmp_path, drop=drop)
    result = t.sync(drop=drop)
    assert result.get("ad_hoc") is not True
    assert _state(t).get("events.jsonl", 0) > 0
