"""Phonetic keys that make loose romanizations and Bangla words comparable.

Idea: map both the typed roman string and a Bangla word into the same coarse "consonant skeleton"
alphabet, then look words up by key. `amr`, `amar`, `aamar` and আমার all become `amr`; `korchi`,
`korci` and করছি all become `krc` at the coarse level. The key is a recall device for candidate
generation; the ranker decides which candidate is right.

Two levels:
* fine   keeps aspiration and sibilant distinctions (kh vs k, sh vs s, ch vs c)
* coarse merges them and drops semivowels, for very sloppy input

Both levels drop every vowel except a word-initial one and collapse doubled consonants.
"""

from __future__ import annotations

import re
from functools import lru_cache

from likhi.engine.textnorm import match_key, normalize_roman

VIRAMA = "্"

# Bangla consonants -> fine class
_CONS = {
    "ক": "k",
    "খ": "kh",
    "গ": "g",
    "ঘ": "gh",
    "ঙ": "ng",
    "চ": "c",
    "ছ": "ch",
    "জ": "j",
    "ঝ": "jh",
    "ঞ": "n",
    "ট": "t",
    "ঠ": "th",
    "ড": "d",
    "ঢ": "dh",
    "ণ": "n",
    "ত": "t",
    "থ": "th",
    "দ": "d",
    "ধ": "dh",
    "ন": "n",
    "প": "p",
    "ফ": "f",
    "ব": "b",
    "ভ": "bh",
    "ম": "m",
    "য": "j",
    "র": "r",
    "ল": "l",
    "শ": "sh",
    "ষ": "sh",
    "স": "s",
    "হ": "h",
    "ড়": "r",
    "ঢ়": "rh",
    "য়": "y",
    "ৎ": "t",
    "ং": "ng",
    "ঃ": "h",
    "ৰ": "r",
    "ৱ": "b",
}
# Independent vowels and vowel signs -> vowel class (only a word-initial vowel survives in the key)
_VOWELS = {
    "অ": "o",
    "আ": "a",
    "ই": "i",
    "ঈ": "i",
    "উ": "u",
    "ঊ": "u",
    "ঋ": "ri",
    "এ": "e",
    "ঐ": "oi",
    "ও": "o",
    "ঔ": "ou",
    "া": "a",
    "ি": "i",
    "ী": "i",
    "ু": "u",
    "ূ": "u",
    "ৃ": "ri",
    "ে": "e",
    "ৈ": "oi",
    "ো": "o",
    "ৌ": "ou",
    "ৗ": "ou",
}
_NUKTA = "়"
_CANDRABINDU = "ঁ"

# Roman digraphs first, then singles. Values are fine classes; vowels map to "V"-prefixed markers.
_ROMAN_MULTI = {
    "kh": "kh",
    "gh": "gh",
    "ng": "ng",
    "ch": "ch",
    "chh": "ch",
    "jh": "jh",
    "th": "th",
    "dh": "dh",
    "ph": "f",
    "bh": "bh",
    "sh": "sh",
    "ck": "k",
    "ks": "k",
    "ksh": "kh",
    "kkh": "kh",
    "oi": "Voi",
    "ou": "Vou",
    "au": "Vou",
    "ai": "Voi",
    "ee": "Vi",
    "oo": "Vu",
    "aa": "Va",
}
_ROMAN_SINGLE = {
    "k": "k",
    "q": "k",
    "g": "g",
    "c": "c",
    "j": "j",
    "z": "j",
    "t": "t",
    "d": "d",
    "n": "n",
    "p": "p",
    "f": "f",
    "b": "b",
    "v": "bh",
    "m": "m",
    "y": "y",
    "r": "r",
    "l": "l",
    "s": "s",
    "h": "h",
    "x": "k",
    "w": "Vo",
    "a": "Va",
    "e": "Ve",
    "i": "Vi",
    "o": "Vo",
    "u": "Vu",
}

_COARSE = {
    "kh": "k",
    "gh": "g",
    "ch": "c",
    "jh": "j",
    "th": "t",
    "dh": "d",
    "bh": "b",
    "sh": "s",
    "ng": "n",
    "rh": "r",
    "y": "",
    "h": "h",
}
_COARSE_VOWEL = {
    "Va": "a",
    "Vo": "a",
    "Ve": "e",
    "Vi": "i",
    "Vu": "u",
    "Voi": "i",
    "Vou": "u",
    "Vri": "r",
}


def _finalize(units: list[str], level: str) -> str:
    """Drop non-initial vowels, apply coarse merges, collapse repeats."""
    out: list[str] = []
    for i, u in enumerate(units):
        if u.startswith("V"):
            if i == 0:
                out.append(_COARSE_VOWEL[u] if level == "coarse" else u[1:])
            continue
        if level == "coarse":
            u = _COARSE.get(u, u)
            if not u:
                continue
        if out and out[-1] == u:
            continue
        out.append(u)
    return "".join(out) if level == "fine" else "".join(out)


def _bangla_units(word: str) -> list[str]:
    s = match_key(word)
    units: list[str] = []
    i = 0
    n = len(s)
    while i < n:
        ch = s[i]
        nxt = s[i + 1] if i + 1 < n else ""
        if ch in _CONS:
            # nukta forms (NFC keeps ড় decomposed as ড + ়)
            if nxt == _NUKTA:
                base = {"ড": "r", "ঢ": "rh", "য": "y"}.get(ch, _CONS[ch])
                units.append(base)
                i += 2
                continue
            units.append(_CONS[ch])
            i += 1
            continue
        if ch == VIRAMA:
            # ya-phala / ba-phala after a consonant are usually not pronounced as j / b
            if nxt == "য":
                units.append("y")
                i += 2
                continue
            if nxt == "ব" and units:
                i += 2
                continue
            i += 1
            continue
        if ch in _VOWELS:
            units.append("V" + _VOWELS[ch])
            i += 1
            continue
        # candrabindu, digits, anything else: ignore
        i += 1
    return units


def _roman_units(roman: str) -> list[str]:
    s = normalize_roman(roman)
    units: list[str] = []
    i = 0
    n = len(s)
    while i < n:
        hit = None
        for L in (3, 2):
            seg = s[i : i + L]
            if len(seg) == L and seg in _ROMAN_MULTI:
                hit = _ROMAN_MULTI[seg]
                i += L
                break
        if hit is None:
            hit = _ROMAN_SINGLE.get(s[i])
            i += 1
            if hit is None:
                continue
        units.append(hit)
    # 'h' right after a consonant that has no aspirated class (e.g. "lh") is noise; keep simple.
    return units


@lru_cache(maxsize=65536)
def key_from_roman(roman: str, level: str = "coarse") -> str:
    return _finalize(_roman_units(roman), level)


@lru_cache(maxsize=65536)
def key_from_bangla(word: str, level: str = "coarse") -> str:
    return _finalize(_bangla_units(word), level)


_RE_LATIN = re.compile(r"[a-z]")


def is_romanish(text: str) -> bool:
    return bool(_RE_LATIN.search(text.casefold()))
