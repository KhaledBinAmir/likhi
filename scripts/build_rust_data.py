"""Convert the engine's data files into formats the Rust engine can memory-map.

    python scripts/build_rust_data.py            # -> models/rust/

Two conversions, for two dependencies the Rust port cannot take:

* **marisa tries -> .lkx tables.** `marisa_trie` is a C++ library with no maintained Rust reader,
  and its on-disk format is intricate enough that reimplementing it would be its own project. It is
  also more than we need: every use in core.py is either an exact lookup or a scan of the entries
  sharing a prefix. That is a sorted array with a binary search, which is a format small enough to
  verify by eye and fast enough to beat a trie on modern hardware, because a prefix scan over sorted
  keys is sequential reads instead of pointer chasing.

* **model.npz -> model.lkw.** An .npz is a zip of .npy members, so reading one means a zip decoder
  plus an .npy header parser, and then decompressing 20 MB every time the engine starts. A flat file
  with an aligned f32 blob can be mapped and used in place.

The weight conversion also pre-applies every transformation `XlitTransformer.__init__` does at load
time -- transposes, the fused q/k/v projection with the attention scaling folded into the q columns,
the sinusoidal position table, the banned-token mask. That is deliberate: those steps are done here
once, in the same NumPy that produced the reference outputs, so the Rust side cannot introduce a
rounding difference by recomputing them, and engine startup does no arithmetic at all.

Both formats are little-endian. Nothing here targets a big-endian machine, and Windows on ARM is
little-endian too.
"""

from __future__ import annotations

import argparse
import json
import math
import struct
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "src"))

LKX_MAGIC = b"LKX3"
LKW_MAGIC = b"LKW1"
ALIGN = 64  # cache-line, and enough for any SIMD load the forward pass might use
BLOCK_LEN = 16  # front-coding restart interval: bounds the linear walk after a binary search


def log(msg: str) -> None:
    print(f"[rust-data] {msg}", flush=True)


# --------------------------------------------------------------------------- tries


# Each trie, with the struct format its records use and what the fields mean. The format strings
# are copied from LikhiEngine.__init__ and must stay in step with it.
# Tables whose scan order reaches the ranker before anything sorts it, and which therefore need
# marisa's enumeration order preserved. See write_lkx.
RANKED = {"romans"}

TRIES = {
    "unigrams": ("<III", "wiki, subtitle and chat counts for a word"),
    "romans": ("<Ib", "attestation count and source id for '<roman>\\t<word>'"),
    "keys": ("<I", "corpus score for '<level>:<key>\\t<word>'"),
    "prefixes": ("<I", "precomputed completion score for short inputs"),
    "bigrams": ("<I", "count for '<prev>\\t<word>'"),
    "bigram_totals": ("<I", "total bigram count for a previous word"),
}


