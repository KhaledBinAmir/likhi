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
import os
from collections import OrderedDict
from collections.abc import Sequence
from dataclasses import dataclass, field
from pathlib import Path

from likhi import lkx
from likhi.engine.romankey import key_from_bangla, key_from_roman
from likhi.engine.textnorm import canonical, normalize_roman, to_output

REPO = Path(__file__).resolve().parents[3]
# LIKHI_MODELS lets a packaged runtime point at its bundled data, where the repository layout
# (models/ beside src/) does not exist.
_MODELS = Path(os.environ["LIKHI_MODELS"]) if os.environ.get("LIKHI_MODELS") else REPO / "models"
DEFAULT_LEXICON = _MODELS / "lexicon"
DEFAULT_XLIT = _MODELS / "indicxlit-np"


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

# How well attested a spelling must be before the fast path stops trusting candidates that agree
# with nothing about the typed string. Chosen by measurement: at 5 the misaligned pairs that were
# reaching the visible list disappear, top-1 is unchanged on every set, and top-5 moves by -0.02 on
# Dakshina and -0.35 on the chat set.
FAST_TRUST_ROM_EXACT = 5

# Bengali spellings of English letter names. The transliteration model reads vowel-less shorthand
# such as "amr" or "tmi" as an acronym (এএমআর, টিএমআই) with high confidence; those readings must
# not enjoy the confident-unknown-word relief, otherwise they beat আমার / তুমি.
_LETTER_NAMES = (
    "ডব্লিউ",
    "এইচ",
    "কিউ",
    "এক্স",
    "ওয়াই",
    "জেড",
    "এফ",
    "এম",
    "এন",
    "এল",
    "এস",
    "আর",
    "বি",
    "সি",
    "ডি",
    "ই",
    "জি",
    "জে",
    "কে",
    "পি",
    "টি",
    "ইউ",
    "ভি",
    "ও",
    "আই",
    "এ",
)


def _fast_supported(ft: "Feats") -> bool:
    """True when something other than a lone attested pair vouches for this candidate.

    Any phonetic agreement counts, exact or as a prefix, as does Avro's rule-based reading and a
    romanization seen more than once. What fails this test is a candidate whose entire case is one
    row of aligned training data -- the part of that data most likely to be a misalignment.
    """
    return bool(
        ft.key_fine
        or ft.key_coarse
        or ft.key_fine_prefix
        or ft.key_coarse_prefix
        or ft.avro
        or ft.rom_prefix
        or ft.rom_exact >= 2
    )


