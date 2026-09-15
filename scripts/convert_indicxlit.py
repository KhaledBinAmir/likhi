"""Convert the IndicXlit (AI4Bharat, MIT) fairseq checkpoint to a CTranslate2 model, without fairseq.

fairseq has no Windows wheels for modern Python, but its checkpoint is just a torch pickle holding
`args`/`cfg` and a state dict. This script rebuilds the CTranslate2 TransformerSpec from the state
dict directly, mirroring ctranslate2.converters.fairseq, and writes a small int8 (or float32) model.

Usage (one-time, needs the `convert` dependency group):
    uv run --group convert python scripts/convert_indicxlit.py \
        --src data/raw/indicxlit/indicxlit-en-indic-v1.0 --out models/indicxlit-ct2 --quant int8

Run `--inspect` first to print the checkpoint args and tensor shapes.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

import numpy as np

SPECIALS = ["<s>", "<pad>", "</s>", "<unk>"]


def load_checkpoint(path: Path) -> tuple[dict, dict, dict]:
    """Return (model_args, task_args, state_dict-as-numpy) using the torch-free loader."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import fairseq_ckpt

    ckpt = fairseq_ckpt.load(path)
    state = {k: np.asarray(v) for k, v in ckpt["model"].items()}
    margs: dict = {}
    targs: dict = {}
    if ckpt.get("args") is not None:
        margs = fairseq_ckpt.to_plain(ckpt["args"])
        targs = dict(margs)
    if ckpt.get("cfg") is not None:
        cfg = fairseq_ckpt.to_plain(ckpt["cfg"])
        margs = {
            **(cfg.get("model") or {}),
            **{k: v for k, v in margs.items() if k not in (cfg.get("model") or {})},
        }
        targs = {**targs, **(cfg.get("task") or {})}
    return margs, targs, state


def read_dict(path: Path, needed: int, lang_tokens: list[str]) -> list[str]:
    """fairseq Dictionary as the multilingual task builds it at load time.

    Order: the 4 specials, then one 'token count' line per entry of dict.*.txt (madeupword padding
    is already in the file), then one ``__lang__`` symbol per line of lang_list.txt, in file order
    (fairseq's ``augment_dictionary``). The result must match the embedding matrix exactly.
    """
    tokens = list(SPECIALS)
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            tok = line.rsplit(" ", 1)[0] if " " in line else line
            tokens.append(tok)
    tokens.extend(lang_tokens)
    if len(tokens) != needed:
        raise SystemExit(f"{path.name}: {len(tokens)} tokens but embedding has {needed} rows")
    return tokens


def read_lang_tokens(path: Path) -> list[str]:
    langs = [ln.strip() for ln in path.read_text(encoding="utf-8").splitlines() if ln.strip()]
    return [f"__{lang}__" for lang in langs]


def sinusoidal_table(num_positions: int, dim: int, padding_idx: int = 1) -> np.ndarray:
    """fairseq SinusoidalPositionalEmbedding.get_embedding, then sliced from padding_idx+1."""
    half = dim // 2
    scale = math.log(10000) / (half - 1)
    freqs = np.exp(np.arange(half, dtype=np.float32) * -scale)
    n = num_positions + padding_idx + 1
    pos = np.arange(n, dtype=np.float32)[:, None] * freqs[None, :]
    emb = np.concatenate([np.sin(pos), np.cos(pos)], axis=1)
    if dim % 2 == 1:
        emb = np.concatenate([emb, np.zeros((n, 1), dtype=np.float32)], axis=1)
    emb[padding_idx, :] = 0
    return emb[padding_idx + 1 :].astype(np.float32)


