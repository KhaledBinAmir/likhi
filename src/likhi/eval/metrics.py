"""Metrics for transliteration candidates.

All comparisons go through `textnorm.match_key`, so nukta encoding, joiners and candrabindu
ordering never count as errors. Gold may be a set of acceptable spellings (e.g. জন্য / জন্যে).
"""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field

from likhi.engine.textnorm import match_key


def edit_distance(a: Sequence[str], b: Sequence[str]) -> int:
    """Levenshtein distance between two sequences (characters or tokens)."""
    if a == b:
        return 0
    if not a:
        return len(b)
    if not b:
        return len(a)
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def cer(hyp: str, ref: str) -> float:
    """Character error rate of hyp against ref (0 when equal, may exceed 1)."""
    h, r = match_key(hyp), match_key(ref)
    if not r:
        return 0.0 if not h else 1.0
    return edit_distance(h, r) / len(r)


def rank_of_gold(candidates: Sequence[str], golds: Iterable[str]) -> int | None:
    """1-based rank of the first candidate equal to any gold, or None."""
    gold_keys = {match_key(g) for g in golds}
    for i, c in enumerate(candidates, 1):
        if match_key(c) in gold_keys:
            return i
    return None


@dataclass
class WordEval:
    """Accumulates word-level results, optionally weighted (e.g. Dakshina attestation counts)."""

    ks: tuple[int, ...] = (1, 3, 5)
    weight_total: float = 0.0
    hits: dict[int, float] = field(default_factory=dict)
    rr_total: float = 0.0
    cer_total: float = 0.0
    n: int = 0

    def add(self, candidates: Sequence[str], golds: Sequence[str], weight: float = 1.0) -> int | None:
        rank = rank_of_gold(candidates, golds)
        self.n += 1
        self.weight_total += weight
        for k in self.ks:
            if rank is not None and rank <= k:
                self.hits[k] = self.hits.get(k, 0.0) + weight
        if rank is not None:
            self.rr_total += weight / rank
        top1 = candidates[0] if candidates else ""
        self.cer_total += weight * min(cer(top1, g) for g in golds)
        return rank

    def summary(self) -> dict[str, float]:
        if self.weight_total == 0:
            return {"n": 0}
        out: dict[str, float] = {"n": self.n}
        for k in self.ks:
            out[f"top{k}"] = 100.0 * self.hits.get(k, 0.0) / self.weight_total
        out["mrr"] = self.rr_total / self.weight_total
        out["cer"] = 100.0 * self.cer_total / self.weight_total
        return out


def wer(hyp_tokens: Sequence[str], ref_tokens: Sequence[str]) -> tuple[int, int]:
    """Return (errors, reference length) so callers can aggregate corpus-level WER."""
    h = [match_key(t) for t in hyp_tokens]
    r = [match_key(t) for t in ref_tokens]
    return edit_distance(h, r), len(r)


def percentile(values: Sequence[float], p: float) -> float:
    """Nearest-rank percentile (p in 0..100) without numpy."""
    if not values:
        return 0.0
    s = sorted(values)
    k = max(0, min(len(s) - 1, round(p / 100.0 * len(s) + 0.5) - 1))
    return s[k]
