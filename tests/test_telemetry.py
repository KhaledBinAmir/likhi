import json

from likhi.telemetry import Telemetry, is_recordable


def test_redaction_rules():
    assert is_recordable("amar", "আমার")
    assert not is_recordable("pass123", "পাস")  # digits
    assert not is_recordable("me@x", "মি")  # address-like
    assert not is_recordable("a/b", "এ")  # path or url
    assert not is_recordable("x" * 40, "এক্স")  # over-long
    assert not is_recordable("", "আমার")


def test_off_mode_writes_nothing(tmp_path):
    t = Telemetry("off", directory=tmp_path)
    t.commit("amr", "আমরা", index=1, top1="আমার")
    t.flush()
    assert not (tmp_path / "events.jsonl").exists()
    assert not (tmp_path / "metrics.jsonl").exists()


def test_metrics_mode_counts_but_stores_no_text(tmp_path):
    t = Telemetry("metrics", directory=tmp_path)
    t.commit("amar", "আমার", index=0)
    t.commit("amr", "আমরা", index=1, top1="আমার")
    t.flush()
    assert not (tmp_path / "events.jsonl").exists()
    row = json.loads((tmp_path / "metrics.jsonl").read_text(encoding="utf-8").strip())
    assert row["words"] == 2 and row["top1_taken"] == 1 and row["pos_1"] == 1
    assert "amar" not in json.dumps(row) and "আমার" not in json.dumps(row)


def test_full_mode_records_only_struggles(tmp_path):
    t = Telemetry("full", directory=tmp_path)
    t.commit("amar", "আমার", index=0)  # accepted first: not recorded
    t.commit("amr", "আমরা", index=2, top1="আমার")  # struggle: recorded
    t.commit("pin1234", "পিন", index=1, top1="পিনা")  # digits: dropped
    t.flush()
    lines = [
        json.loads(x) for x in (tmp_path / "events.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert len(lines) == 1
    assert (
        lines[0]["roman"] == "amr" and lines[0]["chose"] == "আমরা" and lines[0]["we_said"] == "আমার"
    )


def test_secure_field_records_nothing(tmp_path):
    t = Telemetry("full", directory=tmp_path)
    t.commit("gopon", "গোপন", index=1, top1="গোপনে", secure=True)
    t.flush()
    assert not (tmp_path / "events.jsonl").exists()
    assert not (tmp_path / "metrics.jsonl").exists()


def test_sync_ships_only_new_bytes(tmp_path):
    local, drop = tmp_path / "local", tmp_path / "drop"
    t = Telemetry("full", directory=local, drop=drop)
    t.commit("amr", "আমরা", index=1, top1="আমার")
    t.flush()
    out = drop / t.install_id
    first = sorted(p.name for p in out.glob("events-*.jsonl"))
    assert len(first) == 1
    # nothing new: no second chunk
    t.sync()
    assert sorted(p.name for p in out.glob("events-*.jsonl")) == first
    # new line: exactly one more chunk, holding only that line
    t.commit("tmi", "তুমি", index=1, top1="তোমার")
    t.flush()
    chunks = sorted(out.glob("events-*.jsonl"))
    assert len(chunks) == 2
    assert len(chunks[1].read_text(encoding="utf-8").strip().splitlines()) == 1


def test_counters_are_written_without_a_clean_shutdown(tmp_path):
    from likhi.telemetry import FLUSH_EVERY

    t = Telemetry("metrics", directory=tmp_path)
    for _ in range(FLUSH_EVERY):
        t.commit("amar", "আমার", index=0)
    # no flush() call: a killed process must still leave the counters on disk
    rows = [
        json.loads(x) for x in (tmp_path / "metrics.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert sum(r.get("words", 0) for r in rows) == FLUSH_EVERY


def test_sync_survives_unreachable_drop(tmp_path):
    local = tmp_path / "local"
    t = Telemetry("full", directory=local, drop=tmp_path / "nul" / "x" / "y")
    t.commit("amr", "আমরা", index=1, top1="আমার")
    t.flush()  # must not raise
    assert (local / "events.jsonl").exists()