def build_spec(args: dict, state: dict, src_dict: Path, tgt_dict: Path, lang_list: Path):
    from ctranslate2.specs import common_spec, transformer_spec

    enc_layers = int(args["encoder_layers"])
    dec_layers = int(args["decoder_layers"])
    heads = int(args["encoder_attention_heads"])
    assert heads == int(args["decoder_attention_heads"])
    pre_norm = bool(args.get("encoder_normalize_before", False))
    assert pre_norm == bool(args.get("decoder_normalize_before", False)), "mixed norm placement"
    act_name = args.get("activation_fn", "relu")
    activation = {
        "relu": common_spec.Activation.RELU,
        "gelu": common_spec.Activation.GELU,
        "gelu_accurate": common_spec.Activation.GELU,
        "swish": common_spec.Activation.SWISH,
    }[act_name]
    layernorm_embedding = bool(args.get("layernorm_embedding", False))
    spec = transformer_spec.TransformerSpec.from_config(
        (enc_layers, dec_layers),
        heads,
        pre_norm=pre_norm,
        activation=activation,
        layernorm_embedding=layernorm_embedding,
    )

    def g(name: str) -> np.ndarray:
        return state[name]

    def set_ln(ln_spec, prefix: str) -> None:
        ln_spec.gamma = g(prefix + ".weight")
        ln_spec.beta = g(prefix + ".bias")

    def set_linear(lin_spec, prefix: str) -> None:
        lin_spec.weight = g(prefix + ".weight")
        if prefix + ".bias" in state:
            lin_spec.bias = g(prefix + ".bias")

    def set_self_attn(attn_spec, prefix: str) -> None:
        attn_spec.linear[0].weight = np.concatenate(
            [
                g(f"{prefix}.q_proj.weight"),
                g(f"{prefix}.k_proj.weight"),
                g(f"{prefix}.v_proj.weight"),
            ]
        )
        attn_spec.linear[0].bias = np.concatenate(
            [g(f"{prefix}.q_proj.bias"), g(f"{prefix}.k_proj.bias"), g(f"{prefix}.v_proj.bias")]
        )
        set_linear(attn_spec.linear[1], f"{prefix}.out_proj")

    def set_cross_attn(attn_spec, prefix: str) -> None:
        set_linear(attn_spec.linear[0], f"{prefix}.q_proj")
        attn_spec.linear[1].weight = np.concatenate(
            [g(f"{prefix}.k_proj.weight"), g(f"{prefix}.v_proj.weight")]
        )
        attn_spec.linear[1].bias = np.concatenate(
            [g(f"{prefix}.k_proj.bias"), g(f"{prefix}.v_proj.bias")]
        )
        set_linear(attn_spec.linear[2], f"{prefix}.out_proj")

    max_pos = int(args.get("max_source_positions", 1024))
    scale_emb = not bool(args.get("no_scale_embedding", False))

    # ---- encoder
    enc = spec.encoder
    enc.embeddings[0].weight = g("encoder.embed_tokens.weight")
    enc.scale_embeddings = scale_emb
    dim = enc.embeddings[0].weight.shape[1]
    if "encoder.embed_positions.weight" in state:  # learned positions
        enc.position_encodings.encodings = g("encoder.embed_positions.weight")[2:]
    else:
        enc.position_encodings.encodings = sinusoidal_table(max_pos, dim)
    if layernorm_embedding:
        set_ln(enc.layernorm_embedding, "encoder.layernorm_embedding")
    if "encoder.layer_norm.weight" in state:
        set_ln(enc.layer_norm, "encoder.layer_norm")
    for i, layer in enumerate(enc.layer):
        p = f"encoder.layers.{i}"
        set_self_attn(layer.self_attention, f"{p}.self_attn")
        set_ln(layer.self_attention.layer_norm, f"{p}.self_attn_layer_norm")
        set_linear(layer.ffn.linear_0, f"{p}.fc1")
        set_linear(layer.ffn.linear_1, f"{p}.fc2")
        set_ln(layer.ffn.layer_norm, f"{p}.final_layer_norm")

    # ---- decoder
    dec = spec.decoder
    dec.embeddings.weight = g("decoder.embed_tokens.weight")
    dec.scale_embeddings = scale_emb
    if "decoder.embed_positions.weight" in state:
        dec.position_encodings.encodings = g("decoder.embed_positions.weight")[2:]
    else:
        dec.position_encodings.encodings = sinusoidal_table(
            int(args.get("max_target_positions", 1024)), dim
        )
    if layernorm_embedding:
        set_ln(dec.layernorm_embedding, "decoder.layernorm_embedding")
    if "decoder.layer_norm.weight" in state:
        set_ln(dec.layer_norm, "decoder.layer_norm")
    if "decoder.output_projection.weight" in state:
        dec.projection.weight = g("decoder.output_projection.weight")
    else:
        dec.projection.weight = g("decoder.embed_tokens.weight")
    for i, layer in enumerate(dec.layer):
        p = f"decoder.layers.{i}"
        set_self_attn(layer.self_attention, f"{p}.self_attn")
        set_ln(layer.self_attention.layer_norm, f"{p}.self_attn_layer_norm")
        set_cross_attn(layer.attention, f"{p}.encoder_attn")
        set_ln(layer.attention.layer_norm, f"{p}.encoder_attn_layer_norm")
        set_linear(layer.ffn.linear_0, f"{p}.fc1")
        set_linear(layer.ffn.linear_1, f"{p}.fc2")
        set_ln(layer.ffn.layer_norm, f"{p}.final_layer_norm")

    lang_tokens = read_lang_tokens(lang_list)
    src_vocab = read_dict(src_dict, enc.embeddings[0].weight.shape[0], lang_tokens)
    tgt_vocab = read_dict(tgt_dict, dec.embeddings.weight.shape[0], lang_tokens)
    spec.register_source_vocabulary(src_vocab)
    spec.register_target_vocabulary(tgt_vocab)
    # fairseq appends </s> to the source and starts decoding from </s> (no decoder lang token here).
    spec.config.add_source_bos = False
    spec.config.add_source_eos = True
    spec.config.decoder_start_token = "</s>"
    return spec, src_vocab, tgt_vocab


