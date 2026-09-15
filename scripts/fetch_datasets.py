"""Download the open datasets Likhi is built and evaluated on.

Everything lands under data/raw/<name>/ (git-ignored). Downloads are resumable.

Usage:
    python scripts/fetch_datasets.py --all
    python scripts/fetch_datasets.py dakshina frequencywords
"""

from __future__ import annotations

import argparse
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
RAW = REPO / "data" / "raw"

DAKSHINA_URL = "https://storage.googleapis.com/gresearch/dakshina/dakshina_dataset_v1.0.tar"
DAKSHINA_PREFIX = "dakshina_dataset_v1.0/bn/"

FREQWORDS_BASE = (
    "https://raw.githubusercontent.com/hermitdave/FrequencyWords/master/content/2018/bn/"
)
FREQWORDS_FILES = ["bn_full.txt", "bn_50k.txt"]

BNWIKI_URL = "https://dumps.wikimedia.org/bnwiki/latest/bnwiki-latest-pages-articles.xml.bz2"

# Aksharantar (AI4Bharat): one zip per language on the Hugging Face dataset repo.
AKSHARANTAR_BEN_URL = "https://huggingface.co/datasets/ai4bharat/Aksharantar/resolve/main/ben.zip"

# BanglaTLit (EMNLP 2024 Findings), MIT. Note the odd capitalisation of the test file upstream.
BANGLATLIT_BASE = "https://raw.githubusercontent.com/farhanishmam/BanglaTLit/main/data/"
BANGLATLIT_FILES = {
    "BanglaTLit_train.csv": "train.csv",  # 245,727 rows: the PT corpus, ~42.9k of them annotated
    "BanglaTLiT_val.csv": "val.csv",
    "BanglaTLiT_test.csv": "test.csv",
    "BanglaTLit-PT.txt": "pt.txt",
}


def log(msg: str) -> None:
    print(f"[fetch] {msg}", flush=True)


