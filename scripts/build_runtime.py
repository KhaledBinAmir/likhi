"""Build a self-contained engine runtime: embedded Python + NumPy + Likhi + the language data.

The result runs `likhi-server` on a machine with nothing installed, which is what the installer
ships. Embedded Python rather than PyInstaller on purpose: it is smaller, every file is visible and
auditable, and it does not trip antivirus heuristics the way a packed executable does.

    python scripts/build_runtime.py                 # -> dist/runtime
    python scripts/build_runtime.py --skip-download # reuse an already downloaded embeddable zip

Layout produced:
    dist/runtime/python/            embeddable CPython, pythonw.exe runs the server windowless
    dist/runtime/python/Lib/site-packages/   numpy, marisa_trie, avro, likhi
    dist/runtime/models/            indicxlit-np and lexicon
    dist/runtime/likhi-server.cmd   convenience launcher
"""

from __future__ import annotations

import argparse
import platform
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
PY_VERSION = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
CACHE = REPO / "data" / "cache"
RUNTIME_PACKAGES = ["numpy", "marisa-trie", "avro.py"]
MODEL_DIRS = ["indicxlit-np", "lexicon"]


def log(msg: str) -> None:
    print(f"[runtime] {msg}", flush=True)


def embeddable_url(version: str) -> str:
    arch = {"AMD64": "amd64", "ARM64": "arm64"}.get(platform.machine(), "amd64")
    return f"https://www.python.org/ftp/python/{version}/python-{version}-embed-{arch}.zip"


def fetch_embeddable(version: str, skip_download: bool) -> Path:
    CACHE.mkdir(parents=True, exist_ok=True)
    url = embeddable_url(version)
    dest = CACHE / url.rsplit("/", 1)[1]
    if dest.exists():
        log(f"using cached {dest.name}")
        return dest
    if skip_download:
        raise SystemExit(f"{dest} not present and --skip-download was given")
    log(f"downloading {url}")
    with urllib.request.urlopen(url, timeout=120) as resp, open(dest, "wb") as f:
        shutil.copyfileobj(resp, f)
    log(f"downloaded {dest.name} ({dest.stat().st_size / 1e6:.1f} MB)")
    return dest


def find_uv() -> str | None:
    found = shutil.which("uv")
    if found:
        return found
    guess = (
        Path.home()
        / "AppData/Local/Microsoft/WinGet/Packages"
        / "astral-sh.uv_Microsoft.Winget.Source_8wekyb3d8bbwe/uv.exe"
    )
    return str(guess) if guess.exists() else None


def install_packages(site: Path, version: str) -> None:
    """Install the runtime dependencies into ``site``.

    Wheels are resolved for the *target* interpreter (CPython on Windows x86-64), not for whatever
    is running this script, so the build works from a uv virtual environment, which has no pip.
    """
    major_minor = ".".join(version.split(".")[:2])
    uv = find_uv()
    if uv:
        cmd = [
            uv,
            "pip",
            "install",
            "--target",
            str(site),
            "--python-version",
            major_minor,
            "--python-platform",
            "x86_64-pc-windows-msvc",
            "--only-binary",
            ":all:",
            "--no-compile-bytecode",
            *RUNTIME_PACKAGES,
        ]
    else:
        cmd = [
            sys.executable,
            "-m",
            "pip",
            "install",
            "--no-compile",
            "--only-binary",
            ":all:",
            "--target",
            str(site),
            *RUNTIME_PACKAGES,
        ]
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(f"dependency install failed:\n{result.stdout}\n{result.stderr}")


def build(out: Path, version: str, skip_download: bool) -> None:
    zip_path = fetch_embeddable(version, skip_download)
    if out.exists():
        shutil.rmtree(out)
    py_dir = out / "python"
    py_dir.mkdir(parents=True)

    with zipfile.ZipFile(zip_path) as zf:
        zf.extractall(py_dir)
    log(f"extracted embeddable CPython {version}")

    # The embeddable build ignores site-packages until the ._pth file says otherwise.
    pth = next(py_dir.glob("python*._pth"))
    text = pth.read_text(encoding="utf-8")
    if "import site" not in text.replace("#import site", ""):
        text = text.replace("#import site", "import site")
        if "import site" not in text:
            text += "\nimport site\n"
    if "Lib\\site-packages" not in text:
        text = text.replace("\n.\n", "\n.\nLib\\site-packages\n", 1)
        if "Lib\\site-packages" not in text:
            text += "Lib\\site-packages\n"
    pth.write_text(text, encoding="utf-8")
    log(f"enabled site-packages in {pth.name}")

    site = py_dir / "Lib" / "site-packages"
    site.mkdir(parents=True, exist_ok=True)
    log(f"installing {', '.join(RUNTIME_PACKAGES)} for CPython {version} on Windows")
    install_packages(site, version)

    shutil.copytree(
        REPO / "src" / "likhi", site / "likhi", ignore=shutil.ignore_patterns("__pycache__")
    )
    log("copied the likhi package")

    models = out / "models"
    models.mkdir()
    for name in MODEL_DIRS:
        src = REPO / "models" / name
        if not src.exists():
            raise SystemExit(
                f"missing {src}; build it first (likhi-data lexicon / convert_indicxlit)"
            )
        shutil.copytree(src, models / name, ignore=shutil.ignore_patterns("*.part", "__pycache__"))
    log(f"copied models: {', '.join(MODEL_DIRS)}")

    # The engine finds its data relative to the package by default; in the runtime the layout
    # differs, so point it at the bundled copies explicitly.
    # LIKHI_CONFIG is explicit on purpose: the text service's config.json sits in a sibling folder
    # of this runtime, and guessing it from the package path alone has already failed once.
    (out / "likhi-server.cmd").write_text(
        "@echo off\r\n"
        "setlocal\r\n"
        'set "LIKHI_HOME=%~dp0"\r\n'
        'set "LIKHI_MODELS=%LIKHI_HOME%models"\r\n'
        'set "LIKHI_CONFIG=%LIKHI_HOME%..\\pime\\python\\input_methods\\likhi\\config.json"\r\n'
        'start "" "%LIKHI_HOME%python\\pythonw.exe" -m likhi.server %*\r\n',
        encoding="ascii",
    )
    (out / "likhi-report.cmd").write_text(
        "@echo off\r\n"
        "setlocal\r\n"
        'set "LIKHI_HOME=%~dp0"\r\n'
        'set "LIKHI_MODELS=%LIKHI_HOME%models"\r\n'
        '"%LIKHI_HOME%python\\python.exe" -m likhi.report %*\r\n',
        encoding="ascii",
    )

    size = sum(p.stat().st_size for p in out.rglob("*") if p.is_file())
    log(f"built {out} ({size / 1e6:.0f} MB)")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--out", type=Path, default=REPO / "dist" / "runtime")
    ap.add_argument("--python-version", default=PY_VERSION)
    ap.add_argument("--skip-download", action="store_true")
    args = ap.parse_args(argv)
    build(args.out, args.python_version, args.skip_download)
    return 0


if __name__ == "__main__":
    sys.exit(main())
