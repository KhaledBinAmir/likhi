"""Pure-NumPy inference for the IndicXlit character transformer (fairseq `transformer` arch).

Why not a runtime like CTranslate2 or onnxruntime? The model is 11M parameters and the inputs are
single words, so a beam search costs a few million FLOPs. NumPy handles that in milliseconds, and
the whole transliteration channel then depends on nothing but NumPy: no 60 MB native library, no
CPU-specific crashes on users' machines, and every step is readable Python that can be ported or
distilled later.

Faithful to fairseq's pre-norm TransformerModel: scaled embeddings + sinusoidal positions
(offset padding_idx+1), layernorm_embedding, exact-erf GELU, final layer norms, and beam search
with length-normalized scores.
"""

from __future__ import annotations

import json
import math
from collections.abc import Sequence
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

import numpy as np

from likhi.engine.textnorm import canonical, normalize_roman

DEFAULT_MODEL_DIR = Path(__file__).resolve().parents[3] / "models" / "indicxlit-np"

_SQRT2 = math.sqrt(2.0)


def _erf(x: np.ndarray) -> np.ndarray:
    # Abramowitz & Stegun 7.1.26, |error| < 1.5e-7: plenty for inference.
    sign = np.sign(x)
    a = np.abs(x)
    t = 1.0 / (1.0 + 0.3275911 * a)
    y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * np.exp(-a * a)
    return sign * y


def gelu(x: np.ndarray) -> np.ndarray:
    return 0.5 * x * (1.0 + _erf(x / _SQRT2))


def relu(x: np.ndarray) -> np.ndarray:
    return np.maximum(x, 0.0)


def layer_norm(x: np.ndarray, g: np.ndarray, b: np.ndarray, eps: float = 1e-5) -> np.ndarray:
    mu = x.mean(-1, keepdims=True)
    var = ((x - mu) ** 2).mean(-1, keepdims=True)
    return (x - mu) / np.sqrt(var + eps) * g + b


def log_softmax(x: np.ndarray) -> np.ndarray:
    m = x.max(-1, keepdims=True)
    z = x - m
    return z - np.log(np.exp(z).sum(-1, keepdims=True))


def sinusoidal(num_positions: int, dim: int, padding_idx: int = 1) -> np.ndarray:
    """fairseq SinusoidalPositionalEmbedding table; row p is position p (positions start at padding_idx+1)."""
    half = dim // 2
    scale = math.log(10000) / (half - 1)
    freqs = np.exp(np.arange(half, dtype=np.float32) * -scale)
    n = num_positions + padding_idx + 1
    pos = np.arange(n, dtype=np.float32)[:, None] * freqs[None, :]
    emb = np.concatenate([np.sin(pos), np.cos(pos)], axis=1)
    if dim % 2:
        emb = np.concatenate([emb, np.zeros((n, 1), np.float32)], axis=1)
    emb[padding_idx] = 0
    return emb.astype(np.float32)


@dataclass
class _Attn:
    wq: np.ndarray
    bq: np.ndarray
    wk: np.ndarray
    bk: np.ndarray
    wv: np.ndarray
    bv: np.ndarray
    wo: np.ndarray
    bo: np.ndarray
    ln_g: np.ndarray
    ln_b: np.ndarray


@dataclass
class _FFN:
    w1: np.ndarray
    b1: np.ndarray
    w2: np.ndarray
    b2: np.ndarray
    ln_g: np.ndarray
    ln_b: np.ndarray


@dataclass
class _EncLayer:
    attn: _Attn
    ffn: _FFN


@dataclass
class _DecLayer:
    self_attn: _Attn
    cross: _Attn
    ffn: _FFN