def download(url: str, dest: Path, *, chunk: int = 1 << 20, retries: int = 5) -> Path:
    """Resumable download with a .part file. Returns dest."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists():
        log(f"exists, skipping: {dest.name}")
        return dest
    part = dest.with_suffix(dest.suffix + ".part")
    for attempt in range(1, retries + 1):
        have = part.stat().st_size if part.exists() else 0
        req = urllib.request.Request(url, headers={"User-Agent": "likhi-fetch/0.1"})
        if have:
            req.add_header("Range", f"bytes={have}-")
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                status = resp.status
                if have and status != 206:
                    # Server ignored Range; start over.
                    have = 0
                total = resp.headers.get("Content-Length")
                total_i = int(total) + have if total else None
                mode = "ab" if have else "wb"
                done = have
                last = time.time()
                with open(part, mode) as f:
                    while True:
                        buf = resp.read(chunk)
                        if not buf:
                            break
                        f.write(buf)
                        done += len(buf)
                        if time.time() - last > 5:
                            pct = f" {done / total_i:6.1%}" if total_i else ""
                            log(f"{dest.name}: {done / 1e6:,.0f} MB{pct}")
                            last = time.time()
            if total_i is not None and part.stat().st_size < total_i:
                raise OSError("short read")
            part.replace(dest)
            log(f"done: {dest} ({dest.stat().st_size / 1e6:,.1f} MB)")
            return dest
        except (urllib.error.URLError, OSError, TimeoutError) as e:
            log(f"attempt {attempt}/{retries} failed for {dest.name}: {e}")
            time.sleep(min(30, 2**attempt))
    raise SystemExit(f"failed to download {url}")


def _http_range(url: str, start: int, length: int, *, retries: int = 5) -> bytes:
    """Fetch bytes [start, start+length) of url via an HTTP Range request."""
    req = urllib.request.Request(
        url,
        headers={"Range": f"bytes={start}-{start + length - 1}", "User-Agent": "likhi-fetch/0.1"},
    )
    for attempt in range(1, retries + 1):
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                if resp.status != 206:
                    raise OSError(f"server ignored Range (status {resp.status})")
                return resp.read()
        except (urllib.error.URLError, OSError, TimeoutError) as e:
            if attempt == retries:
                raise
            log(f"range request retry {attempt}: {e}")
            time.sleep(2**attempt)
    raise AssertionError("unreachable")


def _remote_tar_members(url: str):
    """Yield (name, size, data_offset) for every member of a remote uncompressed tar.

    Reads only the 512-byte headers with Range requests, so a 2 GB archive can be indexed with a
    few hundred small requests instead of being downloaded whole.
    """
    offset = 0
    long_name: str | None = None
    while True:
        hdr = _http_range(url, offset, 512)
        if len(hdr) < 512 or hdr == b"\0" * 512:
            return
        name = hdr[0:100].split(b"\0", 1)[0].decode("utf-8", "replace")
        size_field = hdr[124:136].split(b"\0", 1)[0].strip()
        size = int(size_field, 8) if size_field else 0
        typeflag = hdr[156:157]
        prefix = hdr[345:500].split(b"\0", 1)[0].decode("utf-8", "replace")
        if prefix:
            name = prefix + "/" + name
        data_off = offset + 512
        blocks = (size + 511) // 512
        if typeflag == b"L":  # GNU long-name entry: the data block holds the real name
            long_name = (
                _http_range(url, data_off, size).split(b"\0", 1)[0].decode("utf-8", "replace")
            )
        else:
            if long_name is not None:
                name, long_name = long_name, None
            if typeflag in (b"0", b"\0") and size > 0:
                yield name, size, data_off
        offset = data_off + blocks * 512


def fetch_dakshina() -> None:
    """Fetch only the Bengali part of Dakshina straight out of the 2 GB tarball."""
    out = RAW / "dakshina" / "bn"
    marker = out / ".extracted"
    if marker.exists():
        log("dakshina bn already extracted")
        return
    log("indexing the remote tarball with Range requests (headers only)")
    n_seen_bn = 0
    for name, size, data_off in _remote_tar_members(DAKSHINA_URL):
        if not name.startswith(DAKSHINA_PREFIX):
            if n_seen_bn:
                break  # tar members from one directory walk are contiguous; we are past bn/
            continue
        n_seen_bn += 1
        target = out / name[len(DAKSHINA_PREFIX) :]
        if target.exists() and target.stat().st_size == size:
            log(f"exists: {target.name}")
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        log(f"{target.name}: {size / 1e6:,.1f} MB")
        part = target.with_suffix(target.suffix + ".part")
        with open(part, "wb") as f:
            pos = 0
            while pos < size:
                length = min(8 << 20, size - pos)
                buf = _http_range(DAKSHINA_URL, data_off + pos, length)
                if not buf:
                    raise OSError("empty range response")
                f.write(buf)
                pos += len(buf)
        part.replace(target)
    if not n_seen_bn:
        raise SystemExit("no bn/ members found in the Dakshina tarball; layout changed?")
    marker.write_text("ok\n")
    log(f"fetched {n_seen_bn} Dakshina bn files to {out}")


def fetch_frequencywords() -> None:
    out = RAW / "frequencywords"
    for name in FREQWORDS_FILES:
        download(FREQWORDS_BASE + name, out / name)


def fetch_bnwiki() -> None:
    download(BNWIKI_URL, RAW / "bnwiki" / "bnwiki-latest-pages-articles.xml.bz2")


def fetch_aksharantar() -> None:
    import zipfile

    out = RAW / "aksharantar"
    zip_path = download(AKSHARANTAR_BEN_URL, out / "ben.zip")
    marker = out / ".extracted"
    if marker.exists():
        log("aksharantar ben already extracted")
        return
    with zipfile.ZipFile(zip_path) as zf:
        zf.extractall(out)
        log(f"extracted: {', '.join(zf.namelist()[:10])}")
    marker.write_text("ok\n")


def fetch_banglatlit() -> None:
    out = RAW / "banglatlit"
    for remote, local in BANGLATLIT_FILES.items():
        download(BANGLATLIT_BASE + remote, out / local)


# IndicXlit (AI4Bharat, MIT): fairseq checkpoint + per-language word-probability dicts used for rescoring.
INDICXLIT_MODEL_URL = (
    "https://github.com/AI4Bharat/IndicXlit/releases/download/v1.0/indicxlit-en-indic-v1.0.zip"
)
INDICXLIT_DICTS_URL = (
    "https://github.com/AI4Bharat/IndicXlit/releases/download/v1.0/word_prob_dicts.zip"
)


def fetch_indicxlit() -> None:
    import zipfile

    out = RAW / "indicxlit"
    for url in (INDICXLIT_MODEL_URL, INDICXLIT_DICTS_URL):
        zip_path = download(url, out / url.rsplit("/", 1)[1])
        marker = zip_path.with_suffix(".extracted")
        if marker.exists():
            continue
        with zipfile.ZipFile(zip_path) as zf:
            names = zf.namelist()
            # Only the Bengali dict (bn_word_prob_dict.json) is needed from word_prob_dicts; the
            # model zip is taken whole.
            wanted = [
                n
                for n in names
                if "word_prob" not in zip_path.name
                or n.endswith("/")
                or n.rsplit("/", 1)[-1].startswith("bn_")
            ]
            for n in wanted:
                zf.extract(n, out)
            log(f"extracted {len(wanted)} entries from {zip_path.name}")
        marker.write_text("ok\n")


FETCHERS = {
    "dakshina": fetch_dakshina,
    "indicxlit": fetch_indicxlit,
    "aksharantar": fetch_aksharantar,
    "banglatlit": fetch_banglatlit,
    "frequencywords": fetch_frequencywords,
    "bnwiki": fetch_bnwiki,
}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("names", nargs="*", choices=sorted(FETCHERS), help="datasets to fetch")
    ap.add_argument("--all", action="store_true", help="fetch every dataset")
    args = ap.parse_args(argv)
    names = sorted(FETCHERS) if args.all else args.names
    if not names:
        ap.print_help()
        return 2
    RAW.mkdir(parents=True, exist_ok=True)
    for name in names:
        log(f"== {name} ==")
        FETCHERS[name]()
    return 0


if __name__ == "__main__":
    sys.exit(main())
