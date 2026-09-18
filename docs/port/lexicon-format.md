# Porting spec — the marisa lexicon: operations and producer

Status: written 2026-09-18 against the working tree at `C:\Users\khaled\src\likhi`.
Every count, ordering claim and payload range in this document was measured by loading the shipped
artifacts in `C:\Users\khaled\src\likhi\models\lexicon` with
`C:\Users\khaled\src\likhi\.venv\Scripts\python.exe` (marisa-trie **1.4.1**), not inferred from the
source. Where a claim is inferred rather than measured it is called out.

Primary sources:

- `src/likhi/engine/core.py` — consumer (`LikhiEngine.__init__`, `candidates()`, and the scoring
  helpers that hit the tries)
- `src/likhi/data/build_lexicon.py` — producer for `unigrams`, `romans`, `keys`, `prefixes`
- `src/likhi/data/build_bigrams.py` — producer for `bigrams`, `bigram_totals`
- `src/likhi/engine/textnorm.py` — `canonical`, `match_key`, `normalize_roman`, `to_output`
- `src/likhi/engine/romankey.py` — `key_from_roman`, `key_from_bangla`

---

## 0. Executive summary for the implementer

There are six on-disk tries. Only **three distinct access shapes** are ever used against them:

| Shape | Description | Used on |
|---|---|---|
| **A. exact get** | look a whole key up, get its payload(s) | `unigrams`, `bigrams`, `bigram_totals` |
| **B. prefix scan** | every entry whose key starts with a byte prefix | `romans`, `keys`, `prefixes` |
| **C. full scan** | every entry, once, at load time | `unigrams` (only) |

Nothing else is used: no ranged iteration, no longest-prefix match, no suffix or fuzzy search, no
deletion, no mutation. The tries are read-only after load.