class XlitTransformer:
    """Loads an exported IndicXlit bundle (see scripts/convert_indicxlit.py --npz) and decodes words."""

    def __init__(self, model_dir: Path | str = DEFAULT_MODEL_DIR, *, max_positions: int = 256) -> None:
        model_dir = Path(model_dir)
        cfg = json.loads((model_dir / "config.json").read_text(encoding="utf-8"))
        self.cfg = cfg
        self.dim = int(cfg["dim"])
        self.heads = int(cfg["heads"])
        self.head_dim = self.dim // self.heads
        self.pre_norm = bool(cfg["pre_norm"])
        self.act = gelu if cfg["activation"].startswith("gelu") else relu
        self.embed_scale = math.sqrt(self.dim) if cfg.get("scale_embedding", True) else 1.0
        self.src_vocab: list[str] = json.loads((model_dir / "source_vocabulary.json").read_text(encoding="utf-8"))
        self.tgt_vocab: list[str] = json.loads((model_dir / "target_vocabulary.json").read_text(encoding="utf-8"))
        self.src_index = {t: i for i, t in enumerate(self.src_vocab)}
        self.tgt_index = {t: i for i, t in enumerate(self.tgt_vocab)}
        self.pad = self.tgt_index["<pad>"]
        self.eos = self.tgt_index["</s>"]
        self.unk = self.tgt_index["<unk>"]
        self.src_eos = self.src_index["</s>"]
        self.src_unk = self.src_index["<unk>"]

        w = np.load(model_dir / "model.npz")
        f = {k: w[k].astype(np.float32) for k in w.files}
        self.enc_embed = f["encoder.embed_tokens.weight"]
        self.dec_embed = f["decoder.embed_tokens.weight"]
        self.out_proj = f.get("decoder.output_projection.weight", self.dec_embed)
        self.pos = sinusoidal(max_positions, self.dim, int(cfg.get("padding_idx", 1)))
        self.pos_offset = int(cfg.get("padding_idx", 1)) + 1
        self.enc_ln_emb = (f.get("encoder.layernorm_embedding.weight"), f.get("encoder.layernorm_embedding.bias"))
        self.dec_ln_emb = (f.get("decoder.layernorm_embedding.weight"), f.get("decoder.layernorm_embedding.bias"))
        self.enc_ln = (f.get("encoder.layer_norm.weight"), f.get("encoder.layer_norm.bias"))
        self.dec_ln = (f.get("decoder.layer_norm.weight"), f.get("decoder.layer_norm.bias"))

        def attn(p: str, ln: str) -> _Attn:
            return _Attn(
                f[f"{p}.q_proj.weight"].T.copy(), f[f"{p}.q_proj.bias"],
                f[f"{p}.k_proj.weight"].T.copy(), f[f"{p}.k_proj.bias"],
                f[f"{p}.v_proj.weight"].T.copy(), f[f"{p}.v_proj.bias"],
                f[f"{p}.out_proj.weight"].T.copy(), f[f"{p}.out_proj.bias"],
                f[f"{ln}.weight"], f[f"{ln}.bias"],
            )

        def ffn(p: str) -> _FFN:
            return _FFN(
                f[f"{p}.fc1.weight"].T.copy(), f[f"{p}.fc1.bias"],
                f[f"{p}.fc2.weight"].T.copy(), f[f"{p}.fc2.bias"],
                f[f"{p}.final_layer_norm.weight"], f[f"{p}.final_layer_norm.bias"],
            )

        self.enc_layers = [
            _EncLayer(attn(f"encoder.layers.{i}.self_attn", f"encoder.layers.{i}.self_attn_layer_norm"), ffn(f"encoder.layers.{i}"))
            for i in range(int(cfg["encoder_layers"]))
        ]
        self.dec_layers = [
            _DecLayer(
                attn(f"decoder.layers.{i}.self_attn", f"decoder.layers.{i}.self_attn_layer_norm"),
                attn(f"decoder.layers.{i}.encoder_attn", f"decoder.layers.{i}.encoder_attn_layer_norm"),
                ffn(f"decoder.layers.{i}"),
            )
            for i in range(int(cfg["decoder_layers"]))
        ]
        self.scaling = self.head_dim**-0.5

    # ------------------------------------------------------------------ building blocks

    def _split(self, x: np.ndarray) -> np.ndarray:
        # (..., T, dim) -> (..., heads, T, head_dim)
        *lead, t, _ = x.shape
        return x.reshape(*lead, t, self.heads, self.head_dim).swapaxes(-3, -2)

    def _merge(self, x: np.ndarray) -> np.ndarray:
        *lead, _h, t, _d = x.shape
        return x.swapaxes(-3, -2).reshape(*lead, t, self.dim)

    def _attend(self, q: np.ndarray, k: np.ndarray, v: np.ndarray, causal: bool = False) -> np.ndarray:
        # q: (..., h, Tq, d), k/v: (..., h, Tk, d)
        s = q @ k.swapaxes(-1, -2)
        if causal:
            tq, tk = s.shape[-2], s.shape[-1]
            mask = np.triu(np.ones((tq, tk), dtype=bool), k=tk - tq + 1)
            s = np.where(mask, -1e30, s)
        s = s - s.max(-1, keepdims=True)
        p = np.exp(s)
        p /= p.sum(-1, keepdims=True)
        return p @ v

    def _encode(self, src_ids: list[int]) -> tuple[np.ndarray, list[tuple[np.ndarray, np.ndarray]]]:
        x = self.embed_scale * self.enc_embed[src_ids] + self.pos[self.pos_offset : self.pos_offset + len(src_ids)]
        if self.enc_ln_emb[0] is not None:
            x = layer_norm(x, *self.enc_ln_emb)
        for L in self.enc_layers:
            a = L.attn
            h = layer_norm(x, a.ln_g, a.ln_b) if self.pre_norm else x
            q = self._split((h @ a.wq + a.bq) * self.scaling)
            k = self._split(h @ a.wk + a.bk)
            v = self._split(h @ a.wv + a.bv)
            h = self._merge(self._attend(q, k, v)) @ a.wo + a.bo
            x = x + h
            if not self.pre_norm:
                x = layer_norm(x, a.ln_g, a.ln_b)
            fnn = L.ffn
            h = layer_norm(x, fnn.ln_g, fnn.ln_b) if self.pre_norm else x
            h = self.act(h @ fnn.w1 + fnn.b1) @ fnn.w2 + fnn.b2
            x = x + h
            if not self.pre_norm:
                x = layer_norm(x, fnn.ln_g, fnn.ln_b)
        if self.enc_ln[0] is not None:
            x = layer_norm(x, *self.enc_ln)
        # Pre-project encoder keys/values for every decoder layer's cross attention.
        kv = []
        for L in self.dec_layers:
            c = L.cross
            kv.append((self._split(x @ c.wk + c.bk), self._split(x @ c.wv + c.bv)))
        return x, kv

    def _decode_step(
        self,
        tok: np.ndarray,  # (B,) token ids of the current step
        step: int,
        cache: list[list[np.ndarray] | None],  # per layer: [K (B,h,t,d), V (B,h,t,d)] or None
        enc_kv: list[tuple[np.ndarray, np.ndarray]],
    ) -> np.ndarray:
        x = self.embed_scale * self.dec_embed[tok][:, None, :] + self.pos[self.pos_offset + step][None, None, :]
        if self.dec_ln_emb[0] is not None:
            x = layer_norm(x, *self.dec_ln_emb)
        for i, L in enumerate(self.dec_layers):
            a = L.self_attn
            h = layer_norm(x, a.ln_g, a.ln_b) if self.pre_norm else x
            q = self._split((h @ a.wq + a.bq) * self.scaling)
            k = self._split(h @ a.wk + a.bk)
            v = self._split(h @ a.wv + a.bv)
            if cache[i] is None:
                cache[i] = [k, v]
            else:
                cache[i][0] = np.concatenate([cache[i][0], k], axis=2)
                cache[i][1] = np.concatenate([cache[i][1], v], axis=2)
            h = self._merge(self._attend(q, cache[i][0], cache[i][1])) @ a.wo + a.bo
            x = x + h
            if not self.pre_norm:
                x = layer_norm(x, a.ln_g, a.ln_b)
            c = L.cross
            h = layer_norm(x, c.ln_g, c.ln_b) if self.pre_norm else x
            q = self._split((h @ c.wq + c.bq) * self.scaling)
            ek, ev = enc_kv[i]
            h = self._merge(self._attend(q, ek[None], ev[None])) @ c.wo + c.bo
            x = x + h
            if not self.pre_norm:
                x = layer_norm(x, c.ln_g, c.ln_b)
            fnn = L.ffn
            h = layer_norm(x, fnn.ln_g, fnn.ln_b) if self.pre_norm else x
            h = self.act(h @ fnn.w1 + fnn.b1) @ fnn.w2 + fnn.b2
            x = x + h
            if not self.pre_norm:
                x = layer_norm(x, fnn.ln_g, fnn.ln_b)
        if self.dec_ln[0] is not None:
            x = layer_norm(x, *self.dec_ln)
        logits = x[:, 0, :] @ self.out_proj.T
        return log_softmax(logits)

    # ------------------------------------------------------------------ public API

    def encode_source(self, roman: str, lang: str = "bn") -> list[int]:
        ids = [self.src_index.get(f"__{lang}__", self.src_unk)]
        ids += [self.src_index.get(ch, self.src_unk) for ch in roman]
        ids.append(self.src_eos)
        return ids

    def beam_search(
        self, roman: str, *, lang: str = "bn", beam: int = 4, nbest: int | None = None, max_len: int | None = None, lenpen: float = 1.0
    ) -> list[tuple[str, float]]:
        """Return [(word, normalized_log_prob)] best first. Scores are fairseq-style: sum / len**lenpen."""
        nbest = nbest or beam
        if not roman:
            return []
        src = self.encode_source(roman, lang)
        max_len = max_len or min(60, 3 * len(roman) + 5)
        _, enc_kv = self._encode(src)

        banned = np.zeros(len(self.tgt_vocab), dtype=np.float32)
        banned[self.pad] = -np.inf
        banned[self.tgt_index["<s>"]] = -np.inf
        for t, i in self.tgt_index.items():
            if t.startswith("__") and t.endswith("__") or t.startswith("madeupword"):
                banned[i] = -np.inf

        # live hypotheses
        tokens = np.full((1,), self.eos, dtype=np.int64)  # decoder starts from </s>
        scores = np.zeros((1,), dtype=np.float32)
        seqs: list[list[int]] = [[]]
        cache: list[list[np.ndarray] | None] = [None] * len(self.dec_layers)
        finished: list[tuple[float, list[int]]] = []

        for step in range(max_len):
            lp = self._decode_step(tokens, step, cache, enc_kv) + banned  # (B, V)
            if step == 0:
                lp[:, self.eos] = -np.inf  # min_len = 1
            cand = scores[:, None] + lp  # (B, V)
            flat = cand.ravel()
            k = min(2 * beam, flat.size)
            top = np.argpartition(-flat, k - 1)[:k]
            top = top[np.argsort(-flat[top])]
            vsize = lp.shape[1]
            new_tokens: list[int] = []
            new_scores: list[float] = []
            new_seqs: list[list[int]] = []
            new_src: list[int] = []
            for idx in top:
                b, t = divmod(int(idx), vsize)
                sc = float(flat[idx])
                if not np.isfinite(sc):
                    continue
                if t == self.eos:
                    finished.append((sc / ((step + 1) ** lenpen), seqs[b]))
                    continue
                if len(new_tokens) < beam:
                    new_tokens.append(t)
                    new_scores.append(sc)
                    new_seqs.append(seqs[b] + [t])
                    new_src.append(b)
            if len(finished) >= beam or not new_tokens:
                break
            # Early stop: best possible normalized live score cannot beat worst kept finished score.
            if len(finished) >= nbest:
                best_live = max(new_scores) / ((step + 2) ** lenpen)
                if best_live < sorted(f[0] for f in finished)[-nbest]:
                    break
            sel = np.asarray(new_src)
            tokens = np.asarray(new_tokens, dtype=np.int64)
            scores = np.asarray(new_scores, dtype=np.float32)
            seqs = new_seqs
            for i in range(len(cache)):
                if cache[i] is not None:
                    cache[i][0] = cache[i][0][sel]
                    cache[i][1] = cache[i][1][sel]
        if not finished:  # ran out of length: take live hyps
            finished = [(float(s) / (max_len**lenpen), q) for s, q in zip(scores, seqs, strict=True)]
        finished.sort(key=lambda x: -x[0])
        out: list[tuple[str, float]] = []
        seen: set[str] = set()
        for sc, q in finished:
            word = "".join(self.tgt_vocab[i] for i in q if i != self.unk)
            if word and word not in seen:
                seen.add(word)
                out.append((word, sc))
            if len(out) >= nbest:
                break
        return out


