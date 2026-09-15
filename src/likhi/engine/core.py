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

from likhi.engine.romankey import key_from_bangla, key_from_roman
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
    gap: int = 0  # for completions: how many extra letters/key units beyond what was typed
    xlit_rank: int = 0  # 1..n if produced by the beam, else 0
    xlit_logp: float = float("nan")  # log P(word | roman) from the model (teacher forced)
    avro: bool = False
    lex_score: int = 0  # corpus score (wiki + 3 subs + 20 chat)
    in_lexicon: bool = False
    is_latin: bool = False  # the raw typed string offered as an English passthrough
    personal_sel: float = 0.0  # decayed count of times the user chose this word for this roman
    personal_share: float = 0.0  # that count as a share of all choices for this roman
    personal_word: float = 0.0  # decayed count of times the user chose this word at all
    sources: set[str] = field(default_factory=set)


DEFAULT_WEIGHTS = {
    "unigram": 1.0,
    "rom_exact": 2.5,
    "rom_exact_log": 0.8,
    "rom_exact_fast": 2.5,  # extra weight on attested spellings in the model-free fast ranking
    "rom_prefix": 0.3,
    "key_fine": 1.2,
    "key_coarse": 0.6,
    "key_prefix": -1.0,
    "gap": -0.4,  # per extra unit a completion adds beyond the typed input
    "xlit_logp": 0.5,  # on the raw log P(word | roman); candidates share the same roman, so no length norm
    "xlit_top1": 1.5,
    "xlit_top3": 0.7,
    "avro": 0.8,
    "oov": -3.0,
    "personal_sel": 5.0,
    "personal_word": 0.6,
    "bigram": 0.7,  # weight on log P(word | previous word) relative to the unigram estimate
    "latin_base": -6.0,  # stands in for the unigram term of a raw-Latin candidate
    "latin": 0.0,
}


UNKNOWN_CONFIDENT_LOGP = -11.0  # log-prior given to an unknown word the model is sure about


class AbortedError(Exception):
    """Raised when a suggestion computation was abandoned because newer input arrived."""


def personal_bonus(w: dict[str, float], count: float, share: float) -> float:
    """Evidence from the user's own picks for this exact roman string.

    Grows with the log of the pick count, scaled by how consistently this word was the choice.
    Calibrated so one pick never flips a strongly established word, two picks flip a close call,
    and about four consistent picks flip anything.
    """
    return w["personal_sel"] * math.log1p(count) * (0.5 + share)


