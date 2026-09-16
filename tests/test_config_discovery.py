"""The engine must find the shell's config wherever it is installed.

Regression test for a silent failure: the packaged installer writes config.json under
<app>\\pime\\python\\input_methods\\likhi, which the engine did not search, so telemetry stayed off
on every fresh install while the developer machine kept reporting from its older layout.
"""

import json

import pytest

from likhi.server import _config_paths, _telemetry_config


def _write(path, **cfg):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"telemetry": "full", **cfg}), encoding="utf-8")


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch):
    for name in (
        "LIKHI_TELEMETRY",
        "LIKHI_TELEMETRY_DROP",
        "LIKHI_TELEMETRY_ENDPOINT",
        "LIKHI_TELEMETRY_KEY",
        "LIKHI_CONFIG",
    ):
        monkeypatch.delenv(name, raising=False)


def test_explicit_config_path_wins(tmp_path, monkeypatch):
    cfg = tmp_path / "somewhere" / "config.json"
    _write(cfg, telemetry_endpoint="https://example.test/v1/ingest", telemetry_key="k")
    monkeypatch.setenv("LIKHI_CONFIG", str(cfg))
    result = _telemetry_config()
    assert result["mode"] == "full"
    assert result["endpoint"] == "https://example.test/v1/ingest"


def test_packaged_layout_is_searched():
    """<app>/pime/python/input_methods/likhi/config.json relative to the installed package."""
    wanted = ("pime", "python", "input_methods", "likhi", "config.json")
    assert any(p.parts[-5:] == wanted for p in _config_paths()), [str(p) for p in _config_paths()]


def test_local_appdata_is_searched(monkeypatch, tmp_path):
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path))
    assert any(p == tmp_path / "Likhi" / "config.json" for p in _config_paths())


def test_missing_config_means_telemetry_off(monkeypatch, tmp_path):
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path))
    monkeypatch.setenv("LIKHI_CONFIG", str(tmp_path / "nope.json"))
    monkeypatch.setenv("PROGRAMFILES(X86)", str(tmp_path / "pf"))
    result = _telemetry_config()
    assert result["mode"] == "off" and result["endpoint"] is None
