"""IndicXlit transliteration model served by CTranslate2 (CPU, int8).

This is the "generative channel": given a roman string it proposes Bengali spellings, including for
words no lexicon has. Optional rescoring with AI4Bharat's word-probability dictionary reproduces
their published "+ unigram re-ranking" setting so our numbers stay comparable.
"""

from __future__ import annotations

import json
import math
import os
from collections.abc import Sequence
from functools import lru_cache
from pathlib import Path

from likhi.engine.textnorm import canonical, normalize_roman

DEFAULT_MODEL_DIR = Path(__file__).resolve().parents[3] / "models" / "indicxlit-ct2"
DEFAULT_WORD_PROB = (
    Path(__file__).resolve().parents[3]
    / "data"
    / "raw"
    / "indicxlit"
    / "word_prob_dicts"
    / "bn_word_prob_dict.json"
)


class IndicXlitSystem:
    name = "indicxlit"

    def __init__(
        self,
        model_dir: Path | str | None = None,
        *,
        lang: str = "bn",
        beam_size: int = 4,
        rescore: bool = False,
        alpha: float = 0.9,
        word_prob_path: Path | str | None = None,
        intra_threads: int = 2,
        compute_type: str = "default",
        cache_size: int = 4096,
    ) -> None:
        import ctranslate2

        model_dir = Path(model_dir or os.environ.get("LIKHI_XLIT_MODEL", DEFAULT_MODEL_DIR))
        self.translator = ctranslate2.Translator(
            str(model_dir),
            device="cpu",
            compute_type=compute_type,
            inter_threads=1,
            intra_threads=intra_threads,
        )
        self.lang_token = f"__{lang}__"
        self.beam_size = beam_size
        self.alpha = alpha
        self.rescore = rescore
        self.word_prob: dict[str, float] | None = None
        if rescore:
            p = Path(word_prob_path or DEFAULT_WORD_PROB)
            with open(p, encoding="utf-8") as f:
                raw = json.load(f)
            self.word_prob = {canonical(k): float(v) for k, v in raw.items()}
        if rescore:
            self.name = "indicxlit+rerank"
        self._suggest_cached = lru_cache(maxsize=cache_size)(self._suggest_uncached)

    def _tokens(self, roman: str) -> list[str]:
        return [self.lang_token, *list(roman)]

    def raw_hypotheses(self, roman: str, n: int) -> list[tuple[str, float]]:
        """Return [(bengali, log_prob)] best first, deduplicated after normalization."""
        roman = normalize_roman(roman)
        if not roman:
            return []
        res = self.translator.translate_batch(
            [self._tokens(roman)],
            beam_size=max(self.beam_size, n),
            num_hypotheses=max(self.beam_size, n),
            return_scores=True,
            max_decoding_length=max(8, 3 * len(roman) + 4),
        )[0]
        seen: set[str] = set()
        out: list[tuple[str, float]] = []
        for toks, score in zip(res.hypotheses, res.scores, strict=True):
            word = canonical(
                "".join(t for t in toks if not (t.startswith("__") and t.endswith("__")))
            )
            if not word or word in seen:
                continue
            seen.add(word)
            # CTranslate2 returns length-normalized log-probs; undo normalization to get a joint score.
            out.append((word, score * max(1, len(toks))))
        return out[:n]

    def _suggest_uncached(self, roman: str, k: int) -> tuple[str, ...]:
        hyps = self.raw_hypotheses(roman, k)
        if not hyps:
            return ()
        if not self.rescore or not self.word_prob:
            return tuple(w for w, _ in hyps)
        # AI4Bharat-style re-ranking: normalize model probabilities and corpus word probabilities
        # over the candidate list, then mix with alpha.
        model_p = [math.exp(s) for _, s in hyps]
        z = sum(model_p) or 1.0
        model_p = [p / z for p in model_p]
        lm_p = [self.word_prob.get(w, 0.0) for w, _ in hyps]
        zl = sum(lm_p)
        lm_p = [p / zl for p in lm_p] if zl > 0 else [0.0] * len(lm_p)
        mixed = [
            self.alpha * m + (1 - self.alpha) * lp for m, lp in zip(model_p, lm_p, strict=True)
        ]
        order = sorted(range(len(hyps)), key=lambda i: -mixed[i])
        return tuple(hyps[i][0] for i in order)

    def suggest(self, roman: str, context: Sequence[str] = (), k: int = 5) -> list[str]:
        return list(self._suggest_cached(normalize_roman(roman), k))
