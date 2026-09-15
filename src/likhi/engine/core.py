"""Likhi engine v0: candidate generation from four channels + a log-linear ranker.

Channels (see docs/PLAN.md, section 2):
1. attested romanizations   roman -> word counts learned from Dakshina / Aksharantar / BanglaTLit
2. phonetic keys            fine and coarse consonant skeletons (handles "amr", "tmi", "korci")
3. transliteration model    IndicXlit beam search for unseen words; also scores every candidate
4. rule literal             Avro Phonetic output, so any spelling remains typeable

Ranking is a weighted sum of features; weights start hand-set and get tuned on dev sets.
Personalization and bigram context are Stage 2 and plug into `score()`.
"""

from __future__ import annotations

import math
from collections.abc import Sequence
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path

from likhi.engine.romankey import key_from_roman
from likhi.engine.textnorm import canonical, normalize_roman, to_output

REPO = Path(__file__).resolve().parents[3]
DEFAULT_LEXICON = REPO / "models" / "lexicon"
DEFAULT_XLIT = REPO / "models" / "indicxlit-np"


@dataclass
class Feats:
    rom_exact: int = 0  # attestation count for exactly this roman string
    rom_prefix: int = 0  # attestations where this roman is a strict prefix of the attested one
    key_fine: bool = False
    key_coarse: bool = False
    key_fine_prefix: bool = False
    key_coarse_prefix: bool = False
    xlit_rank: int = 0  # 1..n if produced by the beam, else 0
    xlit_logp: float = float("nan")  # log P(word | roman) from the model (teacher forced)
    avro: bool = False
    lex_score: int = 0  # corpus score (wiki + 3 subs + 20 chat)
    in_lexicon: bool = False
    sources: set[str] = field(default_factory=set)


DEFAULT_WEIGHTS = {
    "unigram": 1.0,
    "rom_exact": 2.5,
    "rom_exact_log": 0.8,
    "rom_prefix": 0.3,
    "key_fine": 1.2,
    "key_coarse": 0.6,
    "key_prefix": -1.0,
    "xlit_logp": 0.35,
    "xlit_top1": 1.5,
    "xlit_top3": 0.7,
    "avro": 0.8,
    "oov": -3.0,
}


