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
    y = 1.0 - (
        ((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592
    ) * t * np.exp(-a * a)
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
    wqkv: np.ndarray | None = None  # fused [q|k|v] projection, q part pre-scaled
    bqkv: np.ndarray | None = None


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

    def __init__(
        self, model_dir: Path | str = DEFAULT_MODEL_DIR, *, max_positions: int = 256
    ) -> None:
        model_dir = Path(model_dir)
        cfg = json.loads((model_dir / "config.json").read_text(encoding="utf-8"))
        self.cfg = cfg
        self.dim = int(cfg["dim"])
        self.heads = int(cfg["heads"])
        self.head_dim = self.dim // self.heads
        self.pre_norm = bool(cfg["pre_norm"])
        self.act = gelu if cfg["activation"].startswith("gelu") else relu
        self.embed_scale = math.sqrt(self.dim) if cfg.get("scale_embedding", True) else 1.0
        self.src_vocab: list[str] = json.loads(
            (model_dir / "source_vocabulary.json").read_text(encoding="utf-8")
        )
        self.tgt_vocab: list[str] = json.loads(
            (model_dir / "target_vocabulary.json").read_text(encoding="utf-8")
        )
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
        self.enc_ln_emb = (
            f.get("encoder.layernorm_embedding.weight"),
            f.get("encoder.layernorm_embedding.bias"),
        )
        self.dec_ln_emb = (
            f.get("decoder.layernorm_embedding.weight"),
            f.get("decoder.layernorm_embedding.bias"),
        )
        self.enc_ln = (f.get("encoder.layer_norm.weight"), f.get("encoder.layer_norm.bias"))
        self.dec_ln = (f.get("decoder.layer_norm.weight"), f.get("decoder.layer_norm.bias"))

        scaling = (self.dim // self.heads) ** -0.5

        def attn(p: str, ln: str) -> _Attn:
            wq = f[f"{p}.q_proj.weight"].T.copy()
            bq = f[f"{p}.q_proj.bias"]
            wk = f[f"{p}.k_proj.weight"].T.copy()
            bk = f[f"{p}.k_proj.bias"]
            wv = f[f"{p}.v_proj.weight"].T.copy()
            bv = f[f"{p}.v_proj.bias"]
            # One fused matmul for q, k, v; the scaling folded into the q columns.
            wqkv = np.ascontiguousarray(np.concatenate([wq * scaling, wk, wv], axis=1))
            bqkv = np.concatenate([bq * scaling, bk, bv])
            return _Attn(
                wq,
                bq,
                wk,
                bk,
                wv,
                bv,
                f[f"{p}.out_proj.weight"].T.copy(),
                f[f"{p}.out_proj.bias"],
                f[f"{ln}.weight"],
                f[f"{ln}.bias"],
                wqkv,
                bqkv,
            )

        def ffn(p: str) -> _FFN:
            return _FFN(
                f[f"{p}.fc1.weight"].T.copy(),
                f[f"{p}.fc1.bias"],
                f[f"{p}.fc2.weight"].T.copy(),
                f[f"{p}.fc2.bias"],
                f[f"{p}.final_layer_norm.weight"],
                f[f"{p}.final_layer_norm.bias"],
            )

        self.enc_layers = [
            _EncLayer(
                attn(f"encoder.layers.{i}.self_attn", f"encoder.layers.{i}.self_attn_layer_norm"),
                ffn(f"encoder.layers.{i}"),
            )
            for i in range(int(cfg["encoder_layers"]))
        ]
        self.dec_layers = [
            _DecLayer(
                attn(f"decoder.layers.{i}.self_attn", f"decoder.layers.{i}.self_attn_layer_norm"),
                attn(
                    f"decoder.layers.{i}.encoder_attn",
                    f"decoder.layers.{i}.encoder_attn_layer_norm",
                ),
                ffn(f"decoder.layers.{i}"),
            )
            for i in range(int(cfg["decoder_layers"]))
        ]
        self.scaling = self.head_dim**-0.5
        # Tokens the decoder must never emit: pad, bos, language tags, dictionary padding words.
        banned = np.zeros(len(self.tgt_vocab), dtype=np.float32)
        banned[self.pad] = -np.inf
        banned[self.tgt_index["<s>"]] = -np.inf
        for t, i in self.tgt_index.items():
            if (t.startswith("__") and t.endswith("__")) or t.startswith("madeupword"):
                banned[i] = -np.inf
        self._banned = banned

    # ------------------------------------------------------------------ building blocks

    def _split(self, x: np.ndarray) -> np.ndarray:
        # (..., T, dim) -> (..., heads, T, head_dim)
        *lead, t, _ = x.shape
        return x.reshape(*lead, t, self.heads, self.head_dim).swapaxes(-3, -2)

    def _merge(self, x: np.ndarray) -> np.ndarray:
        *lead, _h, t, _d = x.shape
        return x.swapaxes(-3, -2).reshape(*lead, t, self.dim)

    def _qkv(self, h: np.ndarray, a: _Attn) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """Fused q/k/v projection (q already scaled), split into heads."""
        qkv = h @ a.wqkv + a.bqkv
        d = self.dim
        return (
            self._split(qkv[..., :d]),
            self._split(qkv[..., d : 2 * d]),
            self._split(qkv[..., 2 * d :]),
        )

    def _attend(
        self, q: np.ndarray, k: np.ndarray, v: np.ndarray, causal: bool = False
    ) -> np.ndarray:
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
        x = (
            self.embed_scale * self.enc_embed[src_ids]
            + self.pos[self.pos_offset : self.pos_offset + len(src_ids)]
        )
        if self.enc_ln_emb[0] is not None:
            x = layer_norm(x, *self.enc_ln_emb)
        for L in self.enc_layers:
            a = L.attn
            h = layer_norm(x, a.ln_g, a.ln_b) if self.pre_norm else x
            q, k, v = self._qkv(h, a)
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
        x = (
            self.embed_scale * self.dec_embed[tok][:, None, :]
            + self.pos[self.pos_offset + step][None, None, :]
        )
        if self.dec_ln_emb[0] is not None:
            x = layer_norm(x, *self.dec_ln_emb)
        for i, L in enumerate(self.dec_layers):
            a = L.self_attn
            h = layer_norm(x, a.ln_g, a.ln_b) if self.pre_norm else x
            q, k, v = self._qkv(h, a)
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

    def _decode_full(
        self, tgt: np.ndarray, enc_kv: list[tuple[np.ndarray, np.ndarray]]
    ) -> np.ndarray:
        """Teacher-forced decoder pass over whole sequences. tgt: (B, T) input ids -> (B, T, V) log-probs."""
        b, t = tgt.shape
        x = (
            self.embed_scale * self.dec_embed[tgt]
            + self.pos[self.pos_offset : self.pos_offset + t][None]
        )
        if self.dec_ln_emb[0] is not None:
            x = layer_norm(x, *self.dec_ln_emb)
        for i, L in enumerate(self.dec_layers):
            a = L.self_attn
            h = layer_norm(x, a.ln_g, a.ln_b) if self.pre_norm else x
            q, k, v = self._qkv(h, a)
            h = self._merge(self._attend(q, k, v, causal=True)) @ a.wo + a.bo
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
        return log_softmax(x @ self.out_proj.T)

    def encode(self, roman: str, lang: str = "bn") -> list[tuple[np.ndarray, np.ndarray]]:
        """Run the encoder once; the result can be shared by beam_search and score_candidates."""
        return self._encode(self.encode_source(roman, lang))[1]

    def score_candidates(
        self,
        roman: str,
        words: Sequence[str],
        *,
        lang: str = "bn",
        enc_kv: list[tuple[np.ndarray, np.ndarray]] | None = None,
    ) -> np.ndarray:
        """log P(word | roman) for each word (sum over characters incl. </s>), in one batched pass.

        Words containing characters outside the target vocabulary get -inf.
        """
        if not words:
            return np.zeros(0, dtype=np.float32)
        if enc_kv is None:
            enc_kv = self.encode(roman, lang)
        ids: list[list[int]] = []
        ok = np.ones(len(words), dtype=bool)
        for i, w in enumerate(words):
            seq = []
            for ch in w:
                j = self.tgt_index.get(ch)
                if j is None:
                    ok[i] = False
                    break
                seq.append(j)
            ids.append(seq + [self.eos])
        t = max(len(s) for s in ids)
        b = len(ids)
        inp = np.full((b, t), self.pad, dtype=np.int64)
        out = np.full((b, t), self.pad, dtype=np.int64)
        inp[:, 0] = self.eos
        for i, s in enumerate(ids):
            n = len(s)
            out[i, :n] = s
            if n > 1:
                inp[i, 1:n] = s[:-1]
        lp = self._decode_full(inp, enc_kv)  # (B, T, V)
        gathered = np.take_along_axis(lp, out[:, :, None], axis=2)[:, :, 0]
        mask = out != self.pad
        total = (gathered * mask).sum(axis=1)
        total[~ok] = -np.inf
        return total.astype(np.float32)

    # ------------------------------------------------------------------ public API

    def encode_source(self, roman: str, lang: str = "bn") -> list[int]:
        ids = [self.src_index.get(f"__{lang}__", self.src_unk)]
        ids += [self.src_index.get(ch, self.src_unk) for ch in roman]
        ids.append(self.src_eos)
        return ids

    def beam_search(
        self,
        roman: str,
        *,
        lang: str = "bn",
        beam: int = 4,
        nbest: int | None = None,
        max_len: int | None = None,
        lenpen: float = 1.0,
        enc_kv: list[tuple[np.ndarray, np.ndarray]] | None = None,
    ) -> list[tuple[str, float]]:
        """Return [(word, normalized_log_prob)] best first. Scores are fairseq-style: sum / len**lenpen."""
        nbest = nbest or beam
        if not roman:
            return []
        max_len = max_len or min(60, 3 * len(roman) + 5)
        if enc_kv is None:
            enc_kv = self.encode(roman, lang)

        banned = self._banned

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
            for j, idx in enumerate(top):
                b, t = divmod(int(idx), vsize)
                sc = float(flat[idx])
                if not np.isfinite(sc):
                    continue
                if t == self.eos:
                    # fairseq only finalizes an EOS that ranks within the top `beam` candidates;
                    # lower-ranked EOS entries are dropped, otherwise weak short words fill the
                    # finished list and end the search before longer correct words complete.
                    if j < beam:
                        finished.append((sc / ((step + 1) ** lenpen), seqs[b]))
                    continue
                if len(new_tokens) < beam:
                    new_tokens.append(t)
                    new_scores.append(sc)
                    new_seqs.append(seqs[b] + [t])
                    new_src.append(b)
            if len(finished) >= beam or not new_tokens:
                break
            # No other early stop: a live hypothesis' length-normalized score can still improve as
            # it grows, so comparing it against finished ones is not a valid bound (it dropped the
            # model's own best answer, খাচ্ছে for "khacche", at beam 4).
            sel = np.asarray(new_src)
            tokens = np.asarray(new_tokens, dtype=np.int64)
            scores = np.asarray(new_scores, dtype=np.float32)
            seqs = new_seqs
            for i in range(len(cache)):
                if cache[i] is not None:
                    cache[i][0] = cache[i][0][sel]
                    cache[i][1] = cache[i][1][sel]
        if not finished:  # ran out of length: take live hyps
            finished = [
                (float(s) / (max_len**lenpen), q) for s, q in zip(scores, seqs, strict=True)
            ]
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
            p = Path(
                word_prob_path
                or (
                    Path(model_dir).parents[1]
                    / "data"
                    / "raw"
                    / "indicxlit"
                    / "word_prob_dicts"
                    / "bn_word_prob_dict.json"
                )
            )
            self.word_prob = {
                canonical(k): float(v)
                for k, v in json.loads(Path(p).read_text(encoding="utf-8")).items()
            }
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