def _uvarint(n: int) -> bytes:
    """LEB128. Shared-prefix and suffix lengths are almost always < 128, so this is one byte."""
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def write_lkx(
    path: Path,
    entries: list[tuple[str, bytes]],
    record_size: int,
    note: str,
    ranks: list[int] | None = None,
) -> None:
    """Sorted (key, record) pairs, binary searchable and mappable.

        magic       4   b"LKX3"
        version     4   u32 = 3
        count       4   u32   entries
        record_sz   4   u32   bytes per record
        block_len   4   u32   entries per block
        n_blocks    4   u32
        flags       4   u32   bit 0: a rank array follows the records
        keys_len    8   u64
        block_offs  4*n_blocks u32   byte offset of each block within the key blob
        keys        keys_len bytes   front-coded, see below
        (pad to 64)
        records     count*record_sz bytes, entry i at i*record_sz
        ranks       count*4 bytes when flag 0 is set; see below

    Keys are stored front-coded in blocks: the first key of a block is written whole, and each key
    after it as (shared-prefix length, suffix length, suffix) with both lengths as LEB128 varints.
    Storing every key whole was the obvious first version and cost 121 MB against marisa's 21 -- a
    keyboard is not entitled to that much of someone's disk. These keys are sorted and enormously
    redundant ("abcd\\tword" repeated for every attested spelling), so front-coding recovers most of
    what a trie was buying, while keeping lookup to a binary search over block heads followed by a
    short linear walk. Blocks of 16 keep that walk bounded.

    Sorting is by UTF-8 bytes, which for UTF-8 orders identically to comparing Unicode scalars,
    which is what Python's `str` comparison does. That matters: core.py breaks score ties by
    comparing words, so a different order here would silently reorder tied candidates.

    Duplicate keys are permitted and preserved in order: marisa's RecordTrie allows several records
    per key and `.items()` yields each one.

    **Ranks.** `ranks[i]` is the position entry `i` had in marisa's own `.items()` enumeration, and
    it exists because the engine's behaviour depends on that order. `core.py` inserts candidates
    into a dict in the order the trie yields them, and Python's stable sort then breaks equal scores
    by insertion order -- and marisa's order is a LOUDS traversal, not lexicographic: for
    "screensaver" it yields স্ক্রিনসেভারের before its own prefix স্ক্রিনসেভার. Eleven percent of
    sampled prefixes come back in a non-lexicographic order, and the order also decides which
    candidates are cheap enough to send to the model, so it is not merely cosmetic.

    Measured and relied upon: `.items(prefix)` is a restriction of the global `.items()` order, so a
    single global index per entry reproduces every prefix scan exactly.

    Only tables whose scan order reaches the ranking unsorted carry this. The key and prefix
    channels sort their results by (score, word), which is already a total order, so paying four
    bytes an entry for them would buy nothing.
    """
    # Sort keys and ranks together: the rank belongs to the entry, not to its position.
    order = sorted(
        range(len(entries)), key=lambda i: entries[i][0].encode("utf-8")
    )
    sorted_ranks = [ranks[i] for i in order] if ranks is not None else None
    entries = [entries[i] for i in order]

    keys_blob = bytearray()
    block_offs: list[int] = []
    prev = b""
    for i, (key, _rec) in enumerate(entries):
        kb = key.encode("utf-8")
        if i % BLOCK_LEN == 0:
            block_offs.append(len(keys_blob))
            keys_blob += _uvarint(len(kb)) + kb
        else:
            shared = 0
            limit = min(len(prev), len(kb))
            while shared < limit and prev[shared] == kb[shared]:
                shared += 1
            keys_blob += _uvarint(shared) + _uvarint(len(kb) - shared) + kb[shared:]
        prev = kb

    records = bytearray()
    for _key, rec in entries:
        if len(rec) != record_size:
            raise SystemExit(f"{path.name}: record of {len(rec)} bytes, expected {record_size}")
        records += rec

    flags = 1 if sorted_ranks is not None else 0
    head = bytearray()
    head += LKX_MAGIC
    head += struct.pack(
        "<IIIIII", 3, len(entries), record_size, BLOCK_LEN, len(block_offs), flags
    )
    head += struct.pack("<Q", len(keys_blob))
    head += struct.pack(f"<{len(block_offs)}I", *block_offs)
    head += keys_blob
    head += b"\0" * ((-len(head)) % ALIGN)

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as fh:
        fh.write(head)
        fh.write(records)
        if sorted_ranks is not None:
            fh.write(struct.pack(f"<{len(sorted_ranks)}I", *sorted_ranks))
    log(
        f"{path.name}: {len(entries)} entries, {path.stat().st_size / 1e6:.1f} MB  ({note})"
        + ("  [+ranks]" if flags else "")
    )


def convert_tries(lexicon_dir: Path, out_dir: Path) -> None:
    import marisa_trie

    for name, (fmt, note) in TRIES.items():
        src = lexicon_dir / f"{name}.marisa"
        if not src.exists():
            log(f"{name}: absent, skipping")
            continue
        trie = marisa_trie.RecordTrie(fmt)
        trie.load(str(src))
        size = struct.calcsize(fmt)
        entries: list[tuple[str, bytes]] = []
        ranks: list[int] = []
        dupes = 0
        seen: set[str] = set()
        # enumerate() over .items() *is* marisa's order; see write_lkx for why it is kept.
        for i, (key, rec) in enumerate(trie.items()):
            if key in seen:
                dupes += 1
            seen.add(key)
            entries.append((key, struct.pack(fmt, *rec)))
            ranks.append(i)
        if dupes:
            # Not fatal -- the format keeps them -- but core.py's .get() takes only the first
            # record, so if this ever fires the Rust side's choice of "first" has to match marisa's.
            log(f"WARNING {name}: {dupes} duplicate keys; .get() semantics depend on order")
        write_lkx(
            out_dir / f"{name}.lkx",
            entries,
            size,
            note,
            ranks=ranks if name in RANKED else None,
        )

    for extra in ("meta.json", "bigrams_meta.json", "weights.json"):
        src = lexicon_dir / extra
        if src.exists():
            (out_dir / extra).write_bytes(src.read_bytes())
            log(f"copied {extra}")