def export_npz(
    out: Path, args: dict, state: dict, src_dict: Path, tgt_dict: Path, lang_list: Path
) -> int:
    """Write the weights for the pure-NumPy runtime: float16 on disk, plus vocab and hyper-params."""
    lang_tokens = read_lang_tokens(lang_list)
    src_vocab = read_dict(src_dict, state["encoder.embed_tokens.weight"].shape[0], lang_tokens)
    tgt_vocab = read_dict(tgt_dict, state["decoder.embed_tokens.weight"].shape[0], lang_tokens)
    out.mkdir(parents=True, exist_ok=True)
    keep = {
        k: v.astype(np.float16)
        for k, v in state.items()
        if not k.endswith("version") and "embed_positions" not in k
    }
    np.savez_compressed(out / "model.npz", **keep)
    hp = {
        "encoder_layers": int(args["encoder_layers"]),
        "decoder_layers": int(args["decoder_layers"]),
        "heads": int(args["encoder_attention_heads"]),
        "dim": int(args["encoder_embed_dim"]),
        "pre_norm": bool(args.get("encoder_normalize_before", False)),
        "activation": args.get("activation_fn", "relu"),
        "layernorm_embedding": bool(args.get("layernorm_embedding", False)),
        "scale_embedding": not bool(args.get("no_scale_embedding", False)),
        "learned_pos": bool(args.get("encoder_learned_pos", False)),
        "padding_idx": 1,
        "source": "IndicXlit v1.0 en-indic (AI4Bharat, MIT)",
    }
    (out / "config.json").write_text(json.dumps(hp, indent=1), encoding="utf-8")
    (out / "source_vocabulary.json").write_text(
        json.dumps(src_vocab, ensure_ascii=False), encoding="utf-8"
    )
    (out / "target_vocabulary.json").write_text(
        json.dumps(tgt_vocab, ensure_ascii=False), encoding="utf-8"
    )
    size = sum(p.stat().st_size for p in out.iterdir())
    print(f"wrote {out} ({size / 1e6:.1f} MB, {len(keep)} tensors)")
    return 0