class LikhiEngine:
    def __init__(
        self,
        lexicon_dir: Path | str = DEFAULT_LEXICON,
        xlit_dir: Path | str | None = DEFAULT_XLIT,
        *,
        weights: dict[str, float] | None = None,
        beam: int = 4,
        max_per_channel: int = 40,
        use_avro: bool = True,
        use_xlit: bool = True,
    ) -> None:
        import marisa_trie

        lexicon_dir = Path(lexicon_dir)
        self.uni = marisa_trie.RecordTrie("<III")
        self.uni.load(str(lexicon_dir / "unigrams.marisa"))
        self.romans = marisa_trie.RecordTrie("<Ib")
        self.romans.load(str(lexicon_dir / "romans.marisa"))
        self.keys = marisa_trie.RecordTrie("<I")
        self.keys.load(str(lexicon_dir / "keys.marisa"))
        # normalizer for the unigram mixture
        total = 0
        for _w, (a, b, c) in self.uni.items():
            total += a + 3 * b + 20 * c
        self._uni_total = float(total) + 1.0
        self._floor = math.log(0.5 / self._uni_total)

        self.w = dict(DEFAULT_WEIGHTS, **(weights or {}))
        self.beam = beam
        self.max_per_channel = max_per_channel
        self.xlit = None
        if use_xlit and xlit_dir is not None:
            from likhi.engine.xlit_np import XlitTransformer

            self.xlit = XlitTransformer(xlit_dir)
        self._avro = None
        if use_avro:
            try:
                import avro

                self._avro = avro.parse
            except Exception:
                self._avro = None
        self._suggest_cached = lru_cache(maxsize=4096)(self._suggest)

    # ------------------------------------------------------------------ features

    def unigram_logp(self, word: str) -> float:
        rec = self.uni.get(word)
        if not rec:
            return self._floor
        a, b, c = rec[0]
        return math.log((a + 3 * b + 20 * c + 0.5) / self._uni_total)

    def _lex_score(self, word: str) -> int:
        rec = self.uni.get(word)
        if not rec:
            return 0
        a, b, c = rec[0]
        return a + 3 * b + 20 * c

    def candidates(self, roman: str) -> dict[str, Feats]:
        r = normalize_roman(roman)
        feats: dict[str, Feats] = {}

        def f(word: str) -> Feats:
            word = canonical(word)
            if word not in feats:
                feats[word] = Feats(lex_score=self._lex_score(word), in_lexicon=word in self.uni)
            return feats[word]

        if not r:
            return feats

        # 1. attested romanizations: exact, then completions
        for key, (count, _src) in self.romans.items(r + "\t"):
            word = key.split("\t", 1)[1]
            ft = f(word)
            ft.rom_exact += count
            ft.sources.add("rom")
        completions = []
        for key, (count, _src) in self.romans.items(r):
            rom, word = key.split("\t", 1)
            if rom == r:
                continue
            completions.append((count + self._lex_score(word), word, count))
        completions.sort(reverse=True)
        for _s, word, count in completions[: self.max_per_channel]:
            ft = f(word)
            ft.rom_prefix += count
            ft.sources.add("rom+")

        # 2. phonetic keys
        for level, attr, pattr in (
            ("fine", "key_fine", "key_fine_prefix"),
            ("coarse", "key_coarse", "key_coarse_prefix"),
        ):
            k = key_from_roman(r, level)
            if not k:
                continue
            prefix = f"{level[0]}:{k}"
            exact = []
            longer = []
            for key, (score,) in self.keys.items(prefix):
                kk, word = key.split("\t", 1)
                (exact if kk == prefix else longer).append((score, word))
            exact.sort(reverse=True)
            longer.sort(reverse=True)
            for _s, word in exact[: self.max_per_channel]:
                ft = f(word)
                setattr(ft, attr, True)
                ft.sources.add(f"key-{level}")
            for _s, word in longer[: self.max_per_channel // 2]:
                ft = f(word)
                setattr(ft, pattr, True)
                ft.sources.add(f"key-{level}+")

        # 3. transliteration model
        if self.xlit is not None:
            for rank, (word, _lp) in enumerate(
                self.xlit.beam_search(r, beam=self.beam, nbest=self.beam), 1
            ):
                ft = f(word)
                ft.xlit_rank = ft.xlit_rank or rank
                ft.sources.add("xlit")

        # 4. rule literal
        if self._avro is not None:
            try:
                lit = self._avro(roman)
            except Exception:
                lit = ""
            if lit and lit != roman:
                ft = f(lit)
                ft.avro = True
                ft.sources.add("avro")

        # Model scores for every candidate (batched teacher forcing)
        if self.xlit is not None and feats:
            words = list(feats)
            lps = self.xlit.score_candidates(r, words)
            for word, lp in zip(words, lps, strict=True):
                feats[word].xlit_logp = float(lp)
        return feats

    # ------------------------------------------------------------------ ranking

    def score(self, word: str, ft: Feats, roman: str, context: Sequence[str] = ()) -> float:
        w = self.w
        s = w["unigram"] * self.unigram_logp(word)
        if ft.rom_exact:
            s += w["rom_exact"] + w["rom_exact_log"] * math.log(1 + ft.rom_exact)
        if ft.rom_prefix and not ft.rom_exact:
            s += w["rom_prefix"] * math.log(1 + ft.rom_prefix)
        if ft.key_fine:
            s += w["key_fine"]
        elif ft.key_coarse:
            s += w["key_coarse"]
        elif ft.key_fine_prefix or ft.key_coarse_prefix:
            s += w["key_prefix"]
        if ft.xlit_logp == ft.xlit_logp:  # not NaN
            # normalize by word length so long words are not punished
            s += w["xlit_logp"] * ft.xlit_logp / max(1, len(word))
        if ft.xlit_rank == 1:
            s += w["xlit_top1"]
        elif 1 < ft.xlit_rank <= 3:
            s += w["xlit_top3"]
        if ft.avro:
            s += w["avro"]
        if not ft.in_lexicon:
            s += w["oov"]
        return s

    def _suggest(self, roman: str, k: int) -> tuple[str, ...]:
        feats = self.candidates(roman)
        if not feats:
            return ()
        ranked = sorted(feats.items(), key=lambda kv: -self.score(kv[0], kv[1], roman))
        return tuple(to_output(word) for word, _ in ranked[:k])

    def suggest(self, roman: str, context: Sequence[str] = (), k: int = 5) -> list[str]:
        return list(self._suggest_cached(normalize_roman(roman), k))

    def explain(self, roman: str, k: int = 8) -> list[tuple[str, float, Feats]]:
        feats = self.candidates(roman)
        rows = [(w, self.score(w, ft, roman), ft) for w, ft in feats.items()]
        rows.sort(key=lambda x: -x[1])
        return rows[:k]


class LikhiSystem:
    name = "likhi"

    def __init__(self, **kwargs) -> None:
        self.engine = LikhiEngine(**kwargs)

    def suggest(self, roman: str, context: Sequence[str] = (), k: int = 5) -> list[str]:
        return self.engine.suggest(roman, context, k)
