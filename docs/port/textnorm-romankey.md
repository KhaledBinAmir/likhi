# Porting specification — `textnorm.py` + `romankey.py`

Target of the port: `likhi.engine.textnorm` and `likhi.engine.romankey`, rewritten in Rust.

**Source of truth**

| File | Absolute path | SHA-256 |
|---|---|---|
| `textnorm.py` | `C:\Users\khaled\src\likhi\src\likhi\engine\textnorm.py` | `CBD1239DDD0D71951BEDD806CA69DD4E2E6C2FADE25BCD4D09D2B8C9956321D1` |
| `romankey.py` | `C:\Users\khaled\src\likhi\src\likhi\engine\romankey.py` | `69243239C1E297268727D9EE7FAC6DB1400EA6D35E035F6626BCD66E0DD8E3F8` |

The shipped copies under `C:\Users\khaled\src\likhi\dist\runtime\python\Lib\site-packages\likhi\engine\`
are byte-identical to the `src` copies (verified by `Get-FileHash`), so there is only one behaviour to port.

**Reference interpreter**: CPython 3.12.10, `unicodedata.unidata_version == "15.0.0"`.
Every NFC result quoted below was produced by that build.

**Why parity matters here**: `core.py` derives every candidate-generation key from these five
functions (`core.py:251`, `core.py:309`, `core.py:313`, `core.py:359`, `core.py:384`, `core.py:536`,
`core.py:560-561`, `core.py:592-593`). A one-character divergence in any of them changes which trie
buckets are probed, so it surfaces as "wrong suggestions" with no traceable cause.

**Parity fixtures already exist** — see [§10](#10-parity-fixtures).

---

## Table of contents

1. [Call graph and evaluation order](#1-call-graph-and-evaluation-order)
2. [`textnorm.py` — module constants](#2-textnormpy--module-constants)
3. [`textnorm.canonical`](#3-textnormcanonical)
4. [`textnorm.match_key` (needed by `key_from_bangla`)](#4-textnormmatch_key-needed-by-key_from_bangla)
5. [`textnorm.to_output`](#5-textnormto_output)
6. [`textnorm.normalize_roman`](#6-textnormnormalize_roman)
7. [`textnorm` — remaining helpers](#7-textnorm--remaining-helpers)
8. [`romankey.py` — mapping tables, verbatim](#8-romankeypy--mapping-tables-verbatim)
9. [`romankey` — the three algorithms](#9-romankey--the-three-algorithms)
10. [Parity fixtures](#10-parity-fixtures)
11. [Gotchas a naive port gets wrong](#11-gotchas-a-naive-port-gets-wrong)
12. [Dead code — present in Python, provably unreachable](#12-dead-code--present-in-python-provably-unreachable)
13. [Uncertain](#13-uncertain)

---

## 1. Call graph and evaluation order

```
normalize_roman(text)                       # textnorm.py:96   pure, no Unicode normalisation
canonical(text)                             # textnorm.py:55   NFC + 2 fixups
  └─ match_key(text)                        # textnorm.py:66   canonical -> strip joiners -> Assamese fold
  └─ to_output(text, precomposed_nukta)     # textnorm.py:73   canonical -> recompose nukta
  └─ equivalent(a, b)                       # textnorm.py:104  match_key(a) == match_key(b)

key_from_roman(roman, level)                # romankey.py:265
  └─ _roman_units(roman)                    # romankey.py:241
       └─ normalize_roman(roman)
  └─ _finalize(units, level)                # romankey.py:175

key_from_bangla(word, level)                # romankey.py:270
  └─ _bangla_units(word)                    # romankey.py:203
       └─ match_key(word)   ->  canonical  ->  NFC
  └─ _finalize(units, level)                # romankey.py:175