def looks_like_acronym(word: str) -> bool:
    """True when the word can be segmented into two or more English letter names."""
    i, n, parts = 0, len(word), 0
    while i < n:
        for name in _LETTER_NAMES:
            if word.startswith(name, i):
                i += len(name)
                parts += 1
                break
        else:
            return False
    return parts >= 2


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
        lexicon_dir = Path(lexicon_dir)

        # Two table formats, chosen per directory. `.lkx` is what the Rust engine reads and what
        # the Rust lexicon builder emits; `.marisa` is the original. Both are supported so that the
        # two engines can be measured on identical data while the builder is being moved over --
        # without this, a lexicon built by the Rust side could not be evaluated at all, because the
        # evaluation harness drives this engine.
        use_lkx = lkx.exists(lexicon_dir, "unigrams")

        def load(name: str, fmt: str, optional: bool = False):
            if use_lkx:
                if optional and not lkx.exists(lexicon_dir, name):
                    return None
                return lkx.open_table(lexicon_dir, name, fmt)
            import marisa_trie

            path = lexicon_dir / f"{name}.marisa"
            if optional and not path.exists():
                return None
            trie = marisa_trie.RecordTrie(fmt)
            trie.load(str(path))
            return trie

        self.lexicon_format = "lkx" if use_lkx else "marisa"
        self.uni = load("unigrams", "<III")
        self.romans = load("romans", "<Ib")
        self.keys = load("keys", "<I")
        self.prefixes = load("prefixes", "<I", optional=True)
        self.bigrams = load("bigrams", "<I", optional=True)
        self.bigram_totals = (
            load("bigram_totals", "<I", optional=True) if self.bigrams is not None else None
        )
        # normalizer for the unigram mixture
        total = 0
        for _w, (a, b, c) in self.uni.items():
            total += a + 3 * b + 20 * c
        self._uni_total = float(total) + 1.0
        self._floor = math.log(0.5 / self._uni_total)

        self.w = dict(DEFAULT_WEIGHTS)
        import json

        # LIKHI_WEIGHTS points at an alternative weights file (A/B evaluation of candidate weights).
        tuned = Path(os.environ.get("LIKHI_WEIGHTS") or (lexicon_dir / "weights.json"))
        if tuned.exists():  # written by likhi-tune
            self.w.update(json.loads(tuned.read_text(encoding="utf-8")))
        self.w.update(weights or {})
        self.beam = int(os.environ.get("LIKHI_BEAM") or beam)
        self.max_per_channel = max_per_channel
        # How many non-beam candidates get a teacher-forced model score. Beam hypotheses already
        # carry their own score, so this only covers lexicon/key/rule candidates.
        self.model_scored = int(os.environ.get("LIKHI_MODEL_SCORED") or model_scored)
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
        # Keyed by (roman, k, context, use_model). A plain dict rather than lru_cache so learn()
        # can drop just the entries for the word that changed, instead of the whole cache: every
        # committed word calls learn(), and clearing 4096 entries each time undoes the caching.
        self._cache: OrderedDict[tuple, tuple[str, ...]] = OrderedDict()
        self._cache_max = 4096

    # ------------------------------------------------------------------ learning

    def learn(self, roman: str, chosen: str, context: Sequence[str] = ()) -> None:
        """Record a commit. Called by the shell whenever the user commits a candidate."""
        if self.personal is None or not roman or not chosen:
            return
        r = normalize_roman(roman)
        self.personal.learn(r, chosen, tuple(context))
        for key in [k for k in self._cache if k[0] == r]:
            self._cache.pop(key, None)

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
        acronym = (not ft.in_lexicon) and looks_like_acronym(word)
        if not ft.in_lexicon and xl == xl and not acronym:
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
            # Acronym readings of shorthand keep the full penalty.
            scale = 1.0 if (xl != xl or acronym) else min(1.0, max(0.0, -xl / 3.0))
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
        """True when the trie channels alone have solid evidence (an attested spelling)."""
        return self.fast_suggest(roman, (), 1)[1]

    def fast_suggest(
        self, roman: str, context: Sequence[str] = (), k: int = 5
    ) -> tuple[list[str], bool]:
        """Model-free ranking plus a confidence flag, from one pass over the trie channels.

        The flag is True when some candidate is an attested spelling of the typed string (count
        >= 2). Phonetic-key matches alone are not enough: "khacche" key-matches কিছু/কাছে, which
        would be shown while the model's খাচ্ছে is still computing.

        Candidates that agree with nothing about the typed string are pushed behind those that do.
        The aligned romanization data contains a tail of misaligned pairs, and one of those plus a
        high unigram count is enough to reach the visible list with no model to contradict it: "কোন"
        and "হিসেবে" for bangla, "খনির" for sonar, each attested exactly once and matching neither
        phonetic key. That list is selectable, so a user pressing 4 committed a word they never
        typed. Only applied when something is well attested for this exact spelling, because when
        the best evidence is a single occurrence that candidate may be all there is, and demoted
        rather than removed so they still fill slots nothing better is competing for.
        """
        r = normalize_roman(roman)
        prev = (canonical(context[-1]),) if context else ()
        feats = self.candidates(r, use_model=False)
        if not feats:
            return [], False
        w = self.w["rom_exact_fast"]

        def base(word: str, ft: Feats) -> float:
            return self.score(word, ft, r, prev) + w * math.log1p(ft.rom_exact)

        demote = max(ft.rom_exact for ft in feats.values()) >= FAST_TRUST_ROM_EXACT
        ranked = sorted(
            feats.items(),
            key=lambda kv: (
                (0 if not demote or _fast_supported(kv[1]) else 1),
                -base(kv[0], kv[1]),
            ),
        )
        strong = any(ft.rom_exact >= 2 for ft in feats.values())
        return [to_output(word) for word, _ in ranked[:k]], strong

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
        r = normalize_roman(roman)
        prev = (canonical(context[-1]),) if context else ()
        if abort is not None:
            # abortable computations bypass the cache (they may be dropped part-way)
            return list(self._suggest(r, k, prev, not fast, abort))
        key = (r, k, prev, not fast)
        hit = self._cache.get(key)
        if hit is not None:
            self._cache.move_to_end(key)
            return list(hit)
        out = self._suggest(r, k, prev, not fast)
        self._cache[key] = out
        if len(self._cache) > self._cache_max:
            self._cache.popitem(last=False)
        return list(out)

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