def find_files(src: Path) -> tuple[Path, Path, Path, Path]:
    ckpts = list(src.rglob("*.pt"))
    if not ckpts:
        raise SystemExit(f"no .pt checkpoint under {src}")
    ckpt = max(ckpts, key=lambda p: p.stat().st_size)
    dicts = {p.name: p for p in src.rglob("dict.*.txt")}
    src_dict = dicts.get("dict.en.txt")
    # The target dictionaries are one shared multilingual vocabulary duplicated per language.
    tgt_dict = dicts.get("dict.bn.txt") or next(
        (p for n, p in dicts.items() if p is not src_dict), None
    )
    if not src_dict or not tgt_dict:
        raise SystemExit(f"could not find source/target dict.*.txt under {src}: {sorted(dicts)}")
    lang_list = next(iter(src.rglob("lang_list.txt")), None)
    if lang_list is None:
        raise SystemExit(
            "lang_list.txt not found; download it from the IndicXlit repo "
            "(app/ai4bharat/transliteration/transformer/models/en2indic/lang_list.txt)"
        )
    return ckpt, src_dict, tgt_dict, lang_list


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--src", required=True, type=Path, help="extracted indicxlit-en-indic-v1.0 directory"
    )
    ap.add_argument("--out", type=Path, default=Path("models/indicxlit-ct2"))
    ap.add_argument(
        "--quant", default="int8", choices=["int8", "float32", "int8_float32", "float16"]
    )
    ap.add_argument(
        "--inspect", action="store_true", help="print args and tensor shapes, then exit"
    )
    ap.add_argument(
        "--npz",
        type=Path,
        help="instead of CTranslate2, write a NumPy weight bundle for likhi.engine.xlit_np (model.npz + vocab)",
    )
    args = ap.parse_args(argv)

    ckpt_path, src_dict, tgt_dict, lang_list = find_files(args.src)
    print(f"checkpoint: {ckpt_path} ({ckpt_path.stat().st_size / 1e6:.1f} MB)")
    print(f"dicts: {src_dict.name} (source), {tgt_dict.name} (target); langs: {lang_list}")
    margs, targs, state = load_checkpoint(ckpt_path)
    if args.inspect:
        keys = [
            "arch",
            "encoder_layers",
            "decoder_layers",
            "encoder_embed_dim",
            "decoder_embed_dim",
            "encoder_ffn_embed_dim",
            "encoder_attention_heads",
            "decoder_attention_heads",
            "encoder_normalize_before",
            "decoder_normalize_before",
            "activation_fn",
            "layernorm_embedding",
            "no_scale_embedding",
            "share_all_embeddings",
            "share_decoder_input_output_embed",
            "encoder_learned_pos",
            "decoder_learned_pos",
            "max_source_positions",
            "max_target_positions",
        ]
        print("model:", json.dumps({k: margs.get(k) for k in keys}, indent=1, default=str))
        tkeys = [
            "_name",
            "task",
            "source_lang",
            "target_lang",
            "lang_pairs",
            "langs",
            "lang_dict",
            "encoder_langtok",
            "decoder_langtok",
            "lang_tok_style",
            "lang_tok_replacing_bos_eos",
            "langtoks_specs",
            "left_pad_source",
            "left_pad_target",
        ]
        print("task:", json.dumps({k: targs.get(k) for k in tkeys}, indent=1, default=str))
        for k, v in list(state.items())[:12]:
            print(f"  {k}: {v.shape} {v.dtype}")
        print(
            f"  ... {len(state)} tensors, {sum(v.size for v in state.values()) / 1e6:.1f}M params"
        )
        return 0

    if args.npz:
        return export_npz(args.npz, margs, state, src_dict, tgt_dict, lang_list)

    import ctranslate2
    from ctranslate2.converters.converter import Converter

    spec, src_vocab, tgt_vocab = build_spec(margs, state, src_dict, tgt_dict, lang_list)

    class _Conv(Converter):
        def _load(self):
            return spec

    args.out.mkdir(parents=True, exist_ok=True)
    _Conv().convert(str(args.out), quantization=args.quant, force=True)
    meta = {
        "source": "IndicXlit v1.0 en-indic (AI4Bharat, MIT)",
        "quantization": args.quant,
        "src_vocab_size": len(src_vocab),
        "tgt_vocab_size": len(tgt_vocab),
        "lang_tokens": [t for t in src_vocab if t.startswith("__") and t.endswith("__")],
        "ctranslate2": ctranslate2.__version__,
    }
    (args.out / "likhi_meta.json").write_text(json.dumps(meta, indent=1), encoding="utf-8")
    size = sum(p.stat().st_size for p in args.out.rglob("*") if p.is_file())
    print(f"wrote {args.out} ({size / 1e6:.1f} MB); lang tokens: {len(meta['lang_tokens'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