```

`level` is one of the two string literals `"fine"` and `"coarse"`. It is only ever *compared against*
`"coarse"` (`romankey.py:186`, `romankey.py:193`), so **any value other than the exact string
`"coarse"` selects fine behaviour** — including `"Coarse"`, `""` and `"x"` (verified).

---

## 2. `textnorm.py` — module constants

### 2.1 Single-character constants (`textnorm.py:22-27`)

```python
ZWNJ = "\u200c"
ZWJ = "\u200d"
VIRAMA = "্"
NUKTA = "়"
CANDRABINDU = "ঁ"
KHANDA_TA = "ৎ"
```

Verbatim codepoints (dumped from the file, not from the glyphs):

| Name | Codepoint | Unicode name |
|---|---|---|
| `ZWNJ` | U+200C | ZERO WIDTH NON-JOINER |
| `ZWJ` | U+200D | ZERO WIDTH JOINER |
| `VIRAMA` | U+09CD | BENGALI SIGN VIRAMA |
| `NUKTA` | U+09BC | BENGALI SIGN NUKTA |
| `CANDRABINDU` | U+0981 | BENGALI SIGN CANDRABINDU |
| `KHANDA_TA` | U+09CE | BENGALI LETTER KHANDA TA |

Note `ZWNJ`/`ZWJ` are written in the file as `\uXXXX` escapes; the other four are literal glyphs.

### 2.2 Nukta tables (`textnorm.py:30-36`)

```python
_NUKTA_PRECOMPOSED = {
    "ড়": "ড" + NUKTA,  # ড়
    "ঢ়": "ঢ" + NUKTA,  # ঢ়
    "য়": "য" + NUKTA,  # য়
}
_NUKTA_RECOMPOSE = {v: k for k, v in _NUKTA_PRECOMPOSED.items()}
_RE_NUKTA_DECOMPOSED = re.compile("|".join(map(re.escape, _NUKTA_RECOMPOSE)))
```

**The dict keys in the source file really are the single precomposed codepoints** (checked at the
byte level — `textnorm.py:31` is `"` U+09DC `"` `:` `"` U+09A1 `"` `+ NUKTA`). This matters because
the same three glyphs in `romankey.py:58-60` are stored *decomposed*; see [§12](#12-dead-code--present-in-python-provably-unreachable).

`_NUKTA_PRECOMPOSED`, exhaustive, in insertion order:

| Key (precomposed) | Value (decomposed) |
|---|---|
| U+09DC (BENGALI LETTER RRA) | U+09A1 U+09BC |
| U+09DD (BENGALI LETTER RHA) | U+09A2 U+09BC |
| U+09DF (BENGALI LETTER YYA) | U+09AF U+09BC |

`_NUKTA_RECOMPOSE` is the exact inverse (decomposed → precomposed), same three pairs, same order.

`_RE_NUKTA_DECOMPOSED` compiles to the literal pattern `ড়|ঢ়|য়`, i.e.

```
U+09A1 U+09BC | U+09A2 U+09BC | U+09AF U+09BC
```

`re.escape` leaves all six Bengali codepoints unchanged in Python 3.12 (only ASCII
non-alphanumerics get a backslash). The three alternatives start with three *distinct* base letters,
so alternation order is irrelevant — there is no longest-match hazard here.

### 2.3 Vowel-sign class and candrabindu regex (`textnorm.py:39-40`)

```python
_VOWEL_SIGNS = "া-ৄেৈোৌৗৢৣ"
_RE_CANDRABINDU_BEFORE_VOWEL = re.compile(f"{CANDRABINDU}([{_VOWEL_SIGNS}]+)")
```

The literal `_VOWEL_SIGNS` string is exactly:

```
U+09BE U+002D U+09C4 U+09C7 U+09C8 U+09CB U+09CC U+09D7 U+09E2 U+09E3
```

— i.e. one *range* `U+09BE-U+09C4` followed by seven singletons. The compiled pattern is

```
U+0981 U+0028 U+005B U+09BE U+002D U+09C4 U+09C7 U+09C8 U+09CB U+09CC U+09D7 U+09E2 U+09E3 U+005D U+002B U+0029
```

i.e. `ঁ([\u09BE-\u09C4\u09C7\u09C8\u09CB\u09CC\u09D7\u09E2\u09E3]+)`.

**The class expands to exactly these 14 codepoints** (a Rust port must hard-code this set, not
"Bengali vowel signs" as a Unicode property — U+09C5 and U+09C6 are unassigned and U+09D7 is
technically a length mark, not a vowel sign):

| # | Codepoint | Unicode name |
|---|---|---|
| 1 | U+09BE | BENGALI VOWEL SIGN AA |
| 2 | U+09BF | BENGALI VOWEL SIGN I |
| 3 | U+09C0 | BENGALI VOWEL SIGN II |
| 4 | U+09C1 | BENGALI VOWEL SIGN U |
| 5 | U+09C2 | BENGALI VOWEL SIGN UU |
| 6 | U+09C3 | BENGALI VOWEL SIGN VOCALIC R |
| 7 | U+09C4 | BENGALI VOWEL SIGN VOCALIC RR |
| 8 | U+09C7 | BENGALI VOWEL SIGN E |
| 9 | U+09C8 | BENGALI VOWEL SIGN AI |
| 10 | U+09CB | BENGALI VOWEL SIGN O |
| 11 | U+09CC | BENGALI VOWEL SIGN AU |
| 12 | U+09D7 | BENGALI AU LENGTH MARK |
| 13 | U+09E2 | BENGALI VOWEL SIGN VOCALIC L |
| 14 | U+09E3 | BENGALI VOWEL SIGN VOCALIC LL |

### 2.4 Remaining regexes and translation tables (`textnorm.py:42-52`, `93`)

```python
_RE_LEGACY_KHANDA_TA = re.compile("ত" + VIRAMA + ZWJ)
_RE_JOINERS = re.compile(f"[{ZWNJ}{ZWJ}]")
_ASSAMESE_FOLD = str.maketrans({"ৰ": "র", "ৱ": "ব"})
BANGLA_DIGITS = "০১২৩৪৫৬৭৮৯"
_TO_BANGLA_DIGITS = str.maketrans("0123456789", BANGLA_DIGITS)
_TO_WESTERN_DIGITS = str.maketrans(BANGLA_DIGITS, "0123456789")
_RE_BENGALI_BLOCK = re.compile(r"[ঀ-\u09ff]")
_RE_ROMAN_KEEP = re.compile(r"[^a-z0-9']+")
```

Verbatim compiled patterns / tables:

| Symbol | Exact content |
|---|---|
| `_RE_LEGACY_KHANDA_TA` | literal 3-char sequence `U+09A4 U+09CD U+200D` (no metacharacters) |
| `_RE_JOINERS` | `[U+200C U+200D]` — character class of exactly ZWNJ and ZWJ |
| `_ASSAMESE_FOLD` | `{U+09F0 → U+09B0, U+09F1 → U+09AC}` (ৰ→র, ৱ→ব). Exactly 2 entries. |
| `BANGLA_DIGITS` | `U+09E6 U+09E7 U+09E8 U+09E9 U+09EA U+09EB U+09EC U+09ED U+09EE U+09EF` |
| `_TO_BANGLA_DIGITS` | `'0'..'9'` (U+0030..U+0039) → U+09E6..U+09EF, positionally |
| `_TO_WESTERN_DIGITS` | U+09E6..U+09EF → `'0'..'9'`, positionally |
| `_RE_BENGALI_BLOCK` | `[U+0980-U+09FF]` — the whole Bengali block, inclusive |
| `_RE_ROMAN_KEEP` | `[^a-z0-9']+` — negated class: ASCII lowercase `a-z`, ASCII digits `0-9`, ASCII apostrophe U+0027. One-or-more. |

Note `_RE_BENGALI_BLOCK` starts at U+0980 (BENGALI ANJI), not U+0985, and *includes the Bengali
digits* U+09E6..U+09EF — `has_bengali("০")` is `True` (verified).

---

## 3. `textnorm.canonical`

```python
def canonical(text: str) -> str:
    """Internal canonical form: NFC + candrabindu order fix + modern khanda-ta.

    Idempotent. Joiners are kept because they carry rendering intent (র‍্য vs র্য).
    """
    s = unicodedata.normalize("NFC", text)
    s = _RE_LEGACY_KHANDA_TA.sub(KHANDA_TA, s)
    s = _RE_CANDRABINDU_BEFORE_VOWEL.sub(lambda m: m.group(1) + CANDRABINDU, s)
    return s
```
— `textnorm.py:55-63`

### 3.1 Exact steps, in order

1. **`unicodedata.normalize("NFC", text)`** — standard Unicode NFC (NFD then canonical composition),
   Unicode 15.0.0 tables. **Not** NFKC. There is no custom canonical map at this step.
2. **Legacy khanda-ta**: replace *every* non-overlapping occurrence of `U+09A4 U+09CD U+200D`
   (ত + virama + ZWJ) with `U+09CE`. Left to right, non-overlapping, unconditional — there is no
   check that the sequence is word-final.
3. **Candrabindu reorder**: for every non-overlapping match of
   `U+0981 ([vowel-sign]+)`, replace with `group(1) + U+0981` — i.e. move the candrabindu to *after
   the whole greedy run* of vowel signs. `+` is greedy; `re.sub` scans left to right and **does not
   re-scan the replacement text**.

Steps 2 and 3 commute on every input I probed (4 adversarial khanda-ta/candrabindu interleavings,
all identical either way), but there is no proof, so **keep the Python order**.

### 3.2 What NFC does to Bengali here (verified, Unicode 15.0.0)

| Input | NFC output |
|---|---|
| U+09DC | **U+09A1 U+09BC** (decomposes!) |
| U+09DD | **U+09A2 U+09BC** (decomposes!) |
| U+09DF | **U+09AF U+09BC** (decomposes!) |
| U+09A1 U+09BC | U+09A1 U+09BC (unchanged) |
| U+09C7 U+09BE | **U+09CB** (composes: e-kar + aa-kar → o-kar) |
| U+09C7 U+09D7 | **U+09CC** (composes: e-kar + au length mark → au-kar) |
| U+09CB | U+09CB |
| U+09CC | U+09CC |
| U+09A4 U+09CD U+200D | unchanged (ZWJ survives NFC) |
| U+0995 U+09BC | unchanged (kA + nukta has no precomposed form) |

U+09DC/09DD/09DF are **Unicode composition exclusions**: their canonical decomposition exists, but
NFC will not recompose it. Verified: `NFC(NFD(U+09DC)) != U+09DC`. The nukta's canonical combining
class is **7**.

So *after* `canonical`, the internal form always has nukta letters **decomposed** (base + U+09BC).

### 3.3 Worked traces

| Input | After NFC | After khanda-ta | After candrabindu = `canonical` |
|---|---|---|---|
| U+0995 U+0981 U+09BE | same | same | **U+0995 U+09BE U+0981** |
| U+0995 U+0981 U+09C7 U+09BE | U+0995 U+0981 U+09CB | same | **U+0995 U+09CB U+0981** |
| U+0995 U+0981 U+09BE U+09BF | same | same | **U+0995 U+09BE U+09BF U+0981** (moved past *both*) |
| U+09B9 U+09A0 U+09BE U+09A4 U+09CD U+200D | same | U+09B9 U+09A0 U+09BE U+09CE | **U+09B9 U+09A0 U+09BE U+09CE** |
| U+09A4 U+09CD U+200D U+09AF | same | U+09CE U+09AF | **U+09CE U+09AF** |
| U+09AC U+09DC | U+09AC U+09A1 U+09BC | same | **U+09AC U+09A1 U+09BC** |
| U+09B0 U+200D U+09CD U+09AF | same | same | **U+09B0 U+200D U+09CD U+09AF** (joiner kept) |

### 3.4 The docstring's "Idempotent" claim is **false**

Counter-example, verified:

```
input     U+0995 U+0981 U+0981 U+09BE      (ka + candrabindu + candrabindu + aa-kar)
pass 1    U+0995 U+0981 U+09BE U+0981
pass 2    U+0995 U+09BE U+0981 U+0981      <- fixpoint
```

Because `re.sub` does not re-scan its replacement, the *first* candrabindu is not adjacent to a vowel
sign during pass 1 (its right neighbour is the second candrabindu), so it only moves on pass 2.
**A Rust port must apply the substitution exactly once, not to a fixpoint**, or it will disagree with
Python on inputs with two candrabindus. (Second verified case: `U+0995 U+09BE U+0981 U+09BF` reaches
its fixpoint after one pass — the non-idempotence needs two adjacent candrabindus.)

---

## 4. `textnorm.match_key` (needed by `key_from_bangla`)

```python
def match_key(text: str) -> str:
    """Form used for equality and lexicon lookup: canonical, joiners removed, Assamese folded."""
    s = canonical(text)
    s = _RE_JOINERS.sub("", s)
    return s.translate(_ASSAMESE_FOLD)
```
— `textnorm.py:66-70`

Order is load-bearing:

* `canonical` runs **before** joiner removal, so `ত ্ ZWJ` becomes U+09CE — *not* `ত ্`.
  Verified: `match_key(U+09A4 U+09CD U+200D) == U+09CE`.
* The Assamese fold is a plain per-character `str.translate` over exactly two mappings
  (U+09F0→U+09B0, U+09F1→U+09AC), applied **last**.

Verified examples:

| Input | `match_key` |
|---|---|
| U+09B0 U+200D U+09CD U+09AF | U+09B0 U+09CD U+09AF |
| U+0995 U+200C U+09CD U+09B7 | U+0995 U+09CD U+09B7 |
| U+09A1 U+200D U+09BC | U+09A1 U+09BC (joiner removal *creates* an adjacent nukta pair) |
| U+09F0 U+09BE U+09AE | U+09B0 U+09BE U+09AE |
| U+09AC U+09DC | U+09AC U+09A1 U+09BC |

`equivalent(a, b)` (`textnorm.py:104-106`) is `match_key(a) == match_key(b)`.

---

## 5. `textnorm.to_output`

```python
def to_output(text: str, *, precomposed_nukta: bool = True) -> str:
    """Convert internal form to what gets typed into the application."""
    s = canonical(text)
    if precomposed_nukta:
        s = _RE_NUKTA_DECOMPOSED.sub(lambda m: _NUKTA_RECOMPOSE[m.group(0)], s)
    return s
```
— `textnorm.py:73-78`

* `precomposed_nukta` is **keyword-only** and defaults to `True`. Every call site in the engine uses
  the default (`core.py:536`, `core.py:579`).
* The recomposition is **only** those three pairs — it is *not* a general NFC-with-exclusions-undone.
  Verified: `U+0995 U+09BC` and `U+09A8 U+09BC` and `U+09AC U+09BC` all pass through unchanged.
* `U+09A1 U+09BC U+09BC` → `U+09DC U+09BC` (the regex consumes the first nukta only).
* **The result is deliberately not NFC.** `to_output(U+09AC U+09A1 U+09BC)` = `U+09AC U+09DC`, and
  `NFC` of that is `U+09AC U+09A1 U+09BC` again. Never re-normalise the output.
* Joiners (ZWJ/ZWNJ) survive into the output: `to_output(U+09B0 U+200D U+09CD U+09AF)` is unchanged.

---

## 6. `textnorm.normalize_roman`

```python
_RE_ROMAN_KEEP = re.compile(r"[^a-z0-9']+")


def normalize_roman(text: str) -> str:
    """Loose-romanization key: case-folded, ASCII letters/digits/apostrophe only.

    Case is deliberately dropped: unlike Avro, Likhi never relies on capitalisation to disambiguate.
    """
    return _RE_ROMAN_KEEP.sub("", text.casefold())
```
— `textnorm.py:93-101`

Exactly two steps:

1. **`str.casefold()`** — Python's *full Unicode case folding* (CaseFolding.txt `C` + `F` status,
   non-Turkic). **This is not `to_lowercase()`.**