class LikhiEngine:
    def __init__(
        self,
        lexicon_dir: Path | str = DEFAULT_LEXICON,
        xlit_dir: Path | str | None = DEFAULT_XLIT,
        *,
        weights: dict[str, float] | None = None,
        beam: int = 4,
        max_per_channel: int = 30,
        model_scored: int = 16,
        use_avro: bool = True,
        use_xlit: bool = True,
        personal: object | None = None,
        personal_path: Path | str | None = None,
    ) -> None:
        """``personal``: a PersonalStore, or None to disable learning (evaluation runs).
        ``personal_path``: create a PersonalStore at this path (default location when "default")."""
        import marisa_trie

        lexicon_dir = Path(lexicon_dir)
        self.uni = marisa_trie.RecordTrie("<III")
        self.uni.load(str(lexicon_dir / "unigrams.marisa"))
        self.romans = marisa_trie.RecordTrie("<Ib")
        self.romans.load(str(lexicon_dir / "romans.marisa"))
        self.keys = marisa_trie.RecordTrie("<I")
        self.keys.load(str(lexicon_dir / "keys.marisa"))
        self.prefixes = None
        if (lexicon_dir / "prefixes.marisa").exists():
            self.prefixes = marisa_trie.RecordTrie("<I")
            self.prefixes.load(str(lexicon_dir / "prefixes.marisa"))
        self.bigrams = None
        self.bigram_totals = None
        if (lexicon_dir / "bigrams.marisa").exists():
            self.bigrams = marisa_trie.RecordTrie("<I")
            self.bigrams.load(str(lexicon_dir / "bigrams.marisa"))
            self.bigram_totals = marisa_trie.RecordTrie("<I")
            self.bigram_totals.load(str(lexicon_dir / "bigram_totals.marisa"))
        # normalizer for the unigram mixture
        total = 0
        for _w, (a, b, c) in self.uni.items():
            total += a + 3 * b + 20 * c
        self._uni_total = float(total) + 1.0
        self._floor = math.log(0.5 / self._uni_total)

        self.w = dict(DEFAULT_WEIGHTS)
        tuned = lexicon_dir / "weights.json"
        if tuned.exists():  # written by likhi-tune
            import json

            self.w.update(json.loads(tuned.read_text(encoding="utf-8")))
        self.w.update(weights or {})
        self.beam = beam
        self.max_per_channel = max_per_channel
        self.model_scored = model_scored
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
        self.personal = personal
        if personal is None and personal_path is not None:
            from likhi.engine.personal import PersonalStore

            self.personal = PersonalStore(None if personal_path == "default" else personal_path)
        self._suggest_cached = lru_cache(maxsize=4096)(self._suggest)

    # ------------------------------------------------------------------ learning

    def learn(self, roman: str, chosen: str, context: Sequence[str] = ()) -> None:
        """Record a commit. Called by the shell whenever the user commits a candidate."""
        if self.personal is None or not roman or not chosen:
            return
        self.personal.learn(roman, chosen, tuple(context))
        self._suggest_cached.cache_clear()

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

    def context_adjust(self, word: str, context: Sequence[str]) -> float:
        """log P(word | prev) - log P(word): how much the previous word changes the odds.

        Stupid backoff with a 0.4 penalty; a previous word without any bigram data gives 0 for
        every candidate, so context never hurts when it is uninformative.
        """
        if self.bigrams is None or not context:
            return 0.0
        prev = context[-1]
        tot = self.bigram_totals.get(prev)
        if not tot:
            return 0.0
        total = float(tot[0][0])
        rec = self.bigrams.get(f"{prev}\t{word}")
        if rec:
            return math.log(rec[0][0] / total) - self.unigram_logp(word)
        return math.log(0.4)

    def candidates(
        self,
        roman: str,
        *,
        model_scored: int | None = None,
        static_prescore: bool = False,
        use_model: bool = True,
        abort=None,
    ) -> dict[str, Feats]:
        """Gather candidates from all channels and attach features.

        ``model_scored`` overrides how many pre-ranked candidates get a model score;
        ``static_prescore`` pre-ranks with a weight-independent heuristic (used when tuning weights,
        so cached features do not depend on the weights being tuned);
        ``use_model=False`` is the fast path (tries + rules only, a few milliseconds) used while a
        key is being typed; the full path fills in behind it. ``abort`` is an optional callable
        checked between the expensive stages; when it returns True the work is dropped and
        ``AbortedError`` is raised (the caller has moved on to a newer input).
        """
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
        if self.prefixes is not None and len(r) <= 3:
            # short input: precomputed top completions instead of scanning thousands of entries
            for key, (score,) in self.prefixes.items(f"r:{r}\t"):
                word = key.split("\t", 1)[1]
                completions.append((score, word, 1, 2))
        else:
            for key, (count, _src) in self.romans.items(r):
                rom, word = key.split("\t", 1)
                if rom == r:
                    continue
                completions.append(
                    (
                        count * 10_000 + min(self._lex_score(word), 9_999),
                        word,
                        count,
                        len(rom) - len(r),
                    )
                )
        completions.sort(reverse=True)
        for _s, word, count, gap in completions[: self.max_per_channel]:
            ft = f(word)
            if not ft.rom_exact:
                ft.rom_prefix += count
                ft.gap = gap if not ft.gap else min(ft.gap, gap)
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
            if self.prefixes is not None and len(k) <= 2:
                for key, (score,) in self.keys.items(prefix + "\t"):
                    exact.append((score, key.split("\t", 1)[1]))
                for key, (score,) in self.prefixes.items(f"k:{level[0]}:{k}\t"):
                    longer.append((score, key.split("\t", 1)[1]))
            else:
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
                if not ft.rom_exact and not getattr(ft, attr):
                    g = max(1, len(key_from_bangla(word, level)) - len(k))
                    ft.gap = g if not ft.gap else min(ft.gap, g)
                ft.sources.add(f"key-{level}+")

        # 3. transliteration model (encoder shared with the scoring pass below)
        enc_kv = None
        if self.xlit is not None and use_model:
            if abort is not None and abort():
                raise AbortedError(r)
            enc_kv = self.xlit.encode(r)
            for rank, (word, lp) in enumerate(
                self.xlit.beam_search(r, beam=self.beam, nbest=self.beam, enc_kv=enc_kv), 1
            ):
                ft = f(word)
                ft.xlit_rank = ft.xlit_rank or rank
                # The beam already knows log P(word | roman): its scores are length-normalized
                # (sum / (chars + EOS)), so undo that to match score_candidates' raw sums.
                ft.xlit_logp = float(lp) * (len(word) + 1)
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

        # 5. personal history: previously chosen words for this roman (including the raw Latin)
        if self.personal is not None:
            sel = self.personal.selections(r)
            total = sum(sel.values()) or 1.0
            for word, count in sel.items():
                ft = f(word)
                if word == r or word == roman:
                    ft.is_latin = True
                ft.personal_sel = count
                ft.personal_share = count / total
                ft.sources.add("personal")
            for word, ft in feats.items():
                if not ft.is_latin:
                    ft.personal_word = self.personal.word_count(word)

        # Model scores for the most promising candidates only (batched teacher forcing is the
        # expensive step; everything else is a trie lookup).
        if self.xlit is not None and use_model and feats:
            if abort is not None and abort():
                raise AbortedError(r)
            n = self.model_scored if model_scored is None else model_scored
            unscored = {w: ft for w, ft in feats.items() if ft.xlit_logp != ft.xlit_logp}
            if static_prescore:
                pre = sorted(unscored.items(), key=lambda kv: -self._static_prescore(kv[0], kv[1]))
            else:
                pre = sorted(unscored.items(), key=lambda kv: -self.score(kv[0], kv[1], r))
            words = [w for w, _ in pre[:n]]
            if words:
                lps = self.xlit.score_candidates(r, words, enc_kv=enc_kv)
                for word, lp in zip(words, lps, strict=True):
                    feats[word].xlit_logp = float(lp)
        return feats

    def _static_prescore(self, word: str, ft: Feats) -> float:
        """Weight-independent ranking used to pick which candidates deserve a model score."""
        s = self.unigram_logp(word)
        s += 3.0 * bool(ft.rom_exact) + 0.5 * math.log1p(ft.rom_exact)
        s += 1.5 * ft.key_fine + 0.8 * ft.key_coarse
        s += 2.5 * bool(ft.xlit_rank) + 1.0 * ft.avro
        s -= (
            1.0
            * (ft.key_fine_prefix or ft.key_coarse_prefix or bool(ft.rom_prefix))
            * (not ft.rom_exact)
        )
        return s

    # ------------------------------------------------------------------ ranking

    def score(self, word: str, ft: Feats, roman: str, context: Sequence[str] = ()) -> float:
        w = self.w
        if ft.is_latin:
            # Raw Latin is only a candidate once the user has chosen it before; it competes on
            # personal evidence, not on Bangla corpus frequency.
            return (
                w["latin_base"] + w["latin"] + personal_bonus(w, ft.personal_sel, ft.personal_share)
            )
        uni = self.unigram_logp(word)
        xl = ft.xlit_logp
        if not ft.in_lexicon and xl == xl:
            # Unknown words sit at the unigram floor, a hidden second penalty. When the model is
            # confident the word is real, lift the prior towards that of a rare-but-real word.
            confidence = 1.0 - min(1.0, max(0.0, -xl / 3.0))
            uni = uni + (UNKNOWN_CONFIDENT_LOGP - uni) * confidence
        s = w["unigram"] * uni
        if context:
            s += w["bigram"] * self.context_adjust(word, context)
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
            s += w["xlit_logp"] * max(-40.0, ft.xlit_logp)
        else:
            # not scored by the model: assume a poor fit so scored candidates can outrank it
            s += w["xlit_logp"] * -15.0
        if ft.gap and not ft.rom_exact:
            s += w["gap"] * ft.gap
        if ft.xlit_rank == 1:
            s += w["xlit_top1"]
        elif 1 < ft.xlit_rank <= 3:
            s += w["xlit_top3"]
        if ft.avro:
            s += w["avro"]
        if not ft.in_lexicon:
            # The unknown-word penalty encodes "probably not a real word". A confident model is
            # evidence to the contrary: at log P > -3 the penalty fades, at log P ~ 0 it vanishes
            # (খাইতেছো for "khaitecho" is unknown to the lexicon but certain for the model).
            xl = ft.xlit_logp
            scale = 1.0 if xl != xl else min(1.0, max(0.0, -xl / 3.0))
            s += w["oov"] * scale
        if ft.personal_sel:
            s += personal_bonus(w, ft.personal_sel, ft.personal_share)
        if ft.personal_word:
            s += w["personal_word"] * math.log1p(ft.personal_word)
        return s

    def _suggest(
        self, roman: str, k: int, context: tuple[str, ...] = (), use_model: bool = True, abort=None
    ) -> tuple[str, ...]:
        feats = self.candidates(roman, use_model=use_model, abort=abort)
        if not feats:
            return ()
        if use_model:
            ranked = sorted(feats.items(), key=lambda kv: -self.score(kv[0], kv[1], roman, context))
        else:
            # Model-free ranking: attested spellings are the best evidence we have, so weigh them
            # more than in the full ranking (where the model score does that job).
            w = self.w["rom_exact_fast"]
            ranked = sorted(
                feats.items(),
                key=lambda kv: (
                    -(self.score(kv[0], kv[1], roman, context) + w * math.log1p(kv[1].rom_exact))
                ),
            )
        return tuple(to_output(word) for word, _ in ranked[:k])

    def has_strong_match(self, roman: str) -> bool:
        """True when the trie channels alone have solid evidence (an attested romanization or an
        exact phonetic-key match for a lexicon word). When False, the quick model-free answer is
        probably poor and callers should wait for the model."""
        feats = self.candidates(roman, use_model=False)
        # Phonetic-key matches alone are not enough: "khacche" key-matches কিছু/কাছে, which would
        # be shown while the model's খাচ্ছে is still computing. Attested spellings are reliable.
        return any(ft.rom_exact >= 2 for ft in feats.values())

    def suggest(
        self,
        roman: str,
        context: Sequence[str] = (),
        k: int = 5,
        *,
        fast: bool = False,
        abort=None,
    ) -> list[str]:
        """Ranked candidates. ``fast=True`` skips the transliteration model (see candidates()).
        ``abort`` (callable) lets a caller abandon a full computation that became stale."""
        prev = (canonical(context[-1]),) if context else ()
        if abort is not None:
            # abortable computations bypass the cache key (a callable is not hashable/stable)
            out = self._suggest(normalize_roman(roman), k, prev, not fast, abort)
            return list(out)
        return list(self._suggest_cached(normalize_roman(roman), k, prev, not fast))

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