The single biggest porting risk is **not** the file layout. It is that four of the tries store a
*composite* key `"<field1>\t<field2>"` and the engine does prefix scans that deliberately straddle
the `\t`. Two scans on `romans` differ only by whether the `\t` is included
(`romans.items(r + "\t")` vs `romans.items(r)`), and they mean completely different things. See
[§8 Ordering and tie-breaks](#8-ordering-and-tie-breaks) and
[§9 Short-input vs scan-path asymmetry](#9-short-input-vs-scan-path-asymmetry) for the two places a
naive port will silently diverge.

---

## 1. Physical format being replaced

All six files are `marisa_trie.RecordTrie` saves. A `RecordTrie` is a `BytesTrie` whose values are
fixed-size `struct`-packed tuples: the library stores the byte string
`utf8(key) + separator + struct.pack(fmt, *value)` in a plain MARISA LOUDS trie, and on lookup it
splits the stored string and `struct.unpack`s the tail.

Consequences that were **verified**, not assumed:

- Keys are **UTF-8** encoded `str`. Python-side keys are `str`; the trie is `_UnicodeKeyedTrie`.
- The value separator does **not** collide with payload bytes. `RecordTrie("<I")` round-trips
  `0xFFFFFFFF`, `0x00FF00FF` and `0`; `BytesTrie` round-trips a payload of `b"\xff\xff"`.
- `struct.calcsize` for the three formats in use: `"<III"` = **12 bytes**, `"<Ib"` = **5 bytes**,
  `"<I"` = **4 bytes**. The `"<"` prefix means little-endian *and no alignment padding* — `"<Ib"` is
  5 bytes, not 8. A Rust port that lays out `#[repr(C)] struct { u32, i8 }` gets 8 and is wrong if
  it tries to read the old files.
- `b` in `"<Ib"` is a **signed** `i8`. The only values ever written are the source bitmask 1..7, so
  it is de facto a `u8`, but the on-disk type is signed.
- **A key may appear more than once.** `RecordTrie` is a multimap. `romans.marisa` has 1,280,777
  entries over 1,280,757 distinct keys — **20 keys carry two records each**. See §3.5.
- marisa **de-duplicates entries that are identical in both key and payload**. Verified:
  `RecordTrie("<I", [("a",(1,)), ("a",(1,)), ("a",(2,)), ("b",(3,))])` yields
  `[('a',(1,)), ('a',(2,)), ('b',(3,))]`, `len == 3`. This is why `meta.json` reports
  `"romanizations": 1280779` while the file holds 1,280,777 entries — two `(key, payload)` pairs
  were byte-identical and collapsed.

### 1.1 Iteration order is NOT sorted

This was measured on all six files:

```
  unigrams       sorted_asc=False sorted_desc=False
  romans         sorted_asc=False sorted_desc=False
  keys           sorted_asc=False sorted_desc=False
  prefixes       sorted_asc=False sorted_desc=False
  bigrams        sorted_asc=False sorted_desc=False
  bigram_totals  sorted_asc=False sorted_desc=False
```

`romans.items("ki")` is likewise unsorted. MARISA orders nodes by build-time weight, not
lexicographically. `keys()` and `iterkeys()` agree with each other, and the order is stable across
reloads of the same file — but it is not an order the port can reproduce or should depend on.

**Why this matters:** see §8. Most call sites re-sort explicitly, so trie order is usually washed
out. It leaks into the output in exactly one place: the insertion order of the `feats` dict.

---

## 2. Conventions shared by all six tries

### 2.1 The `\t` separator (U+0009)

Four tries use a composite key `"<a>\tb"`. The separator is a literal ASCII tab, always exactly
one per key. Verified tab-count histograms over every key:

| trie | tabs per key |
|---|---|
| `unigrams` | `{0: 258392}` |
| `romans` | `{1: 1280777}` |
| `keys` | `{1: 516606}` |
| `prefixes` | `{1: 124267}` |
| `bigrams` | `{1: 618619}` |
| `bigram_totals` | `{0: 221100}` |

Parsing is always `key.split("\t", 1)` — **maxsplit 1**, take `[1]` for the word. A tab can never
appear in either field:

- Roman sides pass through `normalize_roman()`, which is
  `_RE_ROMAN_KEEP.sub("", text.casefold())` with `_RE_ROMAN_KEEP = re.compile(r"[^a-z0-9']+")`
  (`textnorm.py:93,101`) — so only `[a-z0-9']`.
- Word sides are Bengali tokens. Measured: **0 words contain a tab**, in any trie.

### 2.2 The word side never contains an ASCII letter

Measured across `unigrams`, `romans`, `keys`, `prefixes` and `bigrams`: **zero** word-side strings
contain `[A-Za-z]`. This is what makes the straddling prefix scans of §3.5 safe — `romans.items("ki")`
cannot accidentally match a key whose roman is `"k"` and whose word begins `"i"`, because no word
begins with an ASCII letter.

Word sides are *not* restricted to the Bengali block, though. 253 `romans` word sides and 246
`unigrams` keys contain punctuation that survived tokenisation:

```
'sarak\tদোহাজারী-বৈলতলী'   'shofol\tসফল?ও'      'sure\t১০০%শিওর'
'somvob\tসম্ভব"৷'          "সি'র"                'সি.সি'      'সড়ক-রেল'
```

Note `"সি'র"` — an apostrophe in a *word*, and `2` unigram keys contain `':'`. A port must not
assume word sides are `[\u0980-\u09FF]+`.

### 2.3 Roman-side charset actually present

Measured over `romans.marisa`: the distinct roman-side characters are `'abcdefghijklmnopqrstuvwxyz`
— a-z plus the apostrophe. 21 keys have a non-`a-z` roman side. **No digits appear**, although
`normalize_roman` permits `0-9`. So a user who types a digit produces an `r` that can never match
any stored roman; that is existing behaviour and must be preserved (no special-casing).

`prefixes.marisa` key sides use `'abcdefghijklmnopqrstuvwxyz` plus `:`.
`keys.marisa` key sides use only `:abcdefghijklmnoprstu` — the phonetic class alphabet plus `:`;
`q v w x y z` never appear as fine/coarse classes in the shipped data.

### 2.4 Word normalisation forms — three distinct forms, do not conflate

| form | function | what it does |
|---|---|---|
| `canonical(t)` | `textnorm.py:55` | NFC, legacy khanda-ta → `ৎ`, candrabindu moved *after* the vowel sign. Joiners (ZWJ/ZWNJ) **kept**. |
| `match_key(t)` | `textnorm.py:66` | `canonical` + joiners stripped + Assamese `ৰ→র`, `ৱ→ব`. |
| `to_output(t)` | `textnorm.py:73` | `canonical` + nukta **re**composed (`ড`+`়` → `ড়`). |

**Everything stored in every trie is in `canonical` form** (`canonical` is applied on every token in
`build_lexicon.tokens()`, line 45, and in `collect_romans.add()`, line 131). `match_key` is used
only as a *grouping* key inside the builder; it is never a trie key. `to_output` is applied only on
the way out to the user (`core.py:536, 579`).

Practical trap for the port: the shell shows `to_output(word)`, which has a **precomposed** `ড়`.
If that word comes back as context, `canonical()` re-decomposes it to `ড`+`়` before the bigram
lookup (`core.py:593`). Skipping that round trip breaks every bigram whose previous word has a nukta.

### 2.5 Surface-form collapsing (why duplicate `romans` keys exist)

`build_lexicon.build()` picks **one** surface spelling per `match_key`:

```python
# build_lexicon.py:243-250
def surface(k: str) -> str:
    for sc in (chat, subs, wiki):
        if k in sc.counts:
            return sc.best_surface(k)
    best = rom_surface.get(k)
    return best.most_common(1)[0][0] if best else k

surfaces = {k: surface(k) for k in keys}
```

Priority is **chat, then subs, then wiki**, then the most-attested romanisation surface, then the
`match_key` itself. `SurfaceCounter.best_surface` is `self.surface[k].most_common(1)[0][0]`
(line 60-61) — `Counter.most_common` ties are broken by *first-insertion order*, i.e. corpus order.

`romans` is then written with the collapsed surface:

```python
# build_lexicon.py:260-265
rom_items = []
for (r, w), (c, src) in romans.items():
    k = match_key(w)
    rom_items.append((f"{r}\t{surfaces.get(k, w)}", (min(c, 2**31 - 1), src)))
```

Two different raw words `w1 != w2` with `match_key(w1) == match_key(w2)` and the same roman `r`
collapse onto the same trie key with *different payloads*. That is the exact mechanism behind the
20 duplicate keys. Note the fallback `surfaces.get(k, w)`: Aksharantar-only words are not in
`keys`/`surfaces`, so they keep their own raw spelling.

### 2.6 Payload capping

Three of the four builders clamp to `2**31 - 1` = **2147483647** before writing into an
*unsigned* `<I` field — so the effective ceiling is 2^31-1, not 2^32-1:

- `build_lexicon.py:263` `min(c, 2**31 - 1)` (romans count)
- `build_lexicon.py:274` `min(score, 2**31 - 1)` (keys score)
- `build_lexicon.py:208` `min(int(s), 2**31 - 1)` (prefixes score)
- `build_bigrams.py:87, 91` `min(n, 2**31 - 1)` (bigram count, bigram total)

`unigrams` is **not** capped — raw `Counter` values go in directly (`build_lexicon.py:251-257`).
Measured maxima are far below the ceiling (§10), so no clamp is currently active, but the port must
keep the clamp to stay byte-identical on a larger corpus.

---

## 3. The six tries

### 3.1 `unigrams.marisa`

**Format string:** `"<III"` — 3 × little-endian `u32`, 12 bytes.
**Loaded:** `core.py:184-185`, unconditionally. Missing file = hard failure.

```python
self.uni = marisa_trie.RecordTrie("<III")
self.uni.load(str(lexicon_dir / "unigrams.marisa"))
```

**Key:** the bare word, no separator, no prefix. `canonical` form. Verified 0 tabs, 0 ASCII letters.

**Payload:** `(wiki, subs, chat)` — three independent corpus counts.

| field | source | corpus |
|---|---|---|
| `a` = wiki | Dakshina native-script Wikipedia `bn.wiki-filt.train.text.shuf.txt.gz` | formal |
| `b` = subs | FrequencyWords `bn_full.txt` (OpenSubtitles) | conversational |
| `c` = chat | BanglaTLit train rows, Bengali side | chat |

**Producer** (`build_lexicon.py:251-258`):

```python
uni = marisa_trie.RecordTrie(
    "<III",
    [
        (surfaces[k], (wiki.counts.get(k, 0), subs.counts.get(k, 0), chat.counts.get(k, 0)))
        for k in keys
    ],
)
uni.save(str(out / "unigrams.marisa"))
```

Membership of `keys` (the lexicon) is decided at `build_lexicon.py:232-240`:

```python
keys |= {k for k, c in wiki.counts.items() if c >= min_wiki}   # min_wiki default 2
keys |= set(subs.counts)
keys |= set(chat.counts)
...
    if src & (SRC_DAKSHINA | SRC_BANGLATLIT):
        keys.add(k)
```

so: wiki words seen ≥ `min_wiki` (**default 2**, `build()` signature line 214), **all** subs words,
**all** chat words, plus every word with a Dakshina or BanglaTLit romanisation. Aksharantar-only
words (`SRC_AKSHARANTAR = 2` alone) are deliberately **excluded** — "they are mostly named entities
and would swamp the key index" (line 228-230). Measured: 1,141,058 distinct words appear on the
`romans` word side, of which **981,659 are not in `unigrams`**.

**Operations performed by `core.py`:**

1. **Full scan, once, at construction** (`core.py:202-206`):

   ```python
   total = 0
   for _w, (a, b, c) in self.uni.items():
       total += a + 3 * b + 20 * c
   self._uni_total = float(total) + 1.0
   self._floor = math.log(0.5 / self._uni_total)
   ```

   Measured on the shipped file: `total = 25776947`, `_uni_total = 25776948.0`,
   `_floor = -17.75813834268104`. Note the `+ 1.0` and the `0.5` numerator — they are not the same
   smoothing constant and must not be merged.

   *Duplicate-key caveat:* this sums over `items()`, so if `unigrams` ever gained a duplicate key
   the total would double-count it. It currently has none (258,392 entries, 258,392 distinct keys).

2. **Exact get** in `unigram_logp` (`core.py:258-263`):

   ```python
   rec = self.uni.get(word)
   if not rec:
       return self._floor
   a, b, c = rec[0]
   return math.log((a + 3 * b + 20 * c + 0.5) / self._uni_total)
   ```

   `get` returns a **list of tuples** or `None`; `rec[0]` is the *first* record. `if not rec` also
   catches an empty list. Weights **1 / 3 / 20** and the `+ 0.5` add-half are load-bearing.

3. **Exact get** in `_lex_score` (`core.py:265-270`):

   ```python
   rec = self.uni.get(word)
   if not rec:
       return 0
   a, b, c = rec[0]
   return a + 3 * b + 20 * c
   ```

   Same 1/3/20 mixture, **no** `+0.5`, **no** log, returns `int`. `0` for an unknown word.

4. **Exact membership** in `candidates.f()` (`core.py:312-316`):

   ```python
   def f(word: str) -> Feats:
       word = canonical(word)
       if word not in feats:
           feats[word] = Feats(lex_score=self._lex_score(word), in_lexicon=word in self.uni)
       return feats[word]
   ```

   Verified: `__contains__` on a `RecordTrie` is **exact-key**, not prefix. For key
   `'সারাবিশ্বেই'`, the proper prefix `'সারাব'` gives `uni.get(...) is None`, `'সারাব' in uni ==
   False`, while `uni.has_keys_with_prefix('সারাব') == True`. Likewise `key + 'x' not in uni`.
   A Rust port must use exact lookup here; using "has any key with this prefix" would set
   `in_lexicon` true for half the vocabulary and silently disable the `oov` penalty (weight
   **-6.0** in the shipped `weights.json`).

**Entry count: 258,392** (258,392 distinct keys, 1,355,816 bytes).

Also read from the same directory by the *bigram builder* (`build_bigrams.py:28-33`) to build the
surface map:

```python
uni = marisa_trie.RecordTrie("<III")
uni.load(str(lexicon_dir / "unigrams.marisa"))
return {match_key(w): w for w in uni.keys()}
```

— a **full key scan**, `match_key(w) -> w`. Collisions would silently overwrite; none occur because
`surfaces` is injective by construction.

---

### 3.2 `romans.marisa`

**Format string:** `"<Ib"` — `u32` count + `i8` source bitmask, **5 bytes**, no padding.
**Loaded:** `core.py:186-187`, unconditionally.

**Key:** `f"{roman}\t{word}"`.

- `roman`: output of `normalize_roman()` — case-folded, `[a-z0-9']` only. No leading/trailing
  marker, no length prefix.
- `word`: the **collapsed surface** (`surfaces.get(match_key(w), w)`), `canonical` form.

**Payload:** `(count, source_bits)`.

- `count` — summed attestations, `min(c, 2**31-1)`.
- `source_bits` — OR of `SRC_DAKSHINA = 1`, `SRC_AKSHARANTAR = 2`, `SRC_BANGLATLIT = 4`
  (`build_lexicon.py:34`). Measured distribution over all 1,280,777 entries:

  | bits | meaning | entries |
  |---|---|---|
  | 1 | Dakshina | 14,225 |
  | 2 | Aksharantar | 1,151,123 |
  | 3 | Dak + Aksh | 71,392 |
  | 4 | BanglaTLit | 31,121 |
  | 5 | Dak + BTLit | 4,006 |
  | 6 | Aksh + BTLit | 3,987 |
  | 7 | all three | 4,923 |

  **`core.py` never reads `source_bits`.** Both call sites unpack it into `_src` and discard it
  (`core.py:322, 334`). It is dead weight at runtime and is only consumed by the *builder* at
  `build_lexicon.py:239` (`if src & (SRC_DAKSHINA | SRC_BANGLATLIT)`). A Rust port may drop it from
  the runtime format, but **must** keep it in the producer.

**Producer:** `collect_romans()` (`build_lexicon.py:125-166`) accumulates
`(roman, word) -> [count, source_bits]` with

```python
e = rom[(roman, word)]
e[0] += n
e[1] |= src
```

Contribution weights: Dakshina uses `int(it.weight)` (line 140), Aksharantar uses `1` (line 143),
BanglaTLit uses `1` per positionally aligned token pair (line 163). BanglaTLit alignment requires
`len(r_toks) == len(b_toks)` after `_strip_punct`, plus `_RE_LATIN_ONLY.match(r)`
(`^[A-Za-z']+$`), `has_bengali(b)` and `not re.search(r"[A-Za-z]", b)` (lines 156-163).

**Operations performed by `core.py` — two different prefix scans:**

**(a) exact-roman scan** (`core.py:321-326`):

```python
for key, (count, _src) in self.romans.items(r + "\t"):
    word = key.split("\t", 1)[1]
    ft = f(word)
    ft.rom_exact += count
    ft.sources.add("rom")
```

The prefix is `r + "\t"`, so this enumerates every *word* attested for exactly this roman string.
It is a **prefix scan used as a multimap get** — there is no single-key lookup here.
`ft.rom_exact` **accumulates with `+=`**, which matters because of the duplicate keys (§3.5).

**(b) completion scan** (`core.py:333-345`), taken when `self.prefixes is None or len(r) > 3`:

```python
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
```

Prefix is bare `r` (**no** `\t`), so it straddles the separator and returns every entry whose roman
*starts with* `r`. Exact matches are then dropped with `if rom == r: continue`. The score formula
is exactly `count * 10_000 + min(self._lex_score(word), 9_999)` — attestation dominates, corpus
frequency is a tie-break capped at 9,999. `gap` is `len(rom) - len(r)` in **characters**.

Scan sizes are why the prefix table exists — measured:

| `r` | `\|romans.items(r + "\t")\|` | `\|romans.items(r)\|` |
|---|---|---|
| `ki` | 45 | 5,561 |
| `ami` | 43 | 915 |
| `amr` | 10 | 476 |
| `bangla` | 8 | 803 |

**Entry count: 1,280,777** (1,280,757 distinct keys — 20 duplicated; 12,041,904 bytes).

---

### 3.3 `keys.marisa`

**Format string:** `"<I"` — one `u32`, 4 bytes.
**Loaded:** `core.py:188-189`, unconditionally.

**Key:** `f"{level[0]}:{phonetic_key}\t{word}"`.

- `level[0]` is a **single character**: `"f"` for `fine`, `"c"` for `coarse`. Derived at
  `build_lexicon.py:271-274` (`for level in ("fine", "coarse")` → `f"{level[0]}:{pk}\t{w}"`) and
  at `core.py:355-362` (`prefix = f"{level[0]}:{k}"` is line 362).
- The separator between level and key is a **colon**, between key and word a **tab**.
- `phonetic_key` = `key_from_bangla(word, level)` (`romankey.py:270-271`). Measured key-side
  charset: `:abcdefghijklmnoprstu`.

**Payload:** `(score,)` where

```python
# build_lexicon.py:270
score = wiki.counts.get(k, 0) + 3 * subs.counts.get(k, 0) + 20 * chat.counts.get(k, 0)
```

— i.e. **numerically identical to `_lex_score(word)`**, clamped by `min(score, 2**31 - 1)`. This
redundancy is deliberate: the score is available from a key scan without a second `unigrams` lookup.

**Producer** (`build_lexicon.py:267-276`):

```python
key_items = []
for k in keys:
    w = surfaces[k]
    score = wiki.counts.get(k, 0) + 3 * subs.counts.get(k, 0) + 20 * chat.counts.get(k, 0)
    for level in ("fine", "coarse"):
        pk = key_from_bangla(w, level)
        if pk:
            key_items.append((f"{level[0]}:{pk}\t{w}", (min(score, 2**31 - 1),)))
```

The `if pk:` guard drops words with an empty phonetic key. Measured: `{'c': 258303, 'f': 258303}`
— both levels present for the same 258,303 words, so **89 of the 258,392 unigram words have no key
at all**; they are all-digit tokens (`'১৯'`, `'৳১০০০'`, `'৩০০'`, `'৪'`, …). Conversely **0** words
in `keys` are missing from `unigrams`.

**Operations performed by `core.py`** — one prefix scan, in two variants
(`core.py:355-373`):

```python
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
```

- **Short-key variant** (`prefixes` loaded **and** `len(k) <= 2`): `keys.items(prefix + "\t")` —
  exact-key words only; the "longer" set comes from `prefixes` instead.
- **General variant:** `keys.items(prefix)` straddles the `\t`, returning both exact-key words and
  words whose key merely *starts with* `k`; the split is done in Python by `kk == prefix`.

Measured scan sizes (why the short-key variant exists):

| prefix | total from `keys.items(prefix)` | of which exact-key |
|---|---|---|
| `f:k` | 23,106 | 266 |
| `c:k` | 23,106 | 346 |
| `f:km` | 1,781 | 128 |
| `c:km` | 1,908 | 149 |
| `f:amr` | 223 | 74 |
| `c:amr` | 350 | 104 |

**Entry count: 516,606** (516,606 distinct keys; 3,542,768 bytes).

---

### 3.4 `prefixes.marisa`

**Format string:** `"<I"` — one `u32`, 4 bytes.
**Loaded:** `core.py:190-193` — **optional**:

```python
self.prefixes = None
if (lexicon_dir / "prefixes.marisa").exists():
    self.prefixes = marisa_trie.RecordTrie("<I")
    self.prefixes.load(str(lexicon_dir / "prefixes.marisa"))
```

If absent, `candidates()` falls back to the full scans everywhere. A port must keep the fallback
path working, because the two paths produce **different features** (§9).

**Key:** two namespaces sharing one trie.

| namespace | key grammar | example |
|---|---|---|
| roman completions | `"r:" + roman[:n] + "\t" + word`, `n` in 1..3 | `r:ki\tকিন্তু` |
| phonetic-key completions | `"k:" + level_char + ":" + pk[:n] + "\t" + word`, `n` in 1..2 | `k:f:k\tকরবে` |

Measured shape:

| namespace | prefix length | entries |
|---|---|---|
| `r:` | 1 | 624 |
| `r:` | 2 | 11,609 |
| `r:` | 3 | 97,547 |
| `k:f:` | 1 | 480 |
| `k:f:` | 2 | 7,087 |
| `k:c:` | 1 | 456 |
| `k:c:` | 2 | 6,464 |

10,166 distinct short prefixes; **max 24 words per prefix, min 1, none over 24** — the
`PREFIX_TOP = 24` cap is real and uniform.

**Payload:** `(score,)`, and the score means **two different things** depending on namespace:

- `r:` — `count * 10_000 + min(lex_score(word), 9_999)`, the same formula as the runtime scan path.
- `k:` — the plain phonetic-key score (= `_lex_score`).

Measured range: min 2, max 36,259,999 (= 3625 × 10000 + 9999, consistent with the maximum `romans`
count of 3,625).

**Producer** — `build_prefix_tables()` (`build_lexicon.py:174-210`), reproduced in full because
every constant in it is load-bearing:

```python
SHORT_ROMAN_PREFIX = 3  # completions are precomputed for typed prefixes up to this length
SHORT_KEY_PREFIX = 2
PREFIX_TOP = 24

def build_prefix_tables(out: Path, rom_items: list, key_items: list) -> None:
    import marisa_trie

    lex: dict[str, int] = {}
    for key, (score,) in key_items:
        lex[key.split("\t", 1)[1]] = score
    best: dict[str, dict[str, int]] = defaultdict(dict)
    for key, (count, _src) in rom_items:
        roman, word = key.split("\t", 1)
        for n in range(1, min(SHORT_ROMAN_PREFIX, len(roman)) + 1):
            p = "r:" + roman[:n]
            # attestation count first, corpus frequency only to break ties: a single noisy
            # alignment of a very frequent word must not outrank a well-attested completion
            s = count * 10_000 + min(lex.get(word, 0), 9_999)
            if s > best[p].get(word, -1):
                best[p][word] = s
    for key, (score,) in key_items:
        kk, word = key.split("\t", 1)
        level, pk = kk.split(":", 1)
        for n in range(1, min(SHORT_KEY_PREFIX, len(pk)) + 1):
            p = f"k:{level}:{pk[:n]}"
            if score > best[p].get(word, -1):
                best[p][word] = score
    items = []
    for p, words in best.items():
        for w, s in sorted(words.items(), key=lambda kv: -kv[1])[:PREFIX_TOP]:
            items.append((f"{p}\t{w}", (min(int(s), 2**31 - 1),)))
    marisa_trie.RecordTrie("<I", items).save(str(out / "prefixes.marisa"))
```

Producer details a port must copy exactly:

1. `lex` is built **from `key_items`, not from `unigrams`**. A word with no phonetic key is absent
   from `key_items`, so `lex.get(word, 0)` returns 0 for it even though `_lex_score` would return a
   non-zero value. Affects the 89 all-digit words only, but it is a real divergence between the
   build-time score and the runtime score.
2. `best[p][word]` keeps the **maximum** over all romans sharing the prefix (`if s > ...get(word, -1)`).
   The sentinel is `-1`, so a genuine score of `0` still stores.
3. `n` ranges **1..min(3, len(roman))** for romans, so `r:<roman>` itself is generated when the
   roman is 1-3 characters long. **The roman prefix table therefore contains exact-roman
   completions**, which the runtime scan path explicitly skips. Verified: for `r = "ki"`, the exact
   set has 45 words, the prefix table has 24, and **5 overlap** (`কি`, `কিছু`, `কিভাবে`, `কী`,
   `কীভাবে`).
4. Likewise the key table contains exact-key words. Verified overlaps between
   `keys.items("<lvl>:<k>\t")` and `prefixes.items("k:<lvl>:<k>\t")`: `f:k` 4, `c:k` 4, `f:km` 5,
   `c:am` 7.
5. `sorted(words.items(), key=lambda kv: -kv[1])[:PREFIX_TOP]` is Python's **stable** sort keyed on
   the negated score only. Equal scores at the 24-item boundary are resolved by `dict` insertion
   order, i.e. by the order of `rom_items` / `key_items`, i.e. by the iteration order of the
   builder's `defaultdict`s. **This is not reproducible from the spec alone** — see §11.
6. The two namespaces share one `best` dict but can never collide, because `r:` and `k:` differ in
   the first character.

**Operations performed by `core.py`** — always an exact-prefix scan with the `\t` included:

```python
# core.py:328-332  (roman completions, only when len(r) <= 3)
if self.prefixes is not None and len(r) <= 3:
    for key, (score,) in self.prefixes.items(f"r:{r}\t"):
        word = key.split("\t", 1)[1]
        completions.append((score, word, 1, 2))

# core.py:368-369  (key completions, only when len(k) <= 2)
for key, (score,) in self.prefixes.items(f"k:{level[0]}:{k}\t"):
    longer.append((score, key.split("\t", 1)[1]))
```

Note `completions.append((score, word, 1, 2))`: `count` is hard-wired to **1** and `gap` to **2**,
regardless of the real attestation count or real length difference. See §9.

**Entry count: 124,267** (124,267 distinct keys; 807,264 bytes).

---

### 3.5 `bigrams.marisa`

**Format string:** `"<I"` — one `u32`, 4 bytes.
**Loaded:** `core.py:194-200` — **optional**, and gated on `bigrams.marisa` only:

```python
self.bigrams = None
self.bigram_totals = None
if (lexicon_dir / "bigrams.marisa").exists():
    self.bigrams = marisa_trie.RecordTrie("<I")
    self.bigrams.load(str(lexicon_dir / "bigrams.marisa"))
    self.bigram_totals = marisa_trie.RecordTrie("<I")
    self.bigram_totals.load(str(lexicon_dir / "bigram_totals.marisa"))
```

**`bigram_totals.marisa` is loaded without an existence check.** If `bigrams.marisa` is present and
`bigram_totals.marisa` is not, construction raises. Preserve or fix deliberately, but be aware.

**Key:** `f"{prev}\t{word}"` — both sides `canonical` surface forms taken from `unigrams`.
`prev` may be the literal sentence-start token `"<s>"` (`build_bigrams.py:25`).

**Payload:** `(count,)` — weighted co-occurrence count, `min(n, 2**31-1)`.

**Producer** (`build_bigrams.py:63-90`):

```python
surf = _surface_map(lexicon_dir)          # match_key(w) -> w, from unigrams.marisa
counts: dict[str, Counter[str]] = defaultdict(Counter)
totals: Counter[str] = Counter()
for text, weight in _sentences(raw, wiki_lines):
    prev = START                           # "<s>"
    for tok in tokens(text):
        w = surf.get(match_key(tok))
        if w is None:
            prev = None                    # unknown word breaks the chain
            continue
        if prev is not None:
            counts[prev][w] += weight
            totals[prev] += weight
        prev = w
    ...
items = []
kept = 0
for prev, c in counts.items():
    for w, n in c.most_common(per_prev):
        if n < min_count:
            break
        items.append((f"{prev}\t{w}", (min(n, 2**31 - 1),)))
        kept += 1
```

Constants and rules a port must reproduce:

- **Sentence weights:** Dakshina wiki lines weight **1**, BanglaTLit chat sentences weight **5**
  (`build_bigrams.py:43, 48`).
- **OOV breaks the chain.** An unknown token sets `prev = None`, so the pair *across* it is not
  counted at all — it is not merely skipped as a right-hand word, the *next* pair is dropped too.
- **`<s>` conditions the first word** of each sentence and is reset per sentence.
- **Pruning:** `per_prev = 60` (top-60 by `Counter.most_common`), then `min_count = 2` with a
  **`break`, not `continue`** — `most_common` is descending, so the break truncates the tail.
  `most_common` ties are resolved by insertion order.
- Totals are accumulated **before** pruning, so `sum(bigram counts for prev) <= totals[prev]`.

Verified against the shipped file: max per-`prev` fan-out = **60**, min stored count = **2**,
all 618,619 right-hand words are in `unigrams`, all 108,222 distinct `prev` values appear in
`bigram_totals`.

**Operations performed by `core.py`** — one **exact get**, in `context_adjust`
(`core.py:272-288`):

```python
def context_adjust(self, word: str, context: Sequence[str]) -> float:
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
```

- Only `context[-1]` is used — this is a **strict bigram**, no trigram, no longer history.
- The key is built by string concatenation `f"{prev}\t{word}"`; a port storing `(prev, word)` as a
  tuple is fine, but the tab must not be forgotten if keys are concatenated.
- **Stupid backoff penalty `math.log(0.4)` = -0.9162907318741551**, returned *unconditionally*
  whenever `prev` has a total but the pair is missing. It is **not** multiplied by anything and
  **not** offset by `unigram_logp`. This asymmetry (the hit branch subtracts `unigram_logp`, the
  miss branch does not) is intentional per the docstring but is exactly the kind of thing a port
  "tidies up".
- Returns **0.0** when `prev` is unknown to `bigram_totals`, so context never hurts when
  uninformative.
- The caller multiplies by `w["bigram"]` = **0.7** (`core.py:481`).
- **`<s>` is never queried at runtime.** `context[-1]` is always a real committed word
  (`core.py:593`, `prev = (canonical(context[-1]),)`). The 60 `<s>\t…` entries and the
  `<s>` total (1,062,381) are dead weight at runtime. Do not "fix" this by injecting `<s>` at
  sentence start — it would change every first-word ranking.

**Entry count: 618,619** (618,619 distinct keys; 3,460,360 bytes).

---

### 3.6 `bigram_totals.marisa`

**Format string:** `"<I"` — one `u32`, 4 bytes.
**Loaded:** `core.py:199-200` (see §3.5 for the missing existence check).

**Key:** the bare `prev` word — no separator, no prefix. Plus the literal `"<s>"`.

**Payload:** `(total,)` — the weighted count of every bigram observed with this `prev`,
**before** the `per_prev`/`min_count` pruning. This is why the table has 221,100 keys while only
108,222 of them survive into `bigrams`: **112,878 `prev` values have a total but no stored
continuation at all**. For those, `context_adjust` takes the `math.log(0.4)` branch for every
candidate — a constant shift that cancels in the ranking, but the port must still return the value
rather than short-circuiting to `0.0`.

**Producer** (`build_bigrams.py:91-92`):

```python
tot = marisa_trie.RecordTrie("<I", [(p, (min(n, 2**31 - 1),)) for p, n in totals.items()])
tot.save(str(lexicon_dir / "bigram_totals.marisa"))
```

**Operation:** a single **exact get**, `self.bigram_totals.get(prev)` (`core.py:281`), result used
as `float(tot[0][0])`. `if not tot` catches both `None` and an empty list; note it would *also*
catch a stored total of… no — `tot` is the list `[(0,)]` which is truthy. A stored total of **0**
would reach `float(0.0)` and then `math.log(rec[0][0] / 0.0)` → `ZeroDivisionError`. No zero totals
exist in the shipped file (min is ≥ 1 by construction), but a port should not introduce one.

**Entry count: 221,100** (221,100 distinct keys; 1,054,640 bytes).

---

## 4. Verified entry counts

Loaded with `C:\Users\khaled\src\likhi\.venv\Scripts\python.exe`, marisa-trie 1.4.1, against
`C:\Users\khaled\src\likhi\models\lexicon` (built 2026-09-16).

| trie | format | entries (`len(items())`) | distinct keys | file bytes |
|---|---|---|---|---|
| `unigrams.marisa` | `<III` | **258,392** | 258,392 | 1,355,816 |
| `romans.marisa` | `<Ib` | **1,280,777** | 1,280,757 | 12,041,904 |
| `keys.marisa` | `<I` | **516,606** | 516,606 | 3,542,768 |
| `prefixes.marisa` | `<I` | **124,267** | 124,267 | 807,264 |
| `bigrams.marisa` | `<I` | **618,619** | 618,619 | 3,460,360 |
| `bigram_totals.marisa` | `<I` | **221,100** | 221,100 | 1,054,640 |

Total on-disk: 22,261,752 bytes across the six tries.

`len(trie)` equals `len(items())` for all six, i.e. marisa's `__len__` counts *records*, not
distinct keys.

`meta.json` reports `"words": 258392`, `"romanizations": 1280779`, `"phonetic_keys": 516606`.
The `romanizations` figure is the pre-dedup item count and is **2 higher** than the file
(§1). `bigrams_meta.json` reports `"kept": 618619`, `"prev_types": 221100` — both exact.

Observed payload ranges:

| trie | field | min | max |
|---|---|---|---|
| `unigrams` | wiki | 0 | 155,204 |
| `unigrams` | subs | 0 | 62,511 |
| `unigrams` | chat | 0 | 7,865 |
| `unigrams` | `a+3b+20c` | — | 163,494 (`এবং`) |
| `romans` | count | 1 | 3,625 |
| `romans` | src | 1 | 7 |
| `keys` | score | 0 | 342,722 |
| `prefixes` | score | 2 | 36,259,999 |
| `bigrams` | count | 2 | 35,925 |
| `bigram_totals` | total | — | 1,062,381 (`<s>`) |

Derived constants on this build: `_uni_total = 25776948.0`, `_floor = -17.75813834268104`.

---

## 5. Minimum operation set a replacement must support

A replacement store must provide exactly this much, and nothing more is used:

### 5.1 Unigram store (`unigrams`)

1. `get_exact(word) -> Option<(u32, u32, u32)>` — exact key, first record.
2. `contains_exact(word) -> bool` — exact key, **not** prefix.
3. `iter_all() -> impl Iterator<Item = (&str, (u32,u32,u32))>` — once at load, to compute
   `_uni_total`. May be replaced by a precomputed constant **only if** the constant is rebuilt with
   the data; it is `sum(a + 3b + 20c)` over all records.
4. (Producer side / bigram build) `iter_keys()` to build `match_key(w) -> w`.

### 5.2 Romanisation store (`romans`)

1. `scan_exact_roman(r) -> impl Iterator<Item = (&str word, u32 count)>` — all words for roman
   exactly `r`. **Must be a multimap**: it can yield the same word more than once and the caller
   sums the counts.
2. `scan_roman_prefix(r) -> impl Iterator<Item = (&str roman, &str word, u32 count)>` — all entries
   whose roman **starts with** `r`, *including* roman == `r` (the caller filters those out itself).
   The caller needs the full roman string to compute `len(rom) - len(r)`.
3. Source bits are not needed at runtime.

### 5.3 Phonetic-key store (`keys`)

1. `scan_key_exact(level_char, k) -> impl Iterator<Item = (&str word, u32 score)>` — words whose
   key for that level is exactly `k`.
2. `scan_key_prefix(level_char, k) -> impl Iterator<Item = (&str key, &str word, u32 score)>` —
   words whose key **starts with** `k`, including exact; the caller partitions on `key == k`.

### 5.4 Precomputed-completion store (`prefixes`)

1. `top_roman_completions(r) -> impl Iterator<Item = (&str word, u32 score)>` for
   `1 <= len(r) <= 3`; ≤ 24 results.
2. `top_key_completions(level_char, k) -> impl Iterator<Item = (&str word, u32 score)>` for
   `1 <= len(k) <= 2`; ≤ 24 results.
3. Must be **optional**: absence switches both call sites to the scan paths.

### 5.5 Bigram store (`bigrams`, `bigram_totals`)

1. `bigram_total(prev) -> Option<u32>` — exact.
2. `bigram_count(prev, word) -> Option<u32>` — exact.
3. Must be **optional** as a pair.

### 5.6 Not needed

No insert, no delete, no update, no range scan, no longest-prefix match, no nearest-neighbour, no
`has_keys_with_prefix`, no suffix search, no serialization back out at runtime. The engine never
iterates `romans`, `keys` or `prefixes` in full at runtime.

---

## 6. Trie call sites in `core.py` — complete inventory

| # | line | trie | call | shape |
|---|---|---|---|---|
| 1 | 203 | `uni` | `self.uni.items()` | full scan (once, at construction) |
| 2 | 259 | `uni` | `self.uni.get(word)` | exact get (`unigram_logp`) |
| 3 | 266 | `uni` | `self.uni.get(word)` | exact get (`_lex_score`) |
| 4 | 315 | `uni` | `word in self.uni` | exact contains |
| 5 | 281 | `bigram_totals` | `.get(prev)` | exact get |
| 6 | 285 | `bigrams` | `.get(f"{prev}\t{word}")` | exact get |
| 7 | 322 | `romans` | `.items(r + "\t")` | prefix scan (exact roman) |
| 8 | 330 | `prefixes` | `.items(f"r:{r}\t")` | prefix scan (short roman only) |
| 9 | 334 | `romans` | `.items(r)` | prefix scan (straddles `\t`) |
| 10 | 366 | `keys` | `.items(prefix + "\t")` | prefix scan (short key only) |
| 11 | 368 | `prefixes` | `.items(f"k:{level[0]}:{k}\t")` | prefix scan (short key only) |
| 12 | 371 | `keys` | `.items(prefix)` | prefix scan (straddles `\t`) |

Sites 2, 3 and 4 fire **per candidate word**, not per keystroke; sites 2 and 3 fire again during
scoring. Site 1 fires once per engine construction and is the load-time cost.

---

## 7. Formulae reproduced verbatim

Do not re-derive these. They are quoted from the source.

```python
# core.py:204-206 — normaliser, computed by full scan of unigrams
total += a + 3 * b + 20 * c
self._uni_total = float(total) + 1.0
self._floor = math.log(0.5 / self._uni_total)

# core.py:263 — unigram log-probability
return math.log((a + 3 * b + 20 * c + 0.5) / self._uni_total)

# core.py:270 — integer corpus score
return a + 3 * b + 20 * c

# core.py:284-288 — bigram stupid backoff
total = float(tot[0][0])
rec = self.bigrams.get(f"{prev}\t{word}")
if rec:
    return math.log(rec[0][0] / total) - self.unigram_logp(word)
return math.log(0.4)

# core.py:339-344 — completion score, scan path
count * 10_000 + min(self._lex_score(word), 9_999),
word,
count,
len(rom) - len(r),

# build_lexicon.py:195 — completion score, build path (must match the line above)
s = count * 10_000 + min(lex.get(word, 0), 9_999)

# build_lexicon.py:270 — phonetic-key score
score = wiki.counts.get(k, 0) + 3 * subs.counts.get(k, 0) + 20 * chat.counts.get(k, 0)

# core.py:384 — gap for a key-prefix completion
g = max(1, len(key_from_bangla(word, level)) - len(k))
```

Note that `core.py:384` calls `key_from_bangla` **at runtime**, on a Bangla word, to measure the
gap. The Rust port therefore needs `key_from_bangla` available in the engine, not only in the
builder — the phonetic key is not stored per word in a directly retrievable way (it is embedded in
the `keys` trie key, but the code recomputes it rather than parsing it back out).

---

## 8. Ordering and tie-breaks

### 8.1 Where results are sorted

Three sorts in `candidates()`:

```python
completions.sort(reverse=True)          # core.py:346
exact.sort(reverse=True)                # core.py:374
longer.sort(reverse=True)               # core.py:375
```

These are **whole-tuple** descending sorts, not sorts on the score alone:

- `completions` holds `(score, word, count, gap)`. `reverse=True` therefore orders by
  **score descending, then word descending by Unicode code point, then count descending, then gap
  descending**. The word comparison is Python's `str` ordering = code-point-wise comparison of the
  `canonical` (NFC, joiners kept, nukta decomposed) form. A Rust port comparing `&str` with the
  default `Ord` gets byte-wise UTF-8 ordering, which for well-formed UTF-8 **is** code-point order —
  so `sort_by(|a, b| b.cmp(a))` on the tuple is correct, *provided the strings compared are the
  same canonical form Python compares*. Comparing `to_output` (precomposed nukta) forms would
  reorder ties differently.
- `exact` and `longer` hold `(score, word)` — same rule, score then word descending.

Because the comparison key is a full tuple, the result is a total order and the (unsorted) trie
iteration order **cannot** leak through these three sorts.

### 8.2 Where trie order DOES leak into the output

`feats` is a plain `dict` (`core.py:310`), so it is **insertion-ordered**. Words are inserted by
`f(word)` in this fixed sequence of stages:

1. exact romanisations, in raw `romans.items(r + "\t")` order — **unsorted trie order**
2. completions, in sorted order, capped at `max_per_channel` = 30
3. fine exact keys (sorted, cap 30), fine longer keys (sorted, cap 15)
4. coarse exact keys (sorted, cap 30), coarse longer keys (sorted, cap 15)
5. transliteration beam hypotheses, in beam-rank order
6. the Avro rule literal
7. personal-history words

Every final ranking is a **stable** `sorted(feats.items(), key=...)`
(`core.py:525`, `core.py:530-535`, `core.py:571-577`, `core.py:610-611`). Python's `sorted` is stable, so **candidates with exactly equal
scores come out in `feats` insertion order**, which for stage 1 is raw marisa trie order.

Consequence for the port: two candidates with bit-identical scores can swap places if the
replacement store enumerates exact-roman matches in a different order (e.g. lexicographic instead
of MARISA weight order). Exact float ties are not common but are not impossible — e.g. two words
with identical `(wiki, subs, chat)` triples, both attested once for the same roman, both matching
neither key. If bit-exact parity with the Python engine matters for the eval suite, the port needs
either the same enumeration order or an explicit deterministic tie-break added to *both*
implementations.

### 8.3 Caps applied after sorting

```python
completions[: self.max_per_channel]      # core.py:347   -> 30
exact[: self.max_per_channel]            # core.py:376   -> 30
longer[: self.max_per_channel // 2]      # core.py:380   -> 15  (integer division)
```

`max_per_channel` defaults to **30** (`core.py:172`, constructor keyword) and is not overridable by
an environment variable. `// 2` is floor division — 15, and would be 15 for 31 too.

### 8.4 Producer-side ordering

- `Counter.most_common(n)` (used at `build_lexicon.py:61, 248` and `build_bigrams.py:84`) sorts by
  count descending and breaks ties by **first-insertion order**, i.e. corpus order.
- `sorted(words.items(), key=lambda kv: -kv[1])[:24]` (`build_lexicon.py:207`) is stable, so ties
  at the cut are broken by `dict` insertion order.

These make the *artifacts* corpus-order-dependent. Rebuilding with a Rust producer will not
reproduce the shipped `.marisa` files bit-for-bit unless the same iteration orders are preserved.
See §11.

---

## 9. Short-input vs scan-path asymmetry

This is the highest-risk behaviour in the whole module. The same logical question ("what completes
this input?") is answered by two code paths that produce **different feature values**, not just
different orderings.

### 9.1 Roman completions

| | scan path (`len(r) > 3`, or no `prefixes`) | prefix-table path (`prefixes` loaded **and** `len(r) <= 3`) |
|---|---|---|
| source | `romans.items(r)` | `prefixes.items(f"r:{r}\t")` |
| exact-roman entries | **excluded** (`if rom == r: continue`) | **included** (builder emits `n == len(roman)` too) |
| `count` fed into `rom_prefix` | the real attestation count | hard-wired **1** |
| `gap` | `len(rom) - len(r)`, real character difference | hard-wired **2** |
| candidate pool | every matching entry | at most **24** |

Code (`core.py:327-345`):

```python
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
        completions.append(...)
```

Then (`core.py:347-352`):

```python
for _s, word, count, gap in completions[: self.max_per_channel]:
    ft = f(word)
    if not ft.rom_exact:
        ft.rom_prefix += count
        ft.gap = gap if not ft.gap else min(ft.gap, gap)
    ft.sources.add("rom+")
```

The `if not ft.rom_exact` guard means the exact-roman words that leak in via the prefix table do
**not** get `rom_prefix` or `gap` — they only get `sources.add("rom+")` and, crucially, they
**consume one of the 30 completion slots**. Verified for `r = "ki"`: 5 of the 24 prefix-table words
are also exact-roman words.

`ft.gap` uses `min` over contributions but treats `0` as "unset" (`gap if not ft.gap else min(...)`),
so a genuine gap of 0 can never be recorded. `gap` is applied at `core.py:497-498`:
`if ft.gap and not ft.rom_exact: s += w["gap"] * ft.gap` — and in the shipped `weights.json`
`"gap": 0.0`, so on the current weights the hard-wired `2` has **no effect**. It does have an effect
under `DEFAULT_WEIGHTS["gap"] = -0.4` (`core.py:63`), which is what runs if `weights.json` is absent.

### 9.2 Key completions

Both branches feed `exact` and `longer`, but the prefix-table branch's `longer` list contains
exact-key words too (the builder generates `pk[:n]` for `n` up to `min(2, len(pk))`, so `k` itself
when `len(pk) <= 2`). Those words get `key_fine`/`key_coarse` set from the `exact` list *and*
`key_fine_prefix`/`key_coarse_prefix` set from the `longer` list. In `score()` the
`if key_fine / elif key_coarse / elif key_*_prefix` chain (`core.py:486-491`) means the exact flag
wins, so the double-set is harmless for scoring — but the gap guard
`if not ft.rom_exact and not getattr(ft, attr)` (`core.py:383`) depends on it, and the word still
occupies a slot in the 15-item `longer` cap.

### 9.3 Threshold constants

| constant | value | defined | used |
|---|---|---|---|
| `SHORT_ROMAN_PREFIX` | 3 | `build_lexicon.py:169` | build only |
| roman path threshold | `len(r) <= 3` | `core.py:328` | runtime |
| `SHORT_KEY_PREFIX` | 2 | `build_lexicon.py:170` | build only |
| key path threshold | `len(k) <= 2` | `core.py:365` | runtime |
| `PREFIX_TOP` | 24 | `build_lexicon.py:171` | build only |

The build-time and runtime thresholds are **duplicated literals**, not shared constants. They must
stay in lock-step: if the runtime threshold exceeds the build-time one, the prefix lookup silently
returns nothing and short inputs lose all completions. Verified: `prefixes.items("r:bangla\t")`
returns 0 rows, because `"bangla"` is 6 characters and was never given a prefix-table row.

---

## 10. Non-trie files in the same directory

For completeness — these are loaded from `lexicon_dir` but are not tries.

- `weights.json` (378 bytes) — `core.py:212-214`:

  ```python
  tuned = Path(os.environ.get("LIKHI_WEIGHTS") or (lexicon_dir / "weights.json"))
  if tuned.exists():  # written by likhi-tune
      self.w.update(json.loads(tuned.read_text(encoding="utf-8")))
  ```

  It **overrides** `DEFAULT_WEIGHTS` key by key. The shipped file differs from the defaults in:
  `unigram` 1.0 → **0.5**, `key_fine` 1.2 → **2.0**, `key_coarse` 0.6 → **1.2**,
  `key_prefix` -1.0 → **0.0**, `gap` -0.4 → **0.0**, `xlit_top1` 1.5 → **1.0**,
  `xlit_top3` 0.7 → **1.0**, `avro` 0.8 → **0.0**, `oov` -3.0 → **-6.0**.
  `rom_exact_fast` is present in both at 2.5. A port that hard-codes `DEFAULT_WEIGHTS` will rank
  differently from the shipped engine.

- `meta.json` (535 bytes) and `bigrams_meta.json` (266 bytes) — provenance only; nothing reads them
  at runtime. `meta.json`'s own `sizes_bytes["meta.json"]` is stale (512 vs the actual 535) because
  `out.iterdir()` is stat'ed before the file is rewritten (`build_lexicon.py:291-293`).

---

## 11. Uncertain

Flagged honestly; a confident guess here would be worse than the flag.

1. **Bit-exact reproduction of `prefixes.marisa` by a Rust producer.** The top-24 cut uses a stable
   sort on the negated score only (`build_lexicon.py:207`), so entries tied at the boundary are kept
   or dropped according to Python `dict` insertion order, which derives from the iteration order of
   `romans` (a `defaultdict` filled by `collect_romans`) and of the `keys` `set`. **A Python `set`
   of `str` has hash-randomised iteration order unless `PYTHONHASHSEED` is fixed**, so I am not
   confident the current Python builder is even reproducible against itself across runs. I did not
   test this (a rebuild is a ~91 s job plus the raw corpora). If byte-identical rebuilds matter, this
   needs to be measured and probably fixed with an explicit deterministic tie-break in both
   implementations.

2. **How many score ties actually occur in practice**, and therefore how much §8.2 matters in
   observable output. I proved the mechanism from the code (plain `dict` + stable `sorted`) but did
   not measure a real ranking where two candidates tie to the last float bit. Worth an experiment
   before deciding whether the port needs to replicate MARISA enumeration order.

3. **Whether `min(c, 2**31 - 1)` into a `<I` (unsigned) field is intentional or an off-by-one.**
   The ceiling is 2,147,483,647, not 4,294,967,295. No current value is near it, so I could not
   distinguish intent from oversight. I have specified "keep the clamp as written".

4. **The exact MARISA value separator byte.** `marisa_trie` 1.4.1 exposes `BytesTrie` as a compiled
   extension; `inspect.getsource` fails and `_separator` is not a readable attribute. I verified
   *behaviourally* that payloads containing `0xFF` and `0x00` round-trip correctly, so whatever the
   separator is, it is handled safely. Since the port replaces the container, this should not matter
   — but if anyone needs to read the legacy files from Rust, the separator must be recovered from
   the marisa-trie source rather than from this document.

5. **Whether the 89 all-digit words with no phonetic key, and the resulting `lex.get(word, 0) == 0`
   divergence in `build_prefix_tables` (§3.4 note 1), ever change a visible suggestion.** The
   divergence is real and provable from the code; its impact is probably nil but I did not confirm
   that.

6. **`romankey.key_from_roman` / `key_from_bangla` semantics** are summarised here only where the
   lexicon touches them (the key alphabet, the `f:`/`c:` level char, the runtime call at
   `core.py:384`). Their full transliteration tables, the `_finalize` vowel/semivowel rules and the
   `lru_cache(maxsize=65536)` on both are out of scope for this document and need their own spec
   before the port — the key alphabet observed in `keys.marisa` (`abcdefghijklmnoprstu`) is a
   *consequence* of those tables, not an independent specification of them.

7. **Whether `bigram_totals.marisa` being loaded without an existence check (§3.5) is ever hit in
   practice.** The two files are always written together by `build_bigrams.build()`, so a
   half-present pair only arises from a partial copy. I have documented it rather than assumed it is
   safe.