2. Delete every run of characters outside `[a-z0-9']`. The surviving alphabet is exactly
   **U+0061..U+007A, U+0030..U+0039, U+0027** — 37 characters. No Unicode normalisation is applied.

### 6.1 `casefold` vs `to_lowercase` — verified divergences

This is the single most likely silent bug in the port. Rust's `str::to_lowercase()` is **not**
case folding.

| Input | `casefold()` | `lower()` | `normalize_roman` result | `casefold == lower`? |
|---|---|---|---|---|
| U+00DF `ß` | `"ss"` | `"ß"` | `"ss"` | **no** |
| U+1E9E `ẞ` | `"ss"` | `"ß"` | `"ss"` | **no** |
| U+FB01 `ﬁ` | `"fi"` | `"ﬁ"` | `"fi"` | **no** |
| U+FB03 `ﬃ` | `"ffi"` | `"ﬃ"` | `"ffi"` | **no** |
| U+03C2 `ς` | `"σ"` | `"ς"` | `""` | **no** |
| U+0130 `İ` | `"i"+U+0307` | `"i"+U+0307` | `"i"` | yes |
| U+212A KELVIN | `"k"` | `"k"` | `"k"` | yes |
| U+212B ANGSTROM | `"å"` | `"å"` | `""` | yes |
| U+FF21 fullwidth A | `"ａ"` | `"ａ"` | `""` | yes |