# --------------------------------------------------------------------------- weights


class WeightWriter:
    """Accumulates named f32 arrays into one aligned blob with a JSON index."""

    def __init__(self) -> None:
        self.arrays: list[dict] = []
        self.blob = bytearray()

    def add(self, name: str, arr) -> None:
        import numpy as np

        a = np.ascontiguousarray(arr, dtype=np.float32)
        if not np.isfinite(a).all():
            # -inf is legitimate in the banned mask; anywhere else it is a conversion bug.
            if name != "banned":
                raise SystemExit(f"{name}: contains non-finite values")
        self.arrays.append(
            {"name": name, "shape": list(a.shape), "offset": len(self.blob) // 4, "count": a.size}
        )
        self.blob += a.tobytes(order="C")

    def write(self, path: Path, meta: dict) -> None:
        index = json.dumps(
            {"meta": meta, "arrays": self.arrays}, sort_keys=True, ensure_ascii=True
        ).encode("utf-8")
        head = bytearray()
        head += LKW_MAGIC
        head += struct.pack("<II", 1, len(index))
        head += index
        head += b"\0" * ((-len(head)) % ALIGN)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("wb") as fh:
            fh.write(head)
            fh.write(self.blob)
        log(f"{path.name}: {len(self.arrays)} arrays, {path.stat().st_size / 1e6:.1f} MB")


def convert_model(model_dir: Path, out_dir: Path, max_positions: int) -> None:
    import numpy as np

    from likhi.engine.xlit_np import sinusoidal

    cfg = json.loads((model_dir / "config.json").read_text(encoding="utf-8"))
    dim = int(cfg["dim"])
    heads = int(cfg["heads"])
    head_dim = dim // heads
    padding_idx = int(cfg.get("padding_idx", 1))
    scaling = head_dim**-0.5

    src_vocab = json.loads((model_dir / "source_vocabulary.json").read_text(encoding="utf-8"))
    tgt_vocab = json.loads((model_dir / "target_vocabulary.json").read_text(encoding="utf-8"))
    tgt_index = {t: i for i, t in enumerate(tgt_vocab)}

    w = np.load(model_dir / "model.npz")
    f = {k: w[k].astype(np.float32) for k in w.files}

    out = WeightWriter()
    out.add("enc_embed", f["encoder.embed_tokens.weight"])
    out.add("dec_embed", f["decoder.embed_tokens.weight"])
    out.add("out_proj", f.get("decoder.output_projection.weight", f["decoder.embed_tokens.weight"]))
    # Stored transposed: the forward pass computes x @ out_proj.T, and transposing here means the
    # Rust matmul only ever walks contiguous rows.
    out.add(
        "out_proj_t",
        np.ascontiguousarray(
            f.get("decoder.output_projection.weight", f["decoder.embed_tokens.weight"]).T
        ),
    )
    out.add("pos", sinusoidal(max_positions, dim, padding_idx))

    def opt(name: str, key: str) -> None:
        """Layer norms that may be absent from the checkpoint; shape [] means 'not present'."""
        v = f.get(key)
        out.add(name, v if v is not None else np.zeros(0, dtype=np.float32))

    opt("enc_ln_emb_g", "encoder.layernorm_embedding.weight")
    opt("enc_ln_emb_b", "encoder.layernorm_embedding.bias")
    opt("dec_ln_emb_g", "decoder.layernorm_embedding.weight")
    opt("dec_ln_emb_b", "decoder.layernorm_embedding.bias")
    opt("enc_ln_g", "encoder.layer_norm.weight")
    opt("enc_ln_b", "encoder.layer_norm.bias")
    opt("dec_ln_g", "decoder.layer_norm.weight")
    opt("dec_ln_b", "decoder.layer_norm.bias")

    def attn(prefix: str, ln: str, tag: str, *, fused: bool) -> None:
        wq = f[f"{prefix}.q_proj.weight"].T.copy()
        bq = f[f"{prefix}.q_proj.bias"]
        wk = f[f"{prefix}.k_proj.weight"].T.copy()
        bk = f[f"{prefix}.k_proj.bias"]
        wv = f[f"{prefix}.v_proj.weight"].T.copy()
        bv = f[f"{prefix}.v_proj.bias"]
        if fused:
            # Exactly as xlit_np builds it, including folding the scaling into q. Doing it here in
            # NumPy guarantees identical bits to the reference implementation.
            out.add(f"{tag}.wqkv", np.ascontiguousarray(np.concatenate([wq * scaling, wk, wv], 1)))
            out.add(f"{tag}.bqkv", np.concatenate([bq * scaling, bk, bv]))
        else:
            # Cross attention projects encoder output once with k/v, and applies scaling to q after
            # the projection, so those stay separate.
            out.add(f"{tag}.wq", wq)
            out.add(f"{tag}.bq", bq)
            out.add(f"{tag}.wk", wk)
            out.add(f"{tag}.bk", bk)
            out.add(f"{tag}.wv", wv)
            out.add(f"{tag}.bv", bv)
        out.add(f"{tag}.wo", f[f"{prefix}.out_proj.weight"].T.copy())
        out.add(f"{tag}.bo", f[f"{prefix}.out_proj.bias"])
        out.add(f"{tag}.ln_g", f[f"{ln}.weight"])
        out.add(f"{tag}.ln_b", f[f"{ln}.bias"])

    def ffn(prefix: str, tag: str) -> None:
        out.add(f"{tag}.w1", f[f"{prefix}.fc1.weight"].T.copy())
        out.add(f"{tag}.b1", f[f"{prefix}.fc1.bias"])
        out.add(f"{tag}.w2", f[f"{prefix}.fc2.weight"].T.copy())
        out.add(f"{tag}.b2", f[f"{prefix}.fc2.bias"])
        out.add(f"{tag}.ln_g", f[f"{prefix}.final_layer_norm.weight"])
        out.add(f"{tag}.ln_b", f[f"{prefix}.final_layer_norm.bias"])

    n_enc = int(cfg["encoder_layers"])
    n_dec = int(cfg["decoder_layers"])
    for i in range(n_enc):
        attn(f"encoder.layers.{i}.self_attn", f"encoder.layers.{i}.self_attn_layer_norm",
             f"enc.{i}.attn", fused=True)
        ffn(f"encoder.layers.{i}", f"enc.{i}.ffn")
    for i in range(n_dec):
        attn(f"decoder.layers.{i}.self_attn", f"decoder.layers.{i}.self_attn_layer_norm",
             f"dec.{i}.self", fused=True)
        attn(f"decoder.layers.{i}.encoder_attn", f"decoder.layers.{i}.encoder_attn_layer_norm",
             f"dec.{i}.cross", fused=False)
        ffn(f"decoder.layers.{i}", f"dec.{i}.ffn")

    # The decoder must never emit these: padding, the bos marker, the language tags, and the
    # dictionary's padding words. Built here so the rule lives in one place.
    banned = np.zeros(len(tgt_vocab), dtype=np.float32)
    banned[tgt_index["<pad>"]] = -np.inf
    banned[tgt_index["<s>"]] = -np.inf
    for t, i in tgt_index.items():
        if (t.startswith("__") and t.endswith("__")) or t.startswith("madeupword"):
            banned[i] = -np.inf
    out.add("banned", banned)

    meta = {
        "dim": dim,
        "heads": heads,
        "head_dim": head_dim,
        "encoder_layers": n_enc,
        "decoder_layers": n_dec,
        "pre_norm": bool(cfg["pre_norm"]),
        "activation": cfg["activation"],
        "scale_embedding": bool(cfg.get("scale_embedding", True)),
        "embed_scale": math.sqrt(dim) if cfg.get("scale_embedding", True) else 1.0,
        "padding_idx": padding_idx,
        "pos_offset": padding_idx + 1,
        "max_positions": max_positions,
        "scaling": scaling,
        "source": cfg.get("source", ""),
    }
    out_dir.mkdir(parents=True, exist_ok=True)
    out.write(out_dir / "model.lkw", meta)
    (out_dir / "source_vocabulary.json").write_text(
        json.dumps(src_vocab, ensure_ascii=False), encoding="utf-8"
    )
    (out_dir / "target_vocabulary.json").write_text(
        json.dumps(tgt_vocab, ensure_ascii=False), encoding="utf-8"
    )
    log(f"vocab: {len(src_vocab)} source, {len(tgt_vocab)} target")


# --------------------------------------------------------------------------- avro


def convert_avro(out_dir: Path) -> None:
    """Dump the Avro Phonetic rule tables the parser needs, in their original order.

    `avro.py` keeps its rules in a 52 KB Python literal. Transcribing that into Rust by hand would
    be a thousand opportunities to mistype a Bengali string, so it is emitted as JSON and read at
    load time instead. The tables are data, not code: the algorithm is ported, the rules are copied.

    Order is preserved exactly. `exact_find_in_pattern` returns every pattern matching at the cursor
    and the parser takes `[0]`, so the list order *is* the precedence rule -- longest-match-first
    only works because the table happens to be sorted that way. A dict or a set anywhere in this
    path would silently reorder it.

    Only the forward `parse` path is emitted. The Bijoy conversion and reverse transliteration are
    unused by the engine, and shipping rules nothing reads is dead weight.
    """
    try:
        from avro.resources import DICT
    except ImportError:
        log("avro not importable; skipping avro rules")
        return

    a = DICT["avro"]
    patterns = []
    skipped = 0
    for p in a["patterns"]:
        # `exact_find_in_pattern` requires a non-None "find", so an entry without one can never
        # match and is dropped. Dropping it preserves order among the rest, which is what matters.
        if p.get("find") is None:
            skipped += 1
            continue
        # "replace" is genuinely optional, and its absence is not the same as an empty string: the
        # parser's non-rule branch tests `isinstance(replaced, str)`, so a missing replace falls
        # through to rule matching instead of substituting nothing. Kept as null to preserve that.
        entry = {"find": p["find"], "replace": p.get("replace")}
        if "rules" in p:
            entry["rules"] = [
                {
                    "replace": r["replace"],
                    "matches": [
                        {
                            "type": mm.get("type"),
                            "scope": mm.get("scope"),
                            "value": mm.get("value"),
                        }
                        for mm in r["matches"]
                    ],
                }
                for r in p["rules"]
            ]
        patterns.append(entry)

    bundle = {
        "patterns": patterns,
        "vowel": a["vowel"],
        "consonant": a["consonant"],
        "casesensitive": a["casesensitive"],
        # A list of pairs, not an object: find_in_remap applies these in order and each substitution
        # sees the text the previous one produced, so the order is part of the behaviour.
        "exceptions": [[k, v] for k, v in a["exceptions"].items()],
    }
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / "avro.json"
    path.write_text(json.dumps(bundle, ensure_ascii=False, indent=0), encoding="utf-8")
    rules = sum(len(p.get("rules", [])) for p in patterns)
    log(
        f"avro.json: {len(patterns)} patterns ({rules} rules), "
        f"{len(bundle['exceptions'])} exceptions, {path.stat().st_size / 1e3:.0f} KB"
        + (f"; {skipped} entries without a 'find' dropped" if skipped else "")
    )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--models", type=Path, default=REPO / "models")
    ap.add_argument("--out", type=Path, default=REPO / "models" / "rust")
    ap.add_argument("--max-positions", type=int, default=256)
    ap.add_argument("--skip-tries", action="store_true")
    ap.add_argument("--skip-model", action="store_true")
    ap.add_argument("--skip-avro", action="store_true")
    args = ap.parse_args()

    if not args.skip_avro:
        convert_avro(args.out)
    if not args.skip_tries:
        convert_tries(args.models / "lexicon", args.out / "lexicon")
    if not args.skip_model:
        convert_model(args.models / "indicxlit-np", args.out / "indicxlit", args.max_positions)
    log(f"done -> {args.out}")


if __name__ == "__main__":
    main()
