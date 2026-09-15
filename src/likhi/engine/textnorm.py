"""Bengali and roman text normalization.

Why this exists (see docs/research/01-datasets-and-models.md, section 5):

* The nukta letters ড় (U+09DC), ঢ় (U+09DD), য় (U+09DF) are Unicode composition exclusions, so NFC
  *decomposes* them (ড + ়). Keyboards such as Avro emit the precomposed forms while corpora such as
  Dakshina are NFC. Comparing strings without normalizing gives false mismatches.
* Candrabindu (U+0981) must follow the vowel sign, but people often type it before; NFC does not
  reorder it because vowel signs have combining class 0.
* ZWJ/ZWNJ survive NFC. They matter for rendering (র‍্য) but must not affect lexicon matching.
* The obsolete khanda-ta sequence (ত + ্ + ZWJ) should be U+09CE.

Internal canonical form = NFC with the fixes above. Output form recomposes the nukta letters, which
is what most existing Bangla text and spell-checkers expect.
"""

from __future__ import annotations

import re
import unicodedata

ZWNJ = "‌"
ZWJ = "‍"
VIRAMA = "্"
NUKTA = "়"
CANDRABINDU = "ঁ"
KHANDA_TA = "ৎ"

# Precomposed nukta letters and their NFC (decomposed) spellings.
_NUKTA_PRECOMPOSED = {
    "ড়": "ড" + NUKTA,  # ড়
    "ঢ়": "ঢ" + NUKTA,  # ঢ়
    "য়": "য" + NUKTA,  # য়
}
_NUKTA_RECOMPOSE = {v: k for k, v in _NUKTA_PRECOMPOSED.items()}
_RE_NUKTA_DECOMPOSED = re.compile("|".join(map(re.escape, _NUKTA_RECOMPOSE)))

# Dependent vowel signs (including the two-part ones and the length mark used by NFC decomposition).
_VOWEL_SIGNS = "া-ৄেৈোৌৗৢৣ"
_RE_CANDRABINDU_BEFORE_VOWEL = re.compile(f"{CANDRABINDU}([{_VOWEL_SIGNS}]+)")

_RE_LEGACY_KHANDA_TA = re.compile("ত" + VIRAMA + ZWJ)
_RE_JOINERS = re.compile(f"[{ZWNJ}{ZWJ}]")

# Assamese letters that look like Bengali র and ব; folded only for matching, never in output.
_ASSAMESE_FOLD = str.maketrans({"ৰ": "র", "ৱ": "ব"})

BANGLA_DIGITS = "০১২৩৪৫৬৭৮৯"
_TO_BANGLA_DIGITS = str.maketrans("0123456789", BANGLA_DIGITS)
_TO_WESTERN_DIGITS = str.maketrans(BANGLA_DIGITS, "0123456789")

_RE_BENGALI_BLOCK = re.compile(r"[ঀ-৿]")


def canonical(text: str) -> str:
    """Internal canonical form: NFC + candrabindu order fix + modern khanda-ta.

    Idempotent. Joiners are kept because they carry rendering intent (র‍্য vs র্য).
    """
    s = unicodedata.normalize("NFC", text)
    s = _RE_LEGACY_KHANDA_TA.sub(KHANDA_TA, s)
    s = _RE_CANDRABINDU_BEFORE_VOWEL.sub(lambda m: m.group(1) + CANDRABINDU, s)
    return s


def match_key(text: str) -> str:
    """Form used for equality and lexicon lookup: canonical, joiners removed, Assamese folded."""
    s = canonical(text)
    s = _RE_JOINERS.sub("", s)
    return s.translate(_ASSAMESE_FOLD)


def to_output(text: str, *, precomposed_nukta: bool = True) -> str:
    """Convert internal form to what gets typed into the application."""
    s = canonical(text)
    if precomposed_nukta:
        s = _RE_NUKTA_DECOMPOSED.sub(lambda m: _NUKTA_RECOMPOSE[m.group(0)], s)
    return s


def to_bangla_digits(text: str) -> str:
    return text.translate(_TO_BANGLA_DIGITS)


def to_western_digits(text: str) -> str:
    return text.translate(_TO_WESTERN_DIGITS)


def has_bengali(text: str) -> bool:
    return _RE_BENGALI_BLOCK.search(text) is not None


_RE_ROMAN_KEEP = re.compile(r"[^a-z0-9']+")


def normalize_roman(text: str) -> str:
    """Loose-romanization key: case-folded, ASCII letters/digits/apostrophe only.

    Case is deliberately dropped: unlike Avro, Likhi never relies on capitalisation to disambiguate.
    """
    return _RE_ROMAN_KEEP.sub("", text.casefold())


def equivalent(a: str, b: str) -> bool:
    """True when two Bengali strings are the same word modulo normalization noise."""
    return match_key(a) == match_key(b)