A `to_lowercase`-based port turns `"Straße"` into `"strae"`; Python produces `"strasse"`.
Use a real case-folding implementation (e.g. the `caseless` crate's `default_case_fold_str`, or
`unicode-case-mapping`'s full case folding). ASCII-only fast path is fine *provided* you fall back to
full folding whenever any byte is ≥ 0x80.

### 6.2 Verified examples

| Input | Output |
|---|---|
| `""` | `""` |
| `" "` | `""` |
| `"AMar"` | `"amar"` |
| `"Korchi!"` | `"korchi"` |
| `"don't"` (U+0027) | `"don't"` — apostrophe **kept** |
| `"don’t"` (U+2019) | `"dont"` — curly quote **dropped** |
| `"a-b_c"` | `"abc"` |
| `"2026"` | `"2026"` — digits kept |
| `"২০২৬"` (U+09E8 …) | `""` — Bengali digits dropped |
| `"Ångström"` | `"ngstrm"` |
| `"İstanbul"` | `"istanbul"` |

---

## 7. `textnorm` — remaining helpers

```python
def to_bangla_digits(text: str) -> str:          # textnorm.py:81-82
    return text.translate(_TO_BANGLA_DIGITS)

def to_western_digits(text: str) -> str:         # textnorm.py:85-86
    return text.translate(_TO_WESTERN_DIGITS)

def has_bengali(text: str) -> bool:              # textnorm.py:89-90
    return _RE_BENGALI_BLOCK.search(text) is not None

def equivalent(a: str, b: str) -> bool:          # textnorm.py:104-106
    return match_key(a) == match_key(b)
```

Pure per-codepoint substitution / search. `to_bangla_digits("2026-07") == "২০২৬-০৭"` (non-digits pass
through). `has_bengali` is `true` for **any** codepoint in U+0980..U+09FF, digits included.

`romankey.is_romanish` (`romankey.py:274-278`):

```python
_RE_LATIN = re.compile(r"[a-z]")

def is_romanish(text: str) -> bool:
    return bool(_RE_LATIN.search(text.casefold()))
```

Also `casefold`, also ASCII-only class. `is_romanish("ẞ")` is therefore `True` (folds to `"ss"`).

---

## 8. `romankey.py` — mapping tables, verbatim

`VIRAMA = "্"` (U+09CD, `romankey.py:22`), `_NUKTA = "়"` (U+09BC, `romankey.py:92`),
`_CANDRABINDU = "ঁ"` (U+0981, `romankey.py:93`).

`_CANDRABINDU` is **defined but never used** in `romankey.py`.

### 8.1 `_CONS` — Bangla consonant → fine class (`romankey.py:25-66`), 40 entries, insertion order

| # | Key | Codepoints | Value |
|---|---|---|---|
| 1 | ক | U+0995 | `k` |
| 2 | খ | U+0996 | `kh` |
| 3 | গ | U+0997 | `g` |
| 4 | ঘ | U+0998 | `gh` |
| 5 | ঙ | U+0999 | `ng` |
| 6 | চ | U+099A | `c` |
| 7 | ছ | U+099B | `ch` |
| 8 | জ | U+099C | `j` |
| 9 | ঝ | U+099D | `jh` |
| 10 | ঞ | U+099E | `n` |
| 11 | ট | U+099F | `t` |
| 12 | ঠ | U+09A0 | `th` |
| 13 | ড | U+09A1 | `d` |
| 14 | ঢ | U+09A2 | `dh` |
| 15 | ণ | U+09A3 | `n` |
| 16 | ত | U+09A4 | `t` |
| 17 | থ | U+09A5 | `th` |
| 18 | দ | U+09A6 | `d` |
| 19 | ধ | U+09A7 | `dh` |
| 20 | ন | U+09A8 | `n` |
| 21 | প | U+09AA | `p` |
| 22 | ফ | U+09AB | `f` |
| 23 | ব | U+09AC | `b` |
| 24 | ভ | U+09AD | `bh` |
| 25 | ম | U+09AE | `m` |
| 26 | য | U+09AF | `j` |
| 27 | র | U+09B0 | `r` |
| 28 | ল | U+09B2 | `l` |
| 29 | শ | U+09B6 | `sh` |
| 30 | ষ | U+09B7 | `sh` |
| 31 | স | U+09B8 | `s` |
| 32 | হ | U+09B9 | `h` |
| 33 | ড় | **U+09A1 U+09BC** (2 chars) | `r` |
| 34 | ঢ় | **U+09A2 U+09BC** (2 chars) | `rh` |
| 35 | য় | **U+09AF U+09BC** (2 chars) | `y` |
| 36 | ৎ | U+09CE | `t` |
| 37 | ং | U+0982 | `ng` |
| 38 | ঃ | U+0983 | `h` |
| 39 | ৰ | U+09F0 | `r` |
| 40 | ৱ | U+09F1 | `b` |

Rows **33–35 are two-codepoint keys** and are never reachable (see [§12](#12-dead-code--present-in-python-provably-unreachable)).
Rows 39–40 are also unreachable in practice because `match_key` already folds U+09F0/U+09F1 away —
but they are reachable if `_bangla_units` is ever called on a non-`match_key`ed string, which it is
not. **Not** in `_CONS`: ঌ U+098C, ঽ U+09BD, ঢ়-as-U+09DD, ৡ U+09E1, য়-as-U+09DF, ড়-as-U+09DC.
Note U+09A9, U+09B1, U+09B3–U+09B5 are unassigned/absent by design.

### 8.2 `_VOWELS` — independent vowels + vowel signs → vowel class (`romankey.py:68-91`), 22 entries

| # | Key | Codepoint | Value |
|---|---|---|---|
| 1 | অ | U+0985 | `o` |
| 2 | আ | U+0986 | `a` |
| 3 | ই | U+0987 | `i` |
| 4 | ঈ | U+0988 | `i` |
| 5 | উ | U+0989 | `u` |
| 6 | ঊ | U+098A | `u` |
| 7 | ঋ | U+098B | `ri` |
| 8 | এ | U+098F | `e` |
| 9 | ঐ | U+0990 | `oi` |
| 10 | ও | U+0993 | `o` |
| 11 | ঔ | U+0994 | `ou` |
| 12 | া | U+09BE | `a` |
| 13 | ি | U+09BF | `i` |
| 14 | ী | U+09C0 | `i` |
| 15 | ু | U+09C1 | `u` |
| 16 | ূ | U+09C2 | `u` |
| 17 | ৃ | U+09C3 | `ri` |
| 18 | ে | U+09C7 | `e` |
| 19 | ৈ | U+09C8 | `oi` |
| 20 | ো | U+09CB | `o` |
| 21 | ৌ | U+09CC | `ou` |
| 22 | ৗ | U+09D7 | `ou` |

**Deliberate asymmetry with `_VOWEL_SIGNS` in `textnorm.py`**: U+09C4 (VOCALIC RR), U+09E2 and
U+09E3 are in the candrabindu character class but **not** in `_VOWELS`, so they are ignored by
`_bangla_units`. Note `অ` (U+0985) maps to `o`, not `a`.

### 8.3 `_ROMAN_MULTI` — roman digraphs/trigraphs (`romankey.py:96-119`), 22 entries

19 keys of length 2, 3 keys of length 3 (`chh`, `ksh`, `kkh`). Insertion order below; **insertion
order is irrelevant to behaviour** because lookup is by exact fixed-width slice, never by trying
entries in order.

| # | Key | Value | Len |
|---|---|---|---|
| 1 | `kh` | `kh` | 2 |
| 2 | `gh` | `gh` | 2 |
| 3 | `ng` | `ng` | 2 |
| 4 | `ch` | `ch` | 2 |
| 5 | `chh` | `ch` | **3** |
| 6 | `jh` | `jh` | 2 |
| 7 | `th` | `th` | 2 |
| 8 | `dh` | `dh` | 2 |
| 9 | `ph` | `f` | 2 |
| 10 | `bh` | `bh` | 2 |
| 11 | `sh` | `sh` | 2 |
| 12 | `ck` | `k` | 2 |
| 13 | `ks` | `k` | 2 |
| 14 | `ksh` | `kh` | **3** |
| 15 | `kkh` | `kh` | **3** |
| 16 | `oi` | `Voi` | 2 |
| 17 | `ou` | `Vou` | 2 |
| 18 | `au` | `Vou` | 2 |
| 19 | `ai` | `Voi` | 2 |
| 20 | `ee` | `Vi` | 2 |
| 21 | `oo` | `Vu` | 2 |
| 22 | `aa` | `Va` | 2 |

### 8.4 `_ROMAN_SINGLE` — roman single letters (`romankey.py:120-147`), 26 entries

Covers all 26 ASCII letters (verified: `set("a".."z") - set(_ROMAN_SINGLE) == {}`). Digits `0-9` and
apostrophe `'` are **absent**, hence skipped.

| # | Key | Value |
|---|---|---|
| 1 | `k` | `k` |
| 2 | `q` | `k` |
| 3 | `g` | `g` |
| 4 | `c` | `c` |
| 5 | `j` | `j` |
| 6 | `z` | `j` |
| 7 | `t` | `t` |
| 8 | `d` | `d` |
| 9 | `n` | `n` |
| 10 | `p` | `p` |
| 11 | `f` | `f` |
| 12 | `b` | `b` |
| 13 | `v` | `bh` |
| 14 | `m` | `m` |
| 15 | `y` | `y` |
| 16 | `r` | `r` |
| 17 | `l` | `l` |
| 18 | `s` | `s` |
| 19 | `h` | `h` |
| 20 | `x` | `k` |
| 21 | `w` | `Vo` |
| 22 | `a` | `Va` |
| 23 | `e` | `Ve` |
| 24 | `i` | `Vi` |
| 25 | `o` | `Vo` |
| 26 | `u` | `Vu` |

Note `w` → `Vo` (a **vowel** unit, not a consonant).

### 8.5 `_COARSE` — fine consonant class → coarse (`romankey.py:149-162`), 12 entries

| # | Key | Value |
|---|---|---|
| 1 | `kh` | `k` |
| 2 | `gh` | `g` |
| 3 | `ch` | `c` |
| 4 | `jh` | `j` |
| 5 | `th` | `t` |
| 6 | `dh` | `d` |
| 7 | `bh` | `b` |
| 8 | `sh` | `s` |
| 9 | `ng` | `n` |
| 10 | `rh` | `r` |
| 11 | `y` | `""` (empty string) |
| 12 | `h` | `h` |

Entry 11 is unreachable (see [§12](#12-dead-code--present-in-python-provably-unreachable)).
Entry 12 is an identity mapping.
Classes with **no** entry pass through unchanged: `k g c j t d p f b m n r l s h`.

### 8.6 `_COARSE_VOWEL` — V-marker → coarse initial vowel (`romankey.py:163-172`), 8 entries

| # | Key | Value |
|---|---|---|
| 1 | `Va` | `a` |
| 2 | `Vo` | **`a`** |
| 3 | `Ve` | `e` |
| 4 | `Vi` | `i` |
| 5 | `Vu` | `u` |
| 6 | `Voi` | `i` |
| 7 | `Vou` | `u` |
| 8 | `Vri` | `r` |

`Vo` → `a` (not `o`) is deliberate — `o` and `a` are merged at the coarse level.
I enumerated every V-marker that either table can produce: `{Va, Ve, Vi, Vo, Voi, Vou, Vri, Vu}` —
**exactly the 8 keys above, so the `_COARSE_VOWEL[u]` lookup at `romankey.py:186` can never
`KeyError`.** A Rust port may use a total function here.

---

## 9. `romankey` — the three algorithms

### 9.1 `_bangla_units(word) -> list[str]` (`romankey.py:203-238`)

```python
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
```

**Iteration**: a single left-to-right pass over the **Unicode scalar values** of `match_key(word)`.
`len(s)` and `s[i]` are *character* (code point) indices, not bytes — in Rust iterate over
`chars().collect::<Vec<char>>()` or an equivalent, never over `&[u8]`.

**Branch order is exactly**: consonant (with nukta look-ahead) → virama (with ya/ba look-ahead) →
vowel → skip. There is no longest-match table lookup; the only multi-character handling is the two
explicit 1-character look-aheads. `nxt` is `""` at the end of the string, which matches nothing.

Per-branch detail:

1. **`ch in _CONS`** (single char — rows 33–35 of `_CONS` can never match here).
   * If `nxt == U+09BC`: emit `{U+09A1: "r", U+09A2: "rh", U+09AF: "y"}.get(ch, _CONS[ch])` and
     advance **2**. So the nukta is consumed after *any* consonant: `U+0995 U+09BC` → `["k"]`
     (verified), `U+09A8 U+09BC U+09BE` → `["n", "Va"]` (verified).
   * Else emit `_CONS[ch]`, advance 1.
2. **`ch == U+09CD` (virama)**
   * `nxt == U+09AF` (য): emit `"y"`, advance **2**. This applies **at any position, including
     word-initial** — `U+09CD U+09AF U+09BE` → `["y", "Va"]` (verified), which `_finalize` then turns
     into `j`.
   * `nxt == U+09AC` (ব) **and `units` is non-empty**: emit nothing, advance **2** (ba-phala is
     dropped). The `and units` guard means a **word-initial** `্ব` is *not* dropped:
     `U+09CD U+09AC U+09BE` → `["b", "Va"]` (verified). Asymmetric with ya-phala on purpose.
   * Otherwise: emit nothing, advance **1** (the virama is a conjunct marker and is just dropped;
     the following consonant is processed on the next iteration). `U+0995 U+09CD U+09B7` →
     `["k", "sh"]` (verified).
3. **`ch in _VOWELS`**: emit `"V" + _VOWELS[ch]`, advance 1. Independent vowels and vowel signs are
   treated identically — position in the *units list* is what matters later, not whether the vowel
   was independent.
4. **Anything else**: advance 1, emit nothing. This silently eats candrabindu U+0981, a stray nukta
   with no consonant before it (`U+09BC` alone → `[]`), Bengali digits, ASCII letters, punctuation,
   U+09C4, U+09E2, U+09E3, U+09BD, ZWJ/ZWNJ (already gone via `match_key`), everything.
   `_bangla_units("amar") == []`, so `key_from_bangla("amar", …) == ""` (verified).

Verified traces:

| Word | `match_key` codepoints | units | fine | coarse |
|---|---|---|---|---|
| আমার | U+0986 U+09AE U+09BE U+09B0 | `['Va','m','Va','r']` | `amr` | `amr` |
| করছি | U+0995 U+09B0 U+099B U+09BF | `['k','r','ch','Vi']` | `krch` | `krc` |
| জন্য | U+099C U+09A8 U+09CD U+09AF | `['j','n','y']` | `jn` | `jn` |
| কোরিয়া | U+0995 U+09CB U+09B0 U+09BF U+09AF U+09BC U+09BE | `['k','Vo','r','Vi','y','Va']` | `kr` | `kr` |
| বড় (either encoding) | U+09AC U+09A1 U+09BC | `['b','r']` | `br` | `br` |
| স্বপ্ন | U+09B8 U+09CD U+09AC U+09AA U+09CD U+09A8 | `['s','p','n']` | `spn` | `spn` |
| বাংলাদেশ | U+09AC U+09BE U+0982 U+09B2 U+09BE U+09A6 U+09C7 U+09B6 | `['b','Va','ng','l','Va','d','Ve','sh']` | `bngldsh` | `bnlds` |
| যেন | U+09AF U+09C7 U+09A8 | `['j','Ve','n']` | `jn` | `jn` |
| আম্মা | U+0986 U+09AE U+09CD U+09AE U+09BE | `['Va','m','m','Va']` | `am` | `am` |
| খালি | U+0996 U+09BE U+09B2 U+09BF | `['kh','Va','l','Vi']` | `khl` | `kl` |
| কালি | U+0995 U+09BE U+09B2 U+09BF | `['k','Va','l','Vi']` | `kl` | `kl` |
| র‍্য / র্য | U+09B0 U+09CD U+09AF | `['r','y']` | `r` | `r` |
| ঋষি | U+098B U+09B7 U+09BF | `['Vri','sh','Vi']` | `rish` | `rs` |
| হঠাৎ | U+09B9 U+09A0 U+09BE U+09CE | `['h','th','Va','t']` | `htht` | `ht` |
| মানুষঃ | U+09AE U+09BE U+09A8 U+09C1 U+09B7 U+0983 | `['m','Va','n','Vu','sh','h']` | `mnshh` | `mnsh` |
| রাজাঁ | U+09B0 U+09BE U+099C U+09BE U+0981 | `['r','Va','j','Va']` | `rj` | `rj` |
| ৰাম | U+09B0 U+09BE U+09AE (folded) | `['r','Va','m']` | `rm` | `rm` |

### 9.2 `_roman_units(roman) -> list[str]` (`romankey.py:241-261`)

```python
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
```

**Matching discipline: strict longest-first over a fixed window of 3 then 2, then single.** It is
*not* a trie walk and *not* regex alternation.

Exact loop:

1. Try `s[i:i+3]`; the `len(seg) == L` guard means a 3-window that runs off the end is rejected
   (Python slicing truncates silently — a Rust port slicing `&s[i..min(i+3, n)]` must apply the same
   length guard).
2. Else try `s[i:i+2]` with the same guard.
3. Else look up the single char `s[i]` in `_ROMAN_SINGLE`; **advance `i` by 1 first**, then if the
   lookup failed `continue` (emit nothing). Unknown single characters — digits `0-9` and `'` are the
   only ones that can reach here, since `normalize_roman` already restricted the alphabet — are
   silently skipped without breaking the window alignment.
4. Push `hit`.

There is **no backtracking**: once a 3- or 2-window matches, `i` jumps past it unconditionally.

Verified window traces (these pin down the ordering rule):

| Input | units | Why |
|---|---|---|
| `kks` | `['k','k']` | `kks`✗, `kk`✗ → `k`; then `ks`✓→`k` |
| `kkh` | `['kh']` | 3-window `kkh`✓ |
| `kkho` | `['kh','Vo']` | |
| `ksh` | `['kh']` | 3-window `ksh`✓ beats 2-window `ks` |
| `ks` | `['k']` | 3-window rejected by length guard; `ks`✓ |
| `ksha` | `['kh','Va']` | |
| `cks` | `['k','s']` | `cks`✗, `ck`✓→`k`, then `s` |
| `chh` | `['ch']` | 3-window `chh`✓ |
| `chho` | `['ch','Vo']` | |
| `ch` | `['ch']` | |
| `sch` | `['s','ch']` | `sch`✗, `sc`✗ → `s`; then `ch`✓ |
| `tth` | `['t','th']` | |
| `ddh` | `['d','dh']` | |
| `nng` | `['n','ng']` | |
| `ngh` | `['ng','h']` | `ngh`✗, `ng`✓; then `h` |
| `phh` | `['f','h']` | |
| `shh` | `['sh','h']` | |
| `hh` | `['h','h']` | |
| `aaa` | `['Va','Va']` | `aaa`✗, `aa`✓; then `a` |
| `ooo` | `['Vu','Vo']` | `oo`✓→`Vu`; then `o`→`Vo` |
| `oii` | `['Voi','Vi']` | |
| `a1b'c` | `['Va','b','c']` | `1` and `'` skipped |

Full-word traces:

| Roman | units | fine | coarse |
|---|---|---|---|
| `amar` | `['Va','m','Va','r']` | `amr` | `amr` |
| `aamar` | `['Va','m','Va','r']` | `amr` | `amr` |
| `amr` | `['Va','m','r']` | `amr` | `amr` |
| `korchi` | `['k','Vo','r','ch','Vi']` | `krch` | `krc` |
| `korci` | — | `krc` | `krc` |
| `chhobi` | `['ch','Vo','b','Vi']` | `chb` | `cb` |
| `kkhoma` | `['kh','Vo','m','Va']` | `khm` | `km` |
| `ksham` | `['kh','Va','m']` | `khm` | `km` |
| `boi` | `['b','Voi']` | `b` | `b` |
| `yeno` | `['y','Ve','n','Vo']` | `jn` | `jn` |
| `koria` | `['k','Vo','r','Vi','Va']` | `kr` | `kr` |
| `koriya` | `['k','Vo','r','Vi','y','Va']` | `kr` | `kr` |
| `korea` | `['k','Vo','r','Ve','Va']` | `kr` | `kr` |
| `amma` | `['Va','m','m','Va']` | `am` | `am` |
| `khali` | `['kh','Va','l','Vi']` | `khl` | `kl` |
| `kali` | `['k','Va','l','Vi']` | `kl` | `kl` |
| `bangladesh` | `['b','Va','ng','l','Va','d','Ve','sh']` | `bngldsh` | `bnlds` |
| `shopno` | `['sh','Vo','p','n','Vo']` | `shpn` | `spn` |
| `sopno` | `['s','Vo','p','n','Vo']` | `spn` | `spn` |
| `jonno` | `['j','Vo','n','n','Vo']` | `jn` | `jn` |
| `jonyo` | `['j','Vo','n','y','Vo']` | `jn` | `jn` |
| `w` | `['Vo']` | `o` | `a` |
| `au` | `['Vou']` | `ou` | `u` |
| `ai` | `['Voi']` | `oi` | `i` |
| `ee` | `['Vi']` | `i` | `i` |
| `oo` | `['Vu']` | `u` | `u` |
| `v` | `['bh']` | `bh` | `b` |
| `x` / `q` | `['k']` | `k` | `k` |

### 9.3 `_finalize(units, level) -> str` (`romankey.py:175-200`)

```python
def _finalize(units: list[str], level: str) -> str:
    out: list[str] = []
    for i, u in enumerate(units):
        if u.startswith("V"):
            if i == 0:
                out.append(_COARSE_VOWEL[u] if level == "coarse" else u[1:])
            continue
        if u == "y":
            if i == 0:
                u = "j"
            else:
                continue
        if level == "coarse":
            u = _COARSE.get(u, u)
            if not u:
                continue
        if out and out[-1] == u:
            continue
        out.append(u)
    return "".join(out)
```

Steps, **in this exact order**, per unit:

1. **Vowel markers** (`u` starts with the ASCII letter `V` — note every consonant class in both
   tables is lowercase, so `startswith("V")` is an unambiguous tag):
   * `i == 0` → append `_COARSE_VOWEL[u]` when `level == "coarse"`, otherwise `u[1:]` (drop the `V`).
   * Always `continue` afterwards. **Non-initial vowels are dropped entirely.**
   * This append **bypasses the dedup check** — harmless, because `out` is necessarily empty at
     `i == 0`.
2. **The semivowel `y`**:
   * `i == 0` → rewrite `u = "j"` and fall through to steps 3–4.
   * otherwise `continue` (dropped) — at **both** levels, before any coarse mapping.
3. **Coarse merge** (`level == "coarse"` only): `u = _COARSE.get(u, u)`, then `if not u: continue`.
   The empty-string check is unreachable (see §12).
4. **Adjacent-duplicate collapse**: `if out and out[-1] == u: continue`.
   **This runs *after* the coarse merge**, so `["kh","k"]` collapses to `"k"` at coarse level but
   stays `"khk"` at fine level. Only *adjacent* duplicates collapse.
5. Append, then `"".join(out)`.

**`i` is the index into `units`, not into `out`.** Since `_bangla_units`/`_roman_units` skip unknown
characters without emitting, `i == 0` means "first recognised unit", not "first character". Verified:
`_roman_units("a1b'c")[0] == "Va"`, and `_bangla_units("aআমb")[0] == "Va"`.

Verified `_finalize` micro-cases:

| units | level | result |
|---|---|---|
| `['y']` | coarse | `j` |
| `['k','y']` | coarse | `k` |
| `['Vri','r']` | fine | `rir` |
| `['Vri','r']` | **coarse** | **`r`** |

That last row is a genuine collision worth noting: at coarse level an initial `ঋ` becomes `r`, the
following `র` also becomes `r`, and step 4 collapses them. `key_from_bangla("ঋর", "coarse") == "r"`
(verified).

### 9.4 Public entry points (`romankey.py:264-271`)

```python
@lru_cache(maxsize=65536)
def key_from_roman(roman: str, level: str = "coarse") -> str:
    return _finalize(_roman_units(roman), level)


@lru_cache(maxsize=65536)
def key_from_bangla(word: str, level: str = "coarse") -> str:
    return _finalize(_bangla_units(word), level)
```

* Default `level` is `"coarse"` for both. `core.py:359` and `core.py:384` always pass it explicitly.
* `lru_cache(maxsize=65536)` — pure memoisation, no observable effect on results. A Rust port may use
  any cache or none. (Python's `lru_cache` keys on the *call shape*, so `f("x")` and `f("x","coarse")`
  occupy two cache slots for the same answer — a memory detail only.)
* Both functions are **total**: no input can raise.

---

## 10. Parity fixtures

`C:\Users\khaled\src\likhi\scripts\dump_goldens.py` already emits exhaustive golden vectors for
these two modules. Use them as the port's acceptance test — they are the only thing that can
establish parity.

* `C:\Users\khaled\src\likhi\tests\goldens\textnorm.jsonl` — **1592 lines**, 171 213 bytes.
  Per `dump_goldens.py:193-206`, one record per call:
  `{"fn": "normalize_roman"|"canonical"|"match_key"|"to_output"|"has_bengali"|"to_bangla_digits"|"to_western_digits", "in": …, "out": …}`
  — JSON with `sort_keys=True, ensure_ascii=True` (`dump_goldens.py:185`), so every non-ASCII
  character appears as a `\uXXXX` escape and you can diff without encoding worries.
* `C:\Users\khaled\src\likhi\tests\goldens\romankey.jsonl` — **974 lines**, 89 253 bytes.
  Per `dump_goldens.py:209-220`: `{"fn": "key_from_roman"|"key_from_bangla", "in": …, "level": "fine"|"coarse", "out": …}`,
  emitted for `level` in `("fine", "coarse")` in that order.

`dump_goldens.py:1-22` states the contract explicitly: these are **exact-match** goldens, "no
floating point in them and no excuse for a difference".

Edge inputs are listed verbatim at `dump_goldens.py:39-83` (`EDGE_ROMAN`, 43 entries incl. `""`,
`" "`, `"a'b"`, `"don't"`, `"café"`, `"naïve"`, `"ঢাকা"`, `"amarআমার"`, `"x"*64`, `"amar\tamar"`)
and `dump_goldens.py:86-108` (`EDGE_BANGLA`, 21 entries incl. both nukta encodings of ড়/ঢ়/য়, the
legacy khanda-ta sequence, `কঁা`/`কাঁ`, `র‍্য`/`র্য`, `ৰ`, `ৱ`, `০১২৩৪৫৬৭৮৯`).

Unit tests that encode the intent (weaker than the goldens but readable):
`C:\Users\khaled\src\likhi\tests\test_textnorm.py` and
`C:\Users\khaled\src\likhi\tests\test_romankey.py`.

---

## 11. Gotchas a naive port gets wrong

1. **`casefold`, not `to_lowercase`.** See §6.1. `"Straße"` → `"strasse"` in Python, `"strae"` with
   Rust's `to_lowercase`. Also affects `is_romanish`.
2. **NFC *decomposes* ড়/ঢ়/য়.** U+09DC/09DD/09DF are composition exclusions. After `canonical`
   every nukta letter is base + U+09BC. A port that hand-rolls "NFC" by composing pairs will produce
   precomposed forms and mismatch every lexicon lookup.
3. **`to_output` is deliberately not NFC.** It re-composes exactly the three nukta pairs after
   `canonical`. Do not normalise afterwards.
4. **`canonical` is not idempotent** despite its docstring (§3.4). Apply the candrabindu
   substitution **once**, non-overlapping, left-to-right, with no re-scan of the replacement.
5. **The candrabindu regex moves the sign past a whole greedy run** of vowel signs
   (`U+0995 U+0981 U+09BE U+09BF` → `U+0995 U+09BE U+09BF U+0981`), not past one sign.
6. **The `_VOWEL_SIGNS` class ≠ `_VOWELS` keys.** U+09C4, U+09E2, U+09E3 are reordering triggers in
   `textnorm` but invisible to `romankey`. Two different hard-coded sets; don't unify them.
7. **`match_key` runs `canonical` before stripping joiners**, so `ত ্ ZWJ` → `ৎ`. Reversing the order
   would yield `ত ্`.
8. **Joiner stripping can create a nukta pair**: `U+09A1 U+200D U+09BC` → `U+09A1 U+09BC`.
9. **Roman matching is a fixed 3-then-2-then-1 window with a length guard**, no backtracking.
   `ksh`→`kh` but `ks`→`k`; `cks`→`['k','s']`; `sch`→`['s','ch']`.
10. **Unknown roman characters advance `i` by 1 and emit nothing** — they must not resynchronise the
    window or abort the loop.
11. **`_finalize`'s `i == 0` is the index into the *units list*.** Leading skipped characters
    (digits, apostrophes, latin letters in a Bangla word) do not shift it.
12. **Only the first unit can be a vowel in the output**; every later vowel is dropped, at both levels.
13. **`y` is handled before the coarse map, not by it.** Initial `y` → `j`; non-initial `y` → dropped,
    at *both* levels.
14. **Dedup runs after the coarse merge**, so distinct fine classes that merge coarsely collapse into
    one slot (`kh`+`k` → `k`). At fine level they do not.
15. **Coarse `Vo` → `a`, not `o`.** And coarse `Vri` → `r`, which can then be eaten by an adjacent
    `r` consonant (§9.3).
16. **Ba-phala is dropped only when `units` is non-empty**; ya-phala is converted at any position.
17. **The nukta look-ahead applies after *any* consonant**, not just ড/ঢ/য — it consumes the nukta
    and emits the plain consonant class (`ক` + nukta → `k`).
18. **Any `level` string other than exactly `"coarse"` means fine.** If the Rust port uses an enum,
    make sure every call site that currently passes a string maps identically.
19. **`has_bengali` covers the whole U+0980–U+09FF block, digits included.**
20. **Index in character units, not bytes.** Both tokenisers use Python code-point indexing.
21. **`normalize_roman` keeps ASCII `'` (U+0027) but not `’` (U+2019)**, and keeps ASCII digits
    (which `_roman_units` then discards).

---

## 12. Dead code — present in Python, provably unreachable

Reproduce the *behaviour*, not these entries. Listed so a reviewer diffing the tables does not think
something was lost.

1. **`_CONS` rows 33–35** (`romankey.py:58-60`). Byte-level check of the source shows these three
   keys are stored **decomposed** — `"ড়"` in that file is `U+09A1 U+09BC`, two characters — whereas
   the identical-looking literals in `textnorm.py:31-33` are the single precomposed codepoints.
   `_bangla_units` only ever does `ch in _CONS` / `_CONS[ch]` with a **one-character** `ch`
   (`romankey.py:209`, `211`, `214`, `218`), so a two-character key can never match.
   The nukta look-ahead at `romankey.py:213-214` produces the same values (`r`, `rh`, `y`), so there
   is **no behavioural difference** — but a Rust port that keys the consonant map by `char` must
   simply omit those three rows, and one that keys by `&str` with longest-match will also land on the
   same answers. Either is fine.
2. **`_COARSE["y"] = ""`** (`romankey.py:160`). `_finalize` handles `u == "y"` at
   `romankey.py:187-191`, before the coarse map at line 194, and that branch either rewrites to `"j"`
   or `continue`s. No `"y"` ever reaches `_COARSE.get`.
3. **`if not u: continue`** (`romankey.py:195-196`). `""` is the only falsy value `_COARSE` can
   return, and it only comes from the unreachable `"y"` entry.
4. **`_CANDRABINDU`** (`romankey.py:93`) — defined, never referenced.
5. **`_CONS` rows 39–40** (ৰ U+09F0, ৱ U+09F1). `_bangla_units` always operates on `match_key` output,
   which has already folded them to র/ব. Unreachable through the public API, but harmless to keep.

---

## 13. Uncertain

Flagged rather than guessed.

1. **Which NFC/Unicode version the Rust port must target.** The reference behaviour above was
   produced with Unicode 15.0.0 tables (CPython 3.12.10). The Bengali canonical decompositions and
   the three composition exclusions have been stable for many Unicode versions, and I know of no
   Bengali-relevant NFC change between 15.0 and later releases — **but I did not verify that against
   a newer `unicode-normalization` crate.** If the port uses a crate with Unicode 16/17 tables, run
   the `textnorm.jsonl` goldens before assuming parity.
2. **Whether the "Idempotent" docstring at `textnorm.py:57` is an intended invariant or a stale
   comment.** The double-candrabindu counter-example is real (§3.4). I specified "match Python
   exactly, apply once". If the *intent* was a true fixpoint, that is a Python bug to fix first —
   fixing it in Rust only would break the goldens. **Needs a decision from the author.**
3. **Whether `_CONS` rows 33–35 being decomposed is deliberate or an editor/encoding accident.** The
   two files disagree on how the same three glyphs are stored. The behaviour is identical either way
   today, so nothing blocks the port, but it may indicate that the intent was for those rows to be
   reachable (they would be, if `_bangla_units` used a longest-match lookup) — in which case the
   ঢ়-value `rh` would be reached by a different path. It currently is reached, via the look-ahead
   table, so **no divergence exists**; I flag it only because a reviewer may "fix" one file and
   change nothing, or "fix" the algorithm and change something.
4. **`_finalize` on the initial-vowel append path skips the dedup check** (`romankey.py:186`). I
   argued this is harmless because `out` is empty at `i == 0`. That holds for `_finalize` as called
   today (always with `i` enumerating from 0). It would *not* hold if a future caller passed a
   pre-populated `out`. Not a porting risk now; noted so the Rust version's structure doesn't
   accidentally make it reachable.
5. **Behaviour under `level` values other than `"fine"`/`"coarse"`.** I verified empirically that
   anything `!= "coarse"` acts as fine, but I did not audit every call site in the wider codebase to
   confirm only those two literals are ever passed. `core.py:355-357` passes only `"fine"` and
   `"coarse"`.
6. **Bengali codepoints absent from both tables.** ঌ (U+098C), ৡ (U+09E1), ঽ (U+09BD), ৲৳৴–৹
   (currency/fractions), ৺ ৻ ৼ ৽ ৾ and the Assamese-specific ৎ variants are silently ignored by
   `_bangla_units`. I believe that is intentional (they are vanishingly rare in the lexicon) but the
   code carries no comment saying so.
7. **Performance characteristics of the Rust port are out of scope here.** Given the project's
   per-keystroke budget, note only that `key_from_roman` is on the hot path (`core.py:359`, called
   twice per keystroke) and is currently memoised at 65 536 entries.
