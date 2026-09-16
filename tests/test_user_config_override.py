"""A person's own setting beats the machine default, without erasing the rest of it.

The Likhi window writes %LOCALAPPDATA%\\Likhi\\config.json when someone turns usage reporting off.
That file must win over the installed config, which lives under Program Files and needs an
administrator to edit, but it must only override the keys it actually sets: the endpoint and the
shared key are stamped into the installed config at build time, and a per-user file has no business
restating them. Turning reporting off and on again must not lose the destination.
"""

import json

import pytest

from likhi.server import _telemetry_config, _user_config_path


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch, tmp_path):
    for name in (
        "LIKHI_TELEMETRY",
        "LIKHI_TELEMETRY_DROP",
        "LIKHI_TELEMETRY_ENDPOINT",
        "LIKHI_TELEMETRY_KEY",
        "LIKHI_CONFIG",
    ):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "local"))
    monkeypatch.setenv("PROGRAMFILES(X86)", str(tmp_path / "pf86"))


def _machine_config(tmp_path, **extra):
    p = tmp_path / "machine" / "config.json"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(
        json.dumps(
            {
                "telemetry": "full",
                "telemetry_endpoint": "https://ingest.example/v1/ingest",
                "telemetry_key": "machine-key",
                "telemetry_sync_seconds": 3600,
                **extra,
            }
        ),
        encoding="utf-8",
    )
    return p


def _user_config(**values):
    p = _user_config_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(values), encoding="utf-8")
    return p


def test_machine_config_alone(tmp_path, monkeypatch):
    monkeypatch.setenv("LIKHI_CONFIG", str(_machine_config(tmp_path)))
    result = _telemetry_config()
    assert result["mode"] == "full"
    assert result["endpoint"] == "https://ingest.example/v1/ingest"


def test_user_can_turn_reporting_off(tmp_path, monkeypatch):
    monkeypatch.setenv("LIKHI_CONFIG", str(_machine_config(tmp_path)))
    _user_config(telemetry="off")
    result = _telemetry_config()
    assert result["mode"] == "off"
    # The destination survives, so turning it back on needs nothing else.
    assert result["endpoint"] == "https://ingest.example/v1/ingest"
    assert result["key"] == "machine-key"


def test_user_can_turn_reporting_back_on(tmp_path, monkeypatch):
    monkeypatch.setenv("LIKHI_CONFIG", str(_machine_config(tmp_path)))
    _user_config(telemetry="full")
    result = _telemetry_config()
    assert result["mode"] == "full"
    assert result["endpoint"] == "https://ingest.example/v1/ingest"


def test_a_corrupt_user_file_does_not_break_the_machine_config(tmp_path, monkeypatch):
    monkeypatch.setenv("LIKHI_CONFIG", str(_machine_config(tmp_path)))
    p = _user_config_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("{ this is not json", encoding="utf-8")
    result = _telemetry_config()
    assert result["mode"] == "full"


def test_environment_still_wins_over_both(tmp_path, monkeypatch):
    monkeypatch.setenv("LIKHI_CONFIG", str(_machine_config(tmp_path)))
    _user_config(telemetry="off")
    monkeypatch.setenv("LIKHI_TELEMETRY", "metrics")
    assert _telemetry_config()["mode"] == "metrics"