class IndicXlitNumpySystem:
    """Eval/engine adapter: roman -> candidates via the NumPy transformer, optional unigram rerank."""

    name = "indicxlit-np"

    def __init__(
        self,
        model_dir: Path | str = DEFAULT_MODEL_DIR,
        *,
        beam: int = 4,
        rescore: bool = False,
        alpha: float = 0.9,
        word_prob_path: Path | str | None = None,
        cache_size: int = 8192,
    ) -> None:
        self.model = XlitTransformer(model_dir)
        self.beam = beam
        self.alpha = alpha
        self.word_prob: dict[str, float] | None = None
        if rescore:
            p = Path(word_prob_path or (Path(model_dir).parents[1] / "data" / "raw" / "indicxlit" / "word_prob_dicts" / "ben_word_prob_dict.json"))
            self.word_prob = {canonical(k): float(v) for k, v in json.loads(Path(p).read_text(encoding="utf-8")).items()}
            self.name = "indicxlit-np+rerank"
        self._cached = lru_cache(maxsize=cache_size)(self._suggest)

    def _suggest(self, roman: str, k: int) -> tuple[str, ...]:
        hyps = self.model.beam_search(roman, beam=max(self.beam, k), nbest=max(self.beam, k))
        hyps = [(canonical(w), s) for w, s in hyps]
        if not hyps:
            return ()
        if self.word_prob is None:
            return tuple(w for w, _ in hyps)[:k]
        model_p = np.exp(np.array([s for _, s in hyps], dtype=np.float64))
        model_p /= model_p.sum() or 1.0
        lm_p = np.array([self.word_prob.get(w, 0.0) for w, _ in hyps], dtype=np.float64)
        if lm_p.sum() > 0:
            lm_p /= lm_p.sum()
        mixed = self.alpha * model_p + (1 - self.alpha) * lm_p
        order = np.argsort(-mixed, kind="stable")
        return tuple(hyps[i][0] for i in order)[:k]

    def suggest(self, roman: str, context: Sequence[str] = (), k: int = 5) -> list[str]:
        return list(self._cached(normalize_roman(roman), k))
