# Porting specification — `avro` (avro.py) phonetic rule engine → Rust

**Status:** complete algorithmic spec for `avro.parse()`, the function Likhi uses as channel 4.
**Verified against:** `avro-py 2025.11.3`, Python 3.12, on 2026-09-18, by executing the real package
with `C:\Users\khaled\src\likhi\.venv\Scripts\python.exe`. Every table, constant and golden vector in
this document was dumped from the live objects, not transcribed by eye.

---

## 1. Provenance, files and sizes

Two identical copies of the package exist in the tree. SHA-256 of every `.py` file is byte-identical
between them (verified with `Get-FileHash`), so there is exactly one rule set to port.

| File | Bytes | Role |
|---|---|---|
| `C:\Users\khaled\src\likhi\.venv\Lib\site-packages\avro\__init__.py` | 938 | public re-exports only |
| `…\avro\main.py` | 19,556 | `parse()` / `reverse()` / bijoy drivers, the parse output generator |
| `…\avro\core\__init__.py` | 177 | docstring only |
| `…\avro\core\config.py` | 994 | pulls the character-class sets out of `DICT` |
| `…\avro\core\count.py` | 818 | `count_vowels` / `count_consonants` — **unused by parse**, do not port |
| `…\avro\core\processor.py` | 14,254 | pattern matching, rule evaluation, remap, bijoy rearrangers |
| `…\avro\core\validate.py` | 3,071 | vowel/consonant/punctuation/exact predicates, case fixing |
| `…\avro\resources\__init__.py` | 215 | `from .dictionary import *` |
| **`…\avro\resources\dictionary.py`** | **53,475** | **THE RULE TABLE — the only data file** |

Second copy (identical): `C:\Users\khaled\src\likhi\dist\runtime\python\Lib\site-packages\avro\…`

Package metadata: `avro_py-2025.11.3.dist-info`, author Anindya Shiddhartha,
**License: MIT OR Apache-2.0** — dual-licensed, so the tables may be copied into Likhi (MIT) verbatim.
Keep the SPDX header and the `NOTICE` file contents when you vendor the data.

### 1.1 Where the rules live — answer to "is it a JSON data file?"

**No. There is no JSON, no TOML, no CSV, no binary resource.** `RECORD` in the dist-info lists only
`.py` files. The entire rule set is a Python literal assigned at
`resources/dictionary.py:53`:

```python
# The Avro Dictionary, implemented in Python.
DICT: MainDict = {
    "avro": {
        "patterns": [
            {"find": "bhl", "replace": "ভ্ল"},
```

**Consequence for the Rust port:** nothing can be "shipped as-is and parsed at load time" — there is no
data file to ship. You must *extract* it. The literal is a pure JSON-compatible subtree (only `dict`,
`list`, `str`; no tuples, no `None` values anywhere inside `patterns`, verified), so the conversion is
mechanical and lossless:

```python
# one-shot extraction, run with the venv python
import json, io
from avro.resources import DICT
io.open("avro_rules.json", "w", encoding="utf-8").write(
    json.dumps(DICT["avro"], ensure_ascii=False, indent=1)
)
```

Recommended shape for the Rust side (both are fine; pick one and be consistent):

* **Compile-time**: `include_str!("avro_rules.json")` + `serde_json` in a `OnceLock`, or
* **Build-time**: a `build.rs` / codegen step that emits `static PATTERNS: &[Pattern]` so there is zero
  parse cost and zero allocation at startup. Preferred given Likhi's per-keystroke budget.

Either way the *ordering of the `patterns` array is load-bearing* (see §5) — serialise it as an array,
never a map, and never sort it.

### 1.2 What Likhi actually calls

`C:\Users\khaled\src\likhi\src\likhi\engine\core.py:229-231` and `:405-413`:

```python
import avro
self._avro = avro.parse
...
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
```

So only `parse(text, bijoy=False, remap_words=True)` is on the hot path. **`remap_words` defaults to
`True`** — the exception-word remap in §4.2 is ON in production. `to_bijoy`, `to_unicode`, `reverse`
and all `_iter` / `_async` variants are **not used by Likhi** and should not be ported (§10).

---

## 2. Data model

`resources/dictionary.py:8-49`:

```python
class PatternRuleMatch(TypedDict, total=False):
    type: str
    scope: str
    value: str | None

class PatternRule(TypedDict):
    matches: list[PatternRuleMatch]
    replace: str

class PatternDict(TypedDict, total=False):
    find: str
    replace: str
    reverse: str | None
    rules: list[PatternRule] | None
```

Suggested Rust mirror:

```rust
enum Scope { Punctuation, Vowel, Consonant, Exact(&'static str) }
struct Match { is_prefix: bool, negated: bool, scope: Scope }  // type=="prefix" => is_prefix
struct Rule  { matches: &'static [Match], replace: &'static str }
struct Pattern {
    find: Option<&'static str>,      // 6 entries genuinely have no `find` — see §5.4
    replace: &'static str,
    reverse: Option<&'static str>,   // reverse-transliteration only; unused by parse
    rules: Option<&'static [Rule]>,  // KEY PRESENCE, not emptiness, splits the two passes
}
```

**Counts (verified):** 293 patterns total — **273 without a `rules` key**, **20 with one**.
No pattern has `"rules": None`. No pattern is missing `replace`. 220 patterns carry a `reverse` key;
none of those has a `None` value. `find` lengths: 1×53, 2×132, 3×85, 4×15, 5×2 (max 5, min 1).
One exact duplicate: `{"find":"Sc","replace":"শ্চ","reverse":"scch"}` appears twice, at list indices
224 (`dictionary.py:471`) and 233 (`dictionary.py:480`). Harmless — the first always wins.

---

## 3. Character-class constants

From `resources/dictionary.py:1052-1061`, surfaced by `core/config.py:8-18`.
All eight are **exact string/array literals** — reproduce them character for character.

| Name | `config.py` line | Type in Python | Value |
|---|---|---|---|
| `AVRO_VOWELS` | 8 | `set(str)` | `"aeiou"` |
| `AVRO_CONSONANTS` | 9 | `set(str)` | `"bcdfghjklmnpqrstvwxyz"` |
| `AVRO_CASESENSITIVES` | 10 | `set(str)` | `"oiudgjnrstyz"` |
| `AVRO_NUMBERS` | 11 | `set(str)` | `"0123456789"` (unused by parse) |
| `AVRO_SHORBORNO` | 14 | `set(str)` | `"অআইঈউঊএঐওঔ"` (reverse only) |
| `AVRO_SHONGKHA` | 15 | `set(str)` | `"০১২৩৪৫৬৭৮৯"` (reverse only) |
| `AVRO_KAR` | 16 | `set(list)` | `["া","ি","ী","ৗ","ু","ূ","ৃ","ে","ৈ","ো","ৌ"]` (reverse/bijoy only) |
| `AVRO_IGNORE` | 17 | `set(list)` | `["ঁ","।","?",".","-",";"]` (reverse only) |

Note `AVRO_VOWELS ∪ AVRO_CONSONANTS` = all 26 ASCII letters, exactly once each. That fact drives §3.1.

Bijoy-only constants (`config.py:21-28`, `dictionary.py:1065-1294`) — 219 mappings,
`prekar = ["ি","ৈ","ে"]`, `postkar = ["া","ো","ৌ","ৗ","ু","ূ","ী","ৃ"]`,
`banjonborno = "কখগঘঙচছজঝঞটঠডঢণতথদধনপফবভমশষসহযরলয়ংঃঁৎ"`,
`exceptions = {"halant": "্", "nukta": ["ং","ঃ","ঁ"]}`. **Not needed for `parse`.**

### 3.1 The three predicates — reproduce these formulas exactly

`core/validate.py:11-46`:

```python
@lru_cache(maxsize=256)
def is_vowel(text: str) -> bool:
    return text.lower() in config.AVRO_VOWELS

@lru_cache(maxsize=256)
def is_consonant(text: str) -> bool:
    return text.lower() in config.AVRO_CONSONANTS

@lru_cache(maxsize=256)
def is_punctuation(text: str) -> bool:
    return not (
        text.lower() in config.AVRO_VOWELS
        or text.lower() in config.AVRO_CONSONANTS
    )
```

* All three **lowercase before testing**. `is_vowel('A') == True`, `is_consonant('K') == True`.
* `is_punctuation` is therefore **"is not an ASCII letter"**. Digits, `` ` ``, `.`, `-`, whitespace,
  `<`, `>`, `/`, and *every non-ASCII character including all Bangla* are punctuation.
  Rust: `!c.to_lowercase().next().is_some_and(|c| VOWELS.contains(c) || CONSONANTS.contains(c))`, or
  simply `!c.is_ascii_alphabetic()` — **equivalent for every possible input** because the two sets
  together are exactly `[a-z]` and `.lower()` maps `[A-Z]`→`[a-z]`. (Careful: `'İ'.lower()` in Python
  is the two-char string `"i̇"`, which is not in the sets, so `is_punctuation('İ') == True`; plain
  `is_ascii_alphabetic` agrees. Rust's `char::to_lowercase` on `'K'` U+212A yields `'k'` which would
  *disagree*; use `is_ascii_alphabetic`, not a lowercase-then-contains port.)

`core/validate.py:58-67` — the exact-substring predicate. **Note `end < len`, not `end <= len`:**

```python
def is_exact(
    needle: str, haystack: str, start: int, end: int, matchnot: bool
) -> bool:
    return (
        start >= 0 and end < len(haystack) and haystack[start:end] == needle
    ) != matchnot
```

Reproduce `end < len(haystack)` literally. It is an off-by-one, but it is *the* behaviour. See §7.3.

### 3.2 Case folding — `fix_string_case`

`core/validate.py:49-81`:

```python
@lru_cache(maxsize=256)
def is_case_sensitive(text: str) -> bool:
    return text.lower() in config.AVRO_CASESENSITIVES

def fix_string_case(text: str) -> str:
    fixed = [i if is_case_sensitive(i) else i.lower() for i in text]
    return "".join(fixed)
```

Per character: if `c.lower() ∈ {o,i,u,d,g,j,n,r,s,t,y,z}` keep `c` as typed; otherwise emit `c.lower()`.
So `O I U D G J N R S T Y Z` survive as uppercase; **every other uppercase letter is folded**, notably
`A`, `B`, `C`, `E`, `F`, `H`, `K`, `L`, `M`, `P`, `Q`, `V`, `W`, `X`.

Verified: `fix_string_case("BANGLADESH") == "baNGlaDeSh"`, `fix_string_case("KhOJ") == "khOJ"`,
`fix_string_case("Ami BhaLo") == "ami bhalo"`.

---

## 4. The `parse()` pipeline

`main.py:464-487` → `main.py:225-256`:

```python
@lru_cache(maxsize=128)
def _parse_backend(text: str, remap_words: bool) -> str:
    fixed_text = validate.fix_string_case(text)

    if remap_words:
        fixed_text, manual_required = processor.find_in_remap(fixed_text)
        return _process_remapped(
            fixed_text,
            manual_required,
            lambda segment: "".join(
                chain.from_iterable(_parse_output_generator(segment, 0))
            ),
        )
    else:
        return "".join(
            chain.from_iterable(_parse_output_generator(fixed_text, 0))
        )
```

Order of operations for `parse(roman)` as Likhi calls it:

1. `fix_string_case` over the whole input (§3.2).
2. `find_in_remap` — exception-word substitution, wrapping hits in `<rm>…</rm>` (§4.2).
3. Split on those markers; emit marked spans verbatim (markers stripped); run the transliterator over
   each unmarked span **independently, with the cursor restarting at 0** (§4.3).
4. Concatenate.

`chain.from_iterable(...)` over a generator of strings flattens to characters and `"".join` puts them
back — semantically identical to `"".join(generator)`. No reordering.

`_parse_backend` is `lru_cache(maxsize=128)` keyed on `(text, remap_words)`; `find_in_remap` is
`lru_cache(maxsize=128)` on `(text, reversed)`. Caching is an implementation detail with no observable
effect (the function is pure). Port it or not as you like.

### 4.1 Empty input

`parse("") == ""`. Verified.

### 4.2 `find_in_remap` — the exception-word pass

`core/processor.py:40-73`:

```python
@lru_cache(maxsize=128)
def find_in_remap(text: str, *, reversed: bool = False) -> tuple[str, bool]:
    for key, value in AVRO_EXCEPTIONS.items():
        if reversed:
            pattern = re.compile(re.escape(key), re.IGNORECASE)
            text = pattern.sub(lambda m: "<rm>" + value + "</rm>", text)
        else:
            pattern = re.compile(re.escape(value.lower()), re.IGNORECASE)
            text = pattern.sub(lambda m: "<rm>" + key + "</rm>", text)

    segments = re.split(r"(<rm>.*?</rm>)", text)
    manual_required = any(
        segment
        and not (segment.startswith("<rm>") and segment.endswith("</rm>"))
        for segment in segments
    )
    return text, manual_required
```

Forward (`reversed=False`, the Likhi path): `AVRO_EXCEPTIONS` is `{bangla_key: roman_value}`
(e.g. `"বাংলাদেশ": "Bangladesh"`). For each entry **in dictionary insertion order**, every
case-insensitive occurrence of the roman value *anywhere in the string* is replaced by
`"<rm>" + bangla_key + "</rm>"`.

Critical properties:

* **Iteration order is the source literal order** (Python dicts preserve insertion order). A later
  entry operates on the text already rewritten by earlier ones. **Use an ordered `Vec<(&str,&str)>`
  in Rust, never a `HashMap`.** Four real collisions exist (§9.3).
* **Substring, not word-boundary.** There is no `\b`. `"busy"` → `"<rm>বাস</rm>y"`.
* All 183 roman values are pure ASCII alphanumeric, so `re.escape` is a no-op (verified) and the
  patterns contain no regex metacharacters — a plain case-insensitive substring search is equivalent.
* `.lower()` is applied to the value before compiling, then `re.IGNORECASE` is applied anyway, so the
  value's own casing is irrelevant. Rust: ASCII-case-insensitive `find` in a loop, replacing all
  occurrences left-to-right, restarting the scan **after** the inserted replacement (Python's
  `re.sub` does not rescan inserted text).
* `manual_required` is `True` iff at least one non-marker segment survives (i.e. some of the input was
  *not* covered by an exception).

### 4.3 `_process_remapped`

`main.py:117-152`:

```python
def _process_remapped(
    text: str, manual_required: bool, process_func: Callable[[str], str]
) -> str:
    if not manual_required:
        return text.replace("<rm>", "").replace("</rm>", "")

    segments: list[str] = re.split(r"(<rm>.*?</rm>)", text)
    processed_segments: list[str] = []

    for segment in segments:
        if segment.startswith("<rm>") and segment.endswith("</rm>"):
            processed_segments.append(segment[4:-5])
        elif segment:
            processed_segments.append(process_func(segment))

    return "".join(processed_segments)
```

`segment[4:-5]` strips exactly `len("<rm>")==4` from the front and `len("</rm>")==5` from the back.
`re.split` with a capturing group keeps the delimiters, and the split is **non-greedy** (`.*?`) and
**does not match across a newline** (no `re.DOTALL`).

**The gotcha that changes output:** each unmarked segment is transliterated on its own, starting at
cursor 0. Prefix context is destroyed at marker boundaries — the character after a `</rm>` sees
"start of string", which the rule engine treats as `punctuation` (§6.2). That is why:

```
parse("bangladeshi")  == "বাংলাদেশই"    (trailing "i" alone → prefix is start-of-string → ই)
parse("bangladeshi", remap_words=False) == "বাংলাদেশি"   (ি, because prefix "sh" is a consonant)
```

### 4.4 `_parse_output_generator` — the main loop

`main.py:156-221`, reproduced in full because every branch matters:

```python
def _parse_output_generator(
    fixed_text: str, cur_end: int
) -> Generator[str, None, None]:
    for cur, i in enumerate(fixed_text):
        # Optimized UTF-8 check - most ASCII chars are in range 0-127
        uni_pass = ord(i) < 128
        if not uni_pass:
            cur_end = cur + 1
            yield i
        elif cur >= cur_end and uni_pass:
            match = processor.match_patterns(fixed_text, cur, rule=False)
            matched = bool(match.get("matched"))
            replaced = match.get("replaced")
            found = match.get("found")

            if (
                matched
                and isinstance(replaced, str)
                and isinstance(found, str)
            ):
                yield replaced
                cur_end = cur + len(found)
            else:
                match_rule = processor.match_patterns(
                    fixed_text, cur, rule=True
                )
                matched_rule = bool(match_rule.get("matched"))
                found_rule = match_rule.get("found")
                rules = match_rule.get("rules")
                replaced_rule = match_rule.get("replaced")

                if (
                    matched_rule
                    and isinstance(found_rule, str)
                    and isinstance(rules, list)
                ):
                    cur_end = cur + len(found_rule)
                    replaced_val = processor.process_rules(
                        rules=rules,
                        fixed_text=fixed_text,
                        cur=cur,
                        cur_end=cur_end,
                    )
                    if isinstance(replaced_val, str) and replaced_val:
                        yield replaced_val
                    elif isinstance(replaced_rule, str):
                        yield replaced_rule

                else:
                    cur_end = cur + 1
                    yield i
```

Restated as the algorithm to implement, per position `cur` from `0..len`:

1. If `fixed_text[cur]` is non-ASCII (`ord >= 128`): set `cur_end = cur + 1`, emit the character
   unchanged, continue. **This branch runs even when `cur < cur_end`** — it ignores the skip window.
   (Unreachable in practice: every `find` is ASCII, so a consumed window never covers a non-ASCII
   char. Reproduce it anyway.)
2. Else if `cur < cur_end`: emit nothing (the character was consumed by an earlier match).
3. Else: **pass A** — longest match among the 273 non-rule patterns at `cur` (§5). On a hit, emit
   `pattern.replace`, set `cur_end = cur + pattern.find.len()`.
4. Else **pass B** — longest match among the 20 rule patterns at `cur`. On a hit, set
   `cur_end = cur + pattern.find.len()` **first**, then evaluate `process_rules` (§6) with that
   `cur_end`. If it returns a **non-empty** string, emit it; otherwise emit the pattern's own
   top-level `replace`.
5. Else: `cur_end = cur + 1`, emit the raw character.

**`cur_end` is an exclusive index just past the matched roman span.** It is what `suffix` matches test.

**Empty-string subtlety:** `if isinstance(replaced_val, str) and replaced_val` — a rule that matched
but whose `replace` is `""` falls through to the pattern's top-level `replace`. No rule in the current
data has an empty `replace` (verified), so this is latent, but reproduce it. Conversely, the pattern
`{"find":"o", "replace":"", …}` (index 244) has an **empty top-level replace**, which is how a silent
inherent-vowel `o` after a consonant is deleted: `parse("bl")`-style words rely on it.

---

## 5. Pattern matching

`core/processor.py:26-28`:

```python
PATTERNS = DICT["avro"]["patterns"]
NON_RULE_PATTERNS = [p for p in PATTERNS if "rules" not in p]
RULE_PATTERNS = [p for p in PATTERNS if "rules" in p]
```

The split is on **key presence**, and both sub-lists preserve the original relative order.

`core/processor.py:76-131` and `:134-184`:

```python
    rule_type = NON_RULE_PATTERNS if not rule else RULE_PATTERNS
    pattern = exact_find_in_pattern(fixed_text, reversed, cur, rule_type)

    if pattern:
        p = pattern[0]
        return {
            "matched": True,
            "found": p.get("find"),
            "replaced": p.get("replace"),
            ...
        }
```

```python
    return [
        x
        for x in patterns
        if (
            "find" in x
            and x.get("find") is not None
            and (cur + len(x["find"]) <= len(fixed_text))
            and x["find"] == fixed_text[cur : (cur + len(x["find"]))]
        )
    ]
```

So: collect every pattern whose `find` occurs at `cur` and fits inside the string, then **take the
first one in list order**. Not the longest — *the first*.

### 5.1 Why "first in list order" == "longest match" here (proved, not assumed)

I checked every ordered pair in both sub-lists for the shadowing hazard (a shorter `find` appearing
earlier than a longer `find` that it prefixes):

```
ordering violations within NON_RULE_PATTERNS: 0
ordering violations within RULE_PATTERNS:     0
```

So within each pass, first-match is exactly longest-match. A Rust trie / longest-match matcher is a
faithful port **as long as you keep the two passes separate**. If you ever regenerate the table from a
newer avro.py release, re-run that check before trusting a trie.

### 5.2 Why the two passes cannot be merged into one longest-match

I also checked the cross-list direction:

```
non-rule `find` that is a proper prefix of a rule `find`: NONE
```

Because no non-rule `find` is a proper prefix of any rule `find`, and no `find` string appears in both
lists (verified: the intersection is empty), a shorter non-rule match can never pre-empt a longer rule
match. **In the current table, the two passes are therefore equivalent to a single global
longest-match over all 293 patterns.** That equivalence is a property of this data, not of the
algorithm. Safest port: implement the two passes literally (two tries, or one trie with a
`has_rules` flag and two lookups). Cheapest correct port: one trie, with a regression test asserting
the cross-prefix property.

The opposite direction *does* occur, and is the designed backtick-escape mechanism — a longer non-rule
pattern deliberately shadows a shorter rule pattern:

```
rule 'OI'  vs longer non-rule 'OI`'      rule 'a'  vs longer non-rule 'aZ', 'a`'
rule 'OU'  vs longer non-rule 'OU`'      rule 'i'  vs longer non-rule 'i`'
rule 'O'   vs longer non-rule 'OI`','OU`','O`'    rule 'I' vs longer non-rule 'I`'
rule 'rri' vs longer non-rule 'rri`'     rule 'u'  vs longer non-rule 'u`'
rule 'rr'  vs longer non-rule 'rri`','rrZ','rry'  rule 'U' vs longer non-rule 'U`'
rule 'r'   vs longer non-rule 'rri`','rrZ','rry'  rule 'ee' vs longer non-rule 'ee`'
rule 'oo'  vs longer non-rule 'oo`'      rule 'e'  vs longer non-rule 'ee`','e`'
rule 'o'   vs longer non-rule 'oo`','oZ'
```

### 5.3 Indexing is by Unicode scalar, not byte

`fixed_text[cur]`, `len(fixed_text)` and `fixed_text[cur:cur+n]` are **code-point** operations in
Python. Likhi feeds ASCII romanisation, but `parse()` accepts anything and §4.4 step 1 exists precisely
for non-ASCII input. In Rust, operate on `Vec<char>` (or `&[char]`), **not on `&str` byte offsets**, or
a single Bangla character in the input will desynchronise every cursor comparison.

### 5.4 Six patterns have no `find` at all

Indices 260, 261, 264, 269, 276, 277 (`dictionary.py:568, 569, 616, 676, 767, 768`):

```
{"replace":"আ","reverse":"a"}   {"replace":"র","reverse":"r"}   {"replace":"ই","reverse":"i"}
{"replace":"উ","reverse":"u"}   {"replace":"এ","reverse":"e"}   {"replace":"ও","reverse":"w"}
```

The forward matcher's `"find" in x` guard skips them, so they are **inert for `parse()`**. They exist
for the reverse matcher, which keys on `replace`. Keep `find` as `Option<&str>` and skip `None` in the
forward path — do not "fix" them by inventing a `find`.

### 5.5 Two patterns are dead code

`{"find":"AZ","replace":"অ্যা"}` (index 257, `dictionary.py:565`) and
`{"find":"A`","replace":"া","reverse":"a"}` (index 259, `dictionary.py:567`) can never match, because
`A` is not in `AVRO_CASESENSITIVES` and so `fix_string_case` has already turned it into `a` before the
matcher runs (verified: these are the only two such patterns). Port them for fidelity; they cost
nothing and guard against a future change to `casesensitive`.

---

## 6. Rule evaluation

### 6.1 `process_rules` — first satisfied rule wins

`core/processor.py:231-277`:

```python
    replaced = ""
    matched = False

    # Iterate through rules.
    for rule in rules:
        matched = False

        for match in rule["matches"]:
            matched = process_match(match, fixed_text, cur, cur_end)

            if not matched:
                break

        if matched:
            replaced = rule["replace"]
            break

    return replaced if matched else None
```

* Rules are tried **in list order**; the first rule whose **every** match passes wins. Short-circuit on
  the first failing match.
* `matched` is reset to `False` at the top of each rule, so **a rule with an empty `matches` list
  cannot win** (the inner loop never runs and `matched` stays `False`). No such rule exists today
  (verified) but reproduce the semantics — an `all()`-style port would make an empty list succeed.
* Returns `None` when no rule fired; the caller then falls back to the pattern's own `replace`.

### 6.2 `process_match` — the four scopes

`core/processor.py:280-384`. Reproduce these four boolean expressions *literally*; the double negation
via `!= negative` is easy to get subtly wrong.

```python
    # Initial/default value for replace.
    replace = True
    ...
    # Set check cursor depending on match['type']
    chk = cur - 1 if match_type == "prefix" else cur_end

    # Set scope based on whether scope is negative.
    if match_scope.startswith("!"):
        scope = match_scope[1:]
        negative = True
    else:
        scope = match_scope
        negative = False
```

**`chk = cur - 1` for `prefix`, `chk = cur_end` for `suffix`.** Any `type` that is not the literal
string `"prefix"` is treated as suffix. Missing `type` or `scope` ⇒ the match fails
(`if match_type is None or match_scope is None: return False`, lines 316-317).

```python
    if scope == "punctuation":
        if (
            not (
                (chk < 0 and match_type == "prefix")
                or (chk >= len(fixed_text) and match_type == "suffix")
                or validate.is_punctuation(fixed_text[chk])
            )
            != negative
        ):
            replace = False
```

**Out-of-bounds counts as punctuation.** Start-of-string satisfies `prefix punctuation`;
end-of-string satisfies `suffix punctuation`. This is the word-boundary mechanism.

```python
    elif scope == "vowel":
        if (
            not (
                (
                    (chk >= 0 and match_type == "prefix")
                    or (chk < len(fixed_text) and match_type == "suffix")
                )
                and validate.is_vowel(fixed_text[chk])
            )
            != negative
        ):
            replace = False

    elif scope == "consonant":
        if (
            not (
                (
                    (chk >= 0 and match_type == "prefix")
                    or (chk < len(fixed_text) and match_type == "suffix")
                )
                and validate.is_consonant(fixed_text[chk])
            )
            != negative
        ):
            replace = False
```

**Out-of-bounds counts as NOT a vowel and NOT a consonant** — the opposite convention from
`punctuation`. Therefore start-of-string satisfies both `prefix punctuation` **and**
`prefix !consonant`, which is why so many rules list those two as alternative rules for the same
replacement. Rust:

```rust
// vowel / consonant
let inner = in_bounds && pred(text[chk]);
let ok = (!inner) != negative;      // ok == false  =>  this match fails
// punctuation
let inner = oob_for_this_type || is_punctuation(text[chk]);
let ok = (!inner) != negative;
```

Equivalently and more readably: `ok == (inner == !negative)` — a positive scope passes when `inner`
holds, a negated scope passes when it does not. Both forms are identical; use whichever you will not
mistype.

```python
    elif scope == "exact":
        # Defensive: match_value must be a string
        if not isinstance(match_value, str):
            return False
        if match_type == "prefix":
            exact_start = cur - len(match_value)
            exact_end = cur
        else:
            exact_start = cur_end
            exact_end = cur_end + len(match_value)

        if not validate.is_exact(
            match_value, fixed_text, exact_start, exact_end, negative
        ):
            replace = False
```

With `is_exact` from §3.1. Note that an `exact` window is placed **immediately adjacent** to the
matched span: `[cur - len(value), cur)` for prefix, `[cur_end, cur_end + len(value))` for suffix.
`chk` is not used for `exact`.

**Any scope string other than `punctuation` / `vowel` / `consonant` / `exact` leaves `replace = True`
— i.e. an unknown scope silently passes.** Only these four occur in the data.

### 6.3 Complete inventory of match conditions in the data

Every `(type, scope, value)` triple used by any rule, with occurrence counts (verified):

| type | scope | value | count |
|---|---|---|---|
| prefix | `!consonant` | — | 14 |
| prefix | `consonant` | — | 5 |
| prefix | `punctuation` | — | 16 |
| prefix | `!punctuation` | — | 1 |
| prefix | `vowel` | — | 2 |
| prefix | `exact` | `a` | 1 |
| prefix | `exact` | `o` | 1 |
| prefix | `!exact` | `Z` | 1 |
| prefix | `!exact` | `a` | 1 |
| prefix | `!exact` | `o` | 1 |
| prefix | `!exact` | `r` | 4 |
| prefix | `!exact` | `w` | 3 |
| prefix | `!exact` | `x` | 3 |
| prefix | `!exact` | `y` | 3 |
| suffix | `vowel` | — | 1 |
| suffix | `!vowel` | — | 1 |
| suffix | `!punctuation` | — | 1 |
| suffix | `!exact` | `` ` `` | 17 |
| suffix | `!exact` | `r` | 1 |

There is **no positive `suffix exact`** anywhere, and **no `suffix consonant`**.

---

## 7. The complete rule table

### 7.1 All 20 rule patterns (list index, `dictionary.py` lines)

Read `rule0`, `rule1`, … in order; first fully-satisfied rule wins; if none, use the pattern's own
`replace`. `"<absent>"` means the key is not present (irrelevant to `parse`).

```
[178] dictionary.py:234-250  {"find":"OI","replace":"ৈ","reverse":"oi"}
      rule0: if [prefix !consonant] -> "ঐ"
      rule1: if [prefix punctuation] -> "ঐ"
[179] dictionary.py:251-267  {"find":"OU","replace":"ৌ","reverse":"ou"}
      rule0: if [prefix !consonant] -> "ঔ"
      rule1: if [prefix punctuation] -> "ঔ"
[180] dictionary.py:268-284  {"find":"O","replace":"ো","reverse":"o"}
      rule0: if [prefix !consonant] -> "ও"
      rule1: if [prefix punctuation] -> "ও"
[193] dictionary.py:297-313  {"find":"rri","replace":"ৃ","reverse":"ri"}
      rule0: if [prefix !consonant] -> "ঋ"
      rule1: if [prefix punctuation] -> "ঋ"
[196] dictionary.py:316-347  {"find":"rZ","replace":"র‍্য","reverse":"<absent>"}
      rule0: if [prefix consonant AND prefix !exact 'r' AND prefix !exact 'y' AND prefix !exact 'w' AND prefix !exact 'x'] -> "্র্য"
[197] dictionary.py:348-379  {"find":"ry","replace":"র‍্য","reverse":"<absent>"}
      rule0: if [prefix consonant AND prefix !exact 'r' AND prefix !exact 'y' AND prefix !exact 'w' AND prefix !exact 'x'] -> "্র্য"
[198] dictionary.py:380-409  {"find":"rr","replace":"রর","reverse":"<absent>"}
      rule0: if [prefix !consonant AND suffix !vowel AND suffix !exact 'r' AND suffix !punctuation] -> "র্"
      rule1: if [prefix consonant AND prefix !exact 'r'] -> "্রর"
[202] dictionary.py:413-449  {"find":"r","replace":"র","reverse":"<absent>"}
      rule0: if [prefix consonant AND prefix !exact 'r' AND prefix !exact 'y' AND prefix !exact 'w' AND prefix !exact 'x' AND prefix !exact 'Z'] -> "্র"
[242] dictionary.py:489-517  {"find":"oo","replace":"ু","reverse":"u"}
      rule0: if [prefix !consonant AND suffix !exact '`'] -> "উ"
      rule1: if [prefix punctuation AND suffix !exact '`'] -> "উ"
[244] dictionary.py:523-552  {"find":"o","replace":"","reverse":"<absent>"}
      rule0: if [prefix vowel AND prefix !exact 'o'] -> "ও"
      rule1: if [prefix vowel AND prefix exact 'o'] -> "অ"
      rule2: if [prefix punctuation] -> "অ"
[262] dictionary.py:570-614  {"find":"a","replace":"া","reverse":"a"}
      rule0: if [prefix punctuation AND suffix !exact '`'] -> "আ"
      rule1: if [prefix !consonant AND prefix !exact 'a' AND suffix !exact '`'] -> "য়া"
      rule2: if [prefix exact 'a' AND suffix !exact '`'] -> "আ"
[265] dictionary.py:617-644  {"find":"i","replace":"ি","reverse":"<absent>"}
      rule0: if [prefix !consonant AND suffix !exact '`'] -> "ই"
      rule1: if [prefix punctuation AND suffix !exact '`'] -> "ই"
[267] dictionary.py:646-674  {"find":"I","replace":"ী","reverse":"i"}
      rule0: if [prefix !consonant AND suffix !exact '`'] -> "ঈ"
      rule1: if [prefix punctuation AND suffix !exact '`'] -> "ঈ"
[270] dictionary.py:677-705  {"find":"u","replace":"ু","reverse":"u"}
      rule0: if [prefix !consonant AND suffix !exact '`'] -> "উ"
      rule1: if [prefix punctuation AND suffix !exact '`'] -> "উ"
[272] dictionary.py:707-735  {"find":"U","replace":"ূ","reverse":"u"}
      rule0: if [prefix !consonant AND suffix !exact '`'] -> "ঊ"
      rule1: if [prefix punctuation AND suffix !exact '`'] -> "ঊ"
[274] dictionary.py:737-765  {"find":"ee","replace":"ী","reverse":"i"}
      rule0: if [prefix !consonant AND suffix !exact '`'] -> "ঈ"
      rule1: if [prefix punctuation AND suffix !exact '`'] -> "ঈ"
[278] dictionary.py:769-797  {"find":"e","replace":"ে","reverse":"e"}
      rule0: if [prefix !consonant AND suffix !exact '`'] -> "এ"
      rule1: if [prefix punctuation AND suffix !exact '`'] -> "এ"
[281] dictionary.py:800-818  {"find":"y","replace":"্য","reverse":"<absent>"}
      rule0: if [prefix !consonant AND prefix !punctuation] -> "য়"
      rule1: if [prefix punctuation] -> "ইয়"
[284] dictionary.py:821-838  {"find":"w","replace":"ও","reverse":"o"}
      rule0: if [prefix punctuation AND suffix vowel] -> "ওয়"
      rule1: if [prefix consonant] -> "্ব"
[285] dictionary.py:839-851  {"find":"x","replace":"ক্স","reverse":"ks"}
      rule0: if [prefix punctuation] -> "এক্স"
```

Character-level warning: `র‍্য` at indices 196/197 (and the non-rule `rrZ` / `rry`) contains
**U+200D ZERO WIDTH JOINER** between `র` (U+09B0) and `্` (U+09CD). Do not let an editor or a
normalisation pass strip it. Exact sequence: `U+09B0 U+200D U+09CD U+09AF`.

`,,` → `্‌` (index 290, `dictionary.py:856`) contains **U+200C ZERO WIDTH NON-JOINER**:
`U+09CD U+200C`. Same warning.

### 7.2 Rule-pattern quirks worth knowing

* `[198] rr` rule0 requires four conditions and produces the reph `র্`. Its `suffix !punctuation`
  means end-of-string **fails** it (end-of-string *is* punctuation), so `parse("grrr") == "গ্ররর"`
  rather than a reph.
* `[244] o` has an **empty** top-level `replace` — the inherent vowel after a consonant vanishes.
  `parse("o") == "অ"` (rule2 fires at start-of-string) but the `o` in `hobe` produces nothing.
* `[281] y` rule0's `prefix !consonant AND prefix !punctuation` can only be satisfied by a vowel,
  since the three classes partition every possible character.
* `[262] a` has three rules and the order between them decides everything. Worked trace for `naa`,
  second `a` at `cur = 2` (prefix character is `a` at index 1):
  rule0 needs `prefix punctuation` → `a` is a letter → fail;
  rule1 needs `prefix !exact 'a'` → the prefix *is* `a` → fail;
  rule2 needs `prefix exact 'a'` → pass → `আ`. Hence `parse("naa") == "নাআ"`.
  Rule1's `য়া` output therefore only fires when the preceding character is a **vowel other than `a`**
  (rule0 already claimed the punctuation/start-of-string case, and a consonant prefix falls through to
  the default `া`). Verified: `ea → এয়া`, `ia → ইয়া`, `ua → উয়া`, `oa → অয়া`, `kea → কেয়া`,
  `nea → নেয়া`, but `aa → আআ` and `naa → নাআ`. Trace it, don't guess.

### 7.3 The `end < len(haystack)` off-by-one

In `is_exact`, a positive `exact` window that ends exactly at the end of the string is reported as
**not** matching. Consequences:

* Positive `prefix exact` is unaffected (`exact_end = cur < len` always holds for a valid `cur`).
* Positive `suffix exact` would be affected — but the data contains none (§6.3).
* Negative `suffix !exact` at end-of-string evaluates to "passes" either way, because Python's slice
  would be short or the bound check fails; both routes give `False != True == True`.
* Every `suffix !exact '`'` guard is additionally masked by a longer non-rule pattern (`a`` `,
  `` i` ``, `` e` ``, `` ee` ``, `` oo` ``, `` u` ``, `` U` ``, `` I` ``, `` O` ``, `` OI` ``,
  `` OU` ``, `` rri` ``) that wins pass A before the rule pass runs.

**Net: no currently-observable behaviour difference — but write `end < len` anyway.** If avro.py ever
adds a positive `suffix exact`, a `<=` port would silently diverge.

---

## 8. The complete non-rule pattern table (273 entries)

`idx` is the position in the full 293-element `patterns` array (needed only if you also port reverse);
`line` is the line in `resources/dictionary.py`. **Order within this list is the match priority.**

```
  0 |   56 | {"find":"bhl","replace":"ভ্ল"}
  1 |   57 | {"find":"psh","replace":"পশ"}
  2 |   58 | {"find":"bdh","replace":"ব্ধ","reverse":"bdh"}
  3 |   59 | {"find":"bj","replace":"ব্জ","reverse":"bj"}
  4 |   60 | {"find":"bd","replace":"ব্দ","reverse":"bd"}
  5 |   61 | {"find":"bb","replace":"ব্ব","reverse":"bb"}
  6 |   62 | {"find":"bl","replace":"ব্ল","reverse":"bl"}
  7 |   63 | {"find":"bh","replace":"ভ","reverse":"bh"}
  8 |   64 | {"find":"vl","replace":"ভ্ল","reverse":"vl"}
  9 |   65 | {"find":"b","replace":"ব","reverse":"b"}
 10 |   66 | {"find":"v","replace":"ভ","reverse":"bh"}
 11 |   67 | {"find":"cNG","replace":"চ্ঞ","reverse":"cng"}
 12 |   68 | {"find":"cch","replace":"চ্ছ","reverse":"cch"}
 13 |   69 | {"find":"cc","replace":"চ্চ","reverse":"cc"}
 14 |   70 | {"find":"ch","replace":"ছ"}
 15 |   71 | {"find":"c","replace":"চ","reverse":"ch"}
 16 |   72 | {"find":"dhn","replace":"ধ্ন","reverse":"dhn"}
 17 |   73 | {"find":"dhm","replace":"ধ্ম","reverse":"dhm"}
 18 |   74 | {"find":"dgh","replace":"দ্ঘ","reverse":"dgh"}
 19 |   75 | {"find":"ddh","replace":"দ্ধ","reverse":"ddh"}
 20 |   76 | {"find":"dbh","replace":"দ্ভ","reverse":"dv"}
 21 |   77 | {"find":"dv","replace":"দ্ভ","reverse":"dv"}
 22 |   78 | {"find":"dm","replace":"দ্ম","reverse":"dd"}
 23 |   79 | {"find":"DD","replace":"ড্ড","reverse":"dd"}
 24 |   80 | {"find":"Dh","replace":"ঢ","reverse":"dh"}
 25 |   81 | {"find":"dh","replace":"ধ","reverse":"dh"}
 26 |   82 | {"find":"dg","replace":"দ্গ"}
 27 |   83 | {"find":"dd","replace":"দ্দ","reverse":"dd"}
 28 |   84 | {"find":"D","replace":"ড","reverse":"d"}
 29 |   85 | {"find":"d","replace":"দ","reverse":"d"}
 30 |   86 | {"find":"...","replace":"..."}
 31 |   87 | {"find":".`","replace":".","reverse":"."}
 32 |   88 | {"find":"..","replace":"।।"}
 33 |   89 | {"find":".","replace":"।","reverse":"."}
 34 |   90 | {"find":"ghn","replace":"ঘ্ন","reverse":"ghn"}
 35 |   91 | {"find":"Ghn","replace":"ঘ্ন","reverse":"ghn"}
 36 |   92 | {"find":"gdh","replace":"গ্ধ","reverse":"gdh"}
 37 |   93 | {"find":"Gdh","replace":"গ্ধ","reverse":"gdh"}
 38 |   94 | {"find":"gN","replace":"গ্ণ","reverse":"gn"}
 39 |   95 | {"find":"GN","replace":"গ্ণ","reverse":"gn"}
 40 |   96 | {"find":"gn","replace":"গ্ন","reverse":"gn"}
 41 |   97 | {"find":"Gn","replace":"গ্ন","reverse":"gn"}
 42 |   98 | {"find":"gm","replace":"গ্ম"}
 43 |   99 | {"find":"Gm","replace":"গ্ম","reverse":"gm"}
 44 |  100 | {"find":"gl","replace":"গ্ল","reverse":"gl"}
 45 |  101 | {"find":"Gl","replace":"গ্ল","reverse":"gl"}
 46 |  102 | {"find":"gg","replace":"জ্ঞ","reverse":"gg"}
 47 |  103 | {"find":"GG","replace":"জ্ঞ","reverse":"gg"}
 48 |  104 | {"find":"Gg","replace":"জ্ঞ","reverse":"gg"}
 49 |  105 | {"find":"gG","replace":"জ্ঞ","reverse":"gg"}
 50 |  106 | {"find":"gh","replace":"ঘ","reverse":"gh"}
 51 |  107 | {"find":"Gh","replace":"ঘ","reverse":"gh"}
 52 |  108 | {"find":"g","replace":"গ","reverse":"g"}
 53 |  109 | {"find":"G","replace":"গ","reverse":"g"}
 54 |  110 | {"find":"hN","replace":"হ্ণ","reverse":"nn"}
 55 |  111 | {"find":"hn","replace":"হ্ন","reverse":"nn"}
 56 |  112 | {"find":"hm","replace":"হ্ম","reverse":"mm"}
 57 |  113 | {"find":"hl","replace":"হ্ল"}
 58 |  114 | {"find":"h","replace":"হ","reverse":"h"}
 59 |  115 | {"find":"jjh","replace":"জ্ঝ"}
 60 |  116 | {"find":"jNG","replace":"জ্ঞ","reverse":"gg"}
 61 |  117 | {"find":"jh","replace":"ঝ","reverse":"jh"}
 62 |  118 | {"find":"jj","replace":"জ্জ","reverse":"jj"}
 63 |  119 | {"find":"j","replace":"জ","reverse":"j"}
 64 |  120 | {"find":"J","replace":"জ","reverse":"j"}
 65 |  121 | {"find":"kkhN","replace":"ক্ষ্ণ","reverse":"kkhn"}
 66 |  122 | {"find":"kShN","replace":"ক্ষ্ণ","reverse":"kkhn"}
 67 |  123 | {"find":"kkhm","replace":"ক্ষ্ম"}
 68 |  124 | {"find":"kShm","replace":"ক্ষ্ম","reverse":"kkh"}
 69 |  125 | {"find":"kxN","replace":"ক্ষ্ণ","reverse":"kkh"}
 70 |  126 | {"find":"kxm","replace":"ক্ষ্ম","reverse":"kkh"}
 71 |  127 | {"find":"kkh","replace":"ক্ষ","reverse":"kkh"}
 72 |  128 | {"find":"kSh","replace":"ক্ষ","reverse":"kkh"}
 73 |  129 | {"find":"ksh","replace":"কশ"}
 74 |  130 | {"find":"kx","replace":"ক্ষ","reverse":"kkh"}
 75 |  131 | {"find":"kk","replace":"ক্ক","reverse":"kk"}
 76 |  132 | {"find":"kT","replace":"ক্ট","reverse":"kt"}
 77 |  133 | {"find":"kt","replace":"ক্ত","reverse":"kt"}
 78 |  134 | {"find":"kl","replace":"ক্ল","reverse":"kl"}
 79 |  135 | {"find":"ks","replace":"ক্স","reverse":"ks"}
 80 |  136 | {"find":"kh","replace":"খ","reverse":"kh"}
 81 |  137 | {"find":"k","replace":"ক","reverse":"k"}
 82 |  138 | {"find":"lbh","replace":"ল্ভ"}
 83 |  139 | {"find":"ldh","replace":"ল্ধ"}
 84 |  140 | {"find":"lkh","replace":"লখ"}
 85 |  141 | {"find":"lgh","replace":"লঘ"}
 86 |  142 | {"find":"lph","replace":"লফ"}
 87 |  143 | {"find":"lk","replace":"ল্ক","reverse":"lk"}
 88 |  144 | {"find":"lg","replace":"ল্গ"}
 89 |  145 | {"find":"lT","replace":"ল্ট","reverse":"lt"}
 90 |  146 | {"find":"lD","replace":"ল্ড","reverse":"ld"}
 91 |  147 | {"find":"lp","replace":"ল্প","reverse":"lp"}
 92 |  148 | {"find":"lv","replace":"ল্ভ"}
 93 |  149 | {"find":"lm","replace":"ল্ম","reverse":"lm"}
 94 |  150 | {"find":"ll","replace":"ল্ল","reverse":"ll"}
 95 |  151 | {"find":"lb","replace":"ল্ব","reverse":"lb"}
 96 |  152 | {"find":"l","replace":"ল","reverse":"l"}
 97 |  153 | {"find":"mth","replace":"ম্থ"}
 98 |  154 | {"find":"mph","replace":"ম্ফ","reverse":"mf"}
 99 |  155 | {"find":"mbh","replace":"ম্ভ","reverse":"mv"}
100 |  156 | {"find":"mpl","replace":"মপ্ল"}
101 |  157 | {"find":"mn","replace":"ম্ন","reverse":"mn"}
102 |  158 | {"find":"mp","replace":"ম্প","reverse":"mp"}
103 |  159 | {"find":"mv","replace":"ম্ভ","reverse":"mv"}
104 |  160 | {"find":"mm","replace":"ম্ম","reverse":"mm"}
105 |  161 | {"find":"ml","replace":"ম্ল","reverse":"ml"}
106 |  162 | {"find":"mb","replace":"ম্ব","reverse":"mb"}
107 |  163 | {"find":"mf","replace":"ম্ফ","reverse":"mf"}
108 |  164 | {"find":"m","replace":"ম","reverse":"m"}
109 |  165 | {"find":"0","replace":"০","reverse":"0"}
110 |  166 | {"find":"1","replace":"১","reverse":"1"}
111 |  167 | {"find":"2","replace":"২","reverse":"2"}
112 |  168 | {"find":"3","replace":"৩","reverse":"3"}
113 |  169 | {"find":"4","replace":"৪","reverse":"4"}
114 |  170 | {"find":"5","replace":"৫","reverse":"5"}
115 |  171 | {"find":"6","replace":"৬","reverse":"6"}
116 |  172 | {"find":"7","replace":"৭","reverse":"7"}
117 |  173 | {"find":"8","replace":"৮","reverse":"8"}
118 |  174 | {"find":"9","replace":"৯","reverse":"9"}
119 |  175 | {"find":"NgkSh","replace":"ঙ্ক্ষ","reverse":"ngkh"}
120 |  176 | {"find":"Ngkkh","replace":"ঙ্ক্ষ","reverse":"ngkh"}
121 |  177 | {"find":"NGch","replace":"ঞ্ছ","reverse":"ngch"}
122 |  178 | {"find":"Nggh","replace":"ঙ্ঘ"}
123 |  179 | {"find":"Ngkh","replace":"ঙ্খ","reverse":"ngkh"}
124 |  180 | {"find":"NGjh","replace":"ঞ্ঝ"}
125 |  181 | {"find":"ngOU","replace":"ঙ্গৌ"}
126 |  182 | {"find":"ngOI","replace":"ঙ্গৈ"}
127 |  183 | {"find":"Ngkx","replace":"ঙ্ক্ষ","reverse":"ngkh"}
128 |  184 | {"find":"NGc","replace":"ঞ্চ","reverse":"nch"}
129 |  185 | {"find":"nch","replace":"ঞ্ছ","reverse":"ngch"}
130 |  186 | {"find":"njh","replace":"ঞ্ঝ"}
131 |  187 | {"find":"ngh","replace":"ঙ্ঘ"}
132 |  188 | {"find":"Ngk","replace":"ঙ্ক","reverse":"ngk"}
133 |  189 | {"find":"Ngx","replace":"ঙ্ষ"}
134 |  190 | {"find":"Ngg","replace":"ঙ্গ","reverse":"ngg"}
135 |  191 | {"find":"Ngm","replace":"ঙ্ম"}
136 |  192 | {"find":"NGj","replace":"ঞ্জ","reverse":"ngj"}
137 |  193 | {"find":"ndh","replace":"ন্ধ","reverse":"ndh"}
138 |  194 | {"find":"nTh","replace":"ন্ঠ","reverse":"nth"}
139 |  195 | {"find":"NTh","replace":"ণ্ঠ","reverse":"nth"}
140 |  196 | {"find":"nth","replace":"ন্থ","reverse":"nth"}
141 |  197 | {"find":"nkh","replace":"ঙ্খ","reverse":"ngkh"}
142 |  198 | {"find":"ngo","replace":"ঙ্গ","reverse":"ngg"}
143 |  199 | {"find":"nga","replace":"ঙ্গা"}
144 |  200 | {"find":"ngi","replace":"ঙ্গি"}
145 |  201 | {"find":"ngI","replace":"ঙ্গী"}
146 |  202 | {"find":"ngu","replace":"ঙ্গু"}
147 |  203 | {"find":"ngU","replace":"ঙ্গূ"}
148 |  204 | {"find":"nge","replace":"ঙ্গে"}
149 |  205 | {"find":"ngO","replace":"ঙ্গো"}
150 |  206 | {"find":"NDh","replace":"ণ্ঢ"}
151 |  207 | {"find":"nsh","replace":"নশ"}
152 |  208 | {"find":"Ngr","replace":"ঙর"}
153 |  209 | {"find":"NGr","replace":"ঞর"}
154 |  210 | {"find":"ngr","replace":"ংর"}
155 |  211 | {"find":"nj","replace":"ঞ্জ","reverse":"ngj"}
156 |  212 | {"find":"Ng","replace":"ঙ","reverse":"ng"}
157 |  213 | {"find":"NG","replace":"ঞ","reverse":"y"}
158 |  214 | {"find":"nk","replace":"ঙ্ক","reverse":"ngk"}
159 |  215 | {"find":"ng","replace":"ং","reverse":"ng"}
160 |  216 | {"find":"nn","replace":"ন্ন","reverse":"nn"}
161 |  217 | {"find":"NN","replace":"ণ্ণ"}
162 |  218 | {"find":"Nn","replace":"ণ্ন"}
163 |  219 | {"find":"nm","replace":"ন্ম","reverse":"nm"}
164 |  220 | {"find":"Nm","replace":"ণ্ম"}
165 |  221 | {"find":"nd","replace":"ন্দ","reverse":"nd"}
166 |  222 | {"find":"nT","replace":"ন্ট","reverse":"nt"}
167 |  223 | {"find":"NT","replace":"ণ্ট","reverse":"nt"}
168 |  224 | {"find":"nD","replace":"ন্ড","reverse":"nd"}
169 |  225 | {"find":"ND","replace":"ণ্ড","reverse":"nd"}
170 |  226 | {"find":"nt","replace":"ন্ত","reverse":"nt"}
171 |  227 | {"find":"ns","replace":"ন্স"}
172 |  228 | {"find":"nc","replace":"ঞ্চ","reverse":"nch"}
173 |  229 | {"find":"n","replace":"ন","reverse":"n"}
174 |  230 | {"find":"N","replace":"ণ","reverse":"n"}
175 |  231 | {"find":"OI`","replace":"ৈ","reverse":"oi"}
176 |  232 | {"find":"OU`","replace":"ৌ","reverse":"ou"}
177 |  233 | {"find":"O`","replace":"ো","reverse":"o"}
181 |  285 | {"find":"phl","replace":"ফ্ল","reverse":"fl"}
182 |  286 | {"find":"pT","replace":"প্ট","reverse":"pt"}
183 |  287 | {"find":"pt","replace":"প্ত","reverse":"pt"}
184 |  288 | {"find":"pn","replace":"প্ন","reverse":"pn"}
185 |  289 | {"find":"pp","replace":"প্প","reverse":"pp"}
186 |  290 | {"find":"pl","replace":"প্ল","reverse":"pl"}
187 |  291 | {"find":"ps","replace":"প্স","reverse":"ps"}
188 |  292 | {"find":"ph","replace":"ফ","reverse":"ph"}
189 |  293 | {"find":"fl","replace":"ফ্ল","reverse":"fl"}
190 |  294 | {"find":"f","replace":"ফ","reverse":"ph"}
191 |  295 | {"find":"p","replace":"প","reverse":"p"}
192 |  296 | {"find":"rri`","replace":"ৃ","reverse":"ri"}
194 |  314 | {"find":"rrZ","replace":"রর‍্য"}
195 |  315 | {"find":"rry","replace":"রর‍্য"}
199 |  410 | {"find":"Rg","replace":"ড়্গ"}
200 |  411 | {"find":"Rh","replace":"ঢ়"}
201 |  412 | {"find":"R","replace":"ড়","reverse":"r"}
203 |  450 | {"find":"shch","replace":"শ্ছ","reverse":"sch"}
204 |  451 | {"find":"ShTh","replace":"ষ্ঠ","reverse":"sth"}
205 |  452 | {"find":"Shph","replace":"ষ্ফ","reverse":"sf"}
206 |  453 | {"find":"Sch","replace":"শ্ছ","reverse":"sch"}
207 |  454 | {"find":"skl","replace":"স্ক্ল"}
208 |  455 | {"find":"skh","replace":"স্খ"}
209 |  456 | {"find":"sth","replace":"স্থ","reverse":"sth"}
210 |  457 | {"find":"sph","replace":"স্ফ","reverse":"sf"}
211 |  458 | {"find":"shc","replace":"শ্চ","reverse":"scch"}
212 |  459 | {"find":"sht","replace":"শ্ত"}
213 |  460 | {"find":"shn","replace":"শ্ন","reverse":"sn"}
214 |  461 | {"find":"shm","replace":"শ্ম","reverse":"ss"}
215 |  462 | {"find":"shl","replace":"শ্ল","reverse":"sl"}
216 |  463 | {"find":"Shk","replace":"ষ্ক","reverse":"sk"}
217 |  464 | {"find":"ShT","replace":"ষ্ট","reverse":"st"}
218 |  465 | {"find":"ShN","replace":"ষ্ণ","reverse":"sn"}
219 |  466 | {"find":"Shp","replace":"ষ্প","reverse":"sp"}
220 |  467 | {"find":"Shf","replace":"ষ্ফ","reverse":"sf"}
221 |  468 | {"find":"Shm","replace":"ষ্ম","reverse":"sm"}
222 |  469 | {"find":"spl","replace":"স্প্ল"}
223 |  470 | {"find":"sk","replace":"স্ক","reverse":"sk"}
224 |  471 | {"find":"Sc","replace":"শ্চ","reverse":"scch"}
225 |  472 | {"find":"sT","replace":"স্ট","reverse":"st"}
226 |  473 | {"find":"st","replace":"স্ত","reverse":"st"}
227 |  474 | {"find":"sn","replace":"স্ন","reverse":"sn"}
228 |  475 | {"find":"sp","replace":"স্প","reverse":"sp"}
229 |  476 | {"find":"sf","replace":"স্ফ","reverse":"sf"}
230 |  477 | {"find":"sm","replace":"স্ম","reverse":"sh"}
231 |  478 | {"find":"sl","replace":"স্ল","reverse":"sl"}
232 |  479 | {"find":"sh","replace":"শ","reverse":"sh"}
233 |  480 | {"find":"Sc","replace":"শ্চ","reverse":"scch"}
234 |  481 | {"find":"St","replace":"শ্ত"}
235 |  482 | {"find":"Sn","replace":"শ্ন","reverse":"sn"}
236 |  483 | {"find":"Sm","replace":"শ্ম","reverse":"ss"}
237 |  484 | {"find":"Sl","replace":"শ্ল","reverse":"sl"}
238 |  485 | {"find":"Sh","replace":"ষ","reverse":"sh"}
239 |  486 | {"find":"s","replace":"স","reverse":"s"}
240 |  487 | {"find":"S","replace":"শ","reverse":"sh"}
241 |  488 | {"find":"oo`","replace":"ু","reverse":"u"}
243 |  522 | {"find":"oZ","replace":"অ্য"}
245 |  553 | {"find":"tth","replace":"ত্থ"}
246 |  554 | {"find":"t``","replace":"ৎ","reverse":"t"}
247 |  555 | {"find":"TT","replace":"ট্ট","reverse":"tt"}
248 |  556 | {"find":"Tm","replace":"ট্ম"}
249 |  557 | {"find":"Th","replace":"ঠ","reverse":"th"}
250 |  558 | {"find":"tn","replace":"ত্ন"}
251 |  559 | {"find":"tm","replace":"ত্ম","reverse":"tt"}
252 |  560 | {"find":"th","replace":"থ","reverse":"th"}
253 |  561 | {"find":"tt","replace":"ত্ত","reverse":"tt"}
254 |  562 | {"find":"T","replace":"ট","reverse":"t"}
255 |  563 | {"find":"t","replace":"ত","reverse":"t"}
256 |  564 | {"find":"aZ","replace":"অ্যা"}
257 |  565 | {"find":"AZ","replace":"অ্যা"}
258 |  566 | {"find":"a`","replace":"া","reverse":"a"}
259 |  567 | {"find":"A`","replace":"া","reverse":"a"}
260 |  568 | {"replace":"আ","reverse":"a"}
261 |  569 | {"replace":"র","reverse":"r"}
263 |  615 | {"find":"i`","replace":"ি","reverse":"i"}
264 |  616 | {"replace":"ই","reverse":"i"}
266 |  645 | {"find":"I`","replace":"ী","reverse":"i"}
268 |  675 | {"find":"u`","replace":"ু","reverse":"u"}
269 |  676 | {"replace":"উ","reverse":"u"}
271 |  706 | {"find":"U`","replace":"ূ","reverse":"u"}
273 |  736 | {"find":"ee`","replace":"ী","reverse":"i"}
275 |  766 | {"find":"e`","replace":"ে","reverse":"e"}
276 |  767 | {"replace":"এ","reverse":"e"}
277 |  768 | {"replace":"ও","reverse":"w"}
279 |  798 | {"find":"z","replace":"য","reverse":"z"}
280 |  799 | {"find":"Z","replace":"্য"}
282 |  819 | {"find":"Y","replace":"য়","reverse":"y"}
283 |  820 | {"find":"q","replace":"ক","reverse":"k"}
286 |  852 | {"find":":`","replace":":"}
287 |  853 | {"find":":","replace":"ঃ"}
288 |  854 | {"find":"^`","replace":"^"}
289 |  855 | {"find":"^","replace":"ঁ","reverse":""}
290 |  856 | {"find":",,","replace":"্‌"}
291 |  857 | {"find":",","replace":","}
292 |  858 | {"find":"$","replace":"৳"}
```

---

## 9. `AVRO_EXCEPTIONS` — the 183 remap words

`dictionary.py:865-1050`. Dumped in **insertion order**, which is the order `find_in_remap` applies
them. Columns: index | roman value (what the user types, matched case-insensitively) | Bangla key
(what is substituted).

```
  0 | Bangladesh   | বাংলাদেশ
  1 | India        | ইন্ডিয়া
  2 | Pakistan     | পাকিস্তান
  3 | Srilanka     | শ্রীলঙ্কা
  4 | Nepal        | নেপাল
  5 | Bhutan       | ভুটান
  6 | Maldives     | মালদ্বীপ
  7 | Malaysia     | মালয়েশিয়া
  8 | Singapore    | সিঙ্গাপুর
  9 | China        | চায়না
 10 | Japan        | জাপান
 11 | Korea        | কোরিয়া
 12 | Indonesia    | ইন্দোনেশিয়া
 13 | Thailand     | থাইল্যান্ড
 14 | Philippines  | ফিলিপাইন্স
 15 | Vietnam      | ভিয়েতনাম
 16 | Saudi        | সৌদি
 17 | Arab         | আরব
 18 | Qatar        | কাতার
 19 | Oman         | ওমান
 20 | Bahrain      | বাহরেইন
 21 | Kuwait       | কুয়েত
 22 | Turkey       | তুর্কি
 23 | Egypt        | ইজিপ্ট
 24 | Jordan       | জর্ডান
 25 | Lebanon      | লেবানন
 26 | America      | আমেরিকা
 27 | Canada       | কানাডা
 28 | Mexico       | মেক্সিকো
 29 | Brazil       | ব্রাজিল
 30 | Argentina    | আর্জেন্টিনা
 31 | Chile        | চিলি
 32 | Peru         | পেরু
 33 | Europe       | ইউরোপ
 34 | France       | ফ্রান্স
 35 | Germany      | জার্মানি
 36 | Italy        | ইতালি
 37 | Spain        | স্পেইন
 38 | England      | ইংল্যান্ড
 39 | United       | ইউনাইটেড
 40 | Kingdom      | কিংডম
 41 | Australia    | অস্ট্রেলিয়া
 42 | Russia       | রাশিয়া
 43 | Ukraine      | ইউক্রেন
 44 | Dhaka        | ঢাকা
 45 | Chattogram   | চট্টগ্রাম
 46 | Khulna       | খুলনা
 47 | Rajshahi     | রাজশাহী
 48 | Barishal     | বরিশাল
 49 | Sylhet       | সিলেট
 50 | Rangpur      | রংপুর
 51 | Mymensingh   | ময়মনসিংহ
 52 | Cumilla      | কুমিল্লা
 53 | Bogura       | বগুড়া
 54 | Narayanganj  | নারায়ণগঞ্জ
 55 | Gazipur      | গাজীপুর
 56 | Tangail      | টাঙ্গাইল
 57 | Faridpur     | ফরিদপুর
 58 | Facebook     | ফেসবুক
 59 | Google       | গুগল
 60 | Wikipedia    | উইকিপিডিয়া
 61 | Whatsapp     | হোয়াটসঅ্যাপ
 62 | Twitter      | টুইটার
 63 | Linkedin     | লিঙ্কডইন
 64 | Instagram    | ইনস্টাগ্রাম
 65 | YouTube      | ইউটিউব
 66 | IMDb         | আইএমডিবি
 67 | Amazon       | অ্যামাজন
 68 | Microsoft    | মাইক্রোসফট
 69 | Apple        | অ্যাপল
 70 | Netflix      | নেটফ্লিক্স
 71 | Spotify      | স্পটিফাই
 72 | Telegram     | টেলিগ্রাম
 73 | Zoom         | জুম
 74 | Skype        | স্কাইপ
 75 | Discord      | ডিসকর্ড
 76 | Reddit       | রেডিট
 77 | Pinterest    | পিন্টারেস্ট
 78 | TikTok       | টিকটক
 79 | Snapchat     | স্ন্যাপচ্যাট
 80 | PayPal       | পেপাল
 81 | Visa         | ভিসা
 82 | Mastercard   | মাস্টারকার্ড
 83 | Express      | এক্সপ্রেস
 84 | American     | আমেরিকান
 85 | Maps         | ম্যাপস
 86 | Gmail        | জিমেইল
 87 | Drive        | ড্রাইভ
 88 | Dropbox      | ড্রপবক্স
 89 | Shopify      | শপিফাই
 90 | eBay         | ইবে
 91 | Alibaba      | আলিবাবা
 92 | AliExpress   | আলিএক্সপ্রেস
 93 | Computer     | কম্পিউটার
 94 | Laptop       | ল্যাপটপ
 95 | Mobile       | মোবাইল
 96 | Tablet       | ট্যাবলেট
 97 | Television   | টেলিভিশন
 98 | Radio        | রেডিও
 99 | Telephone    | টেলিফোন
100 | Internet     | ইন্টারনেট
101 | WiFi         | ওয়াইফাই
102 | Bluetooth    | ব্লুটুথ
103 | Website      | ওয়েবসাইট
104 | App          | অ্যাপ
105 | Software     | সফটওয়্যার
106 | Hardware     | হার্ডওয়্যার
107 | Game         | গেম
108 | Gaming       | গেমিং
109 | Document     | ডকুমেন্ট
110 | Video        | ভিডিও
111 | Audio        | অডিও
112 | Camera       | ক্যামেরা
113 | Printer      | প্রিন্টার
114 | Scanner      | স্ক্যানার
115 | Driver       | ড্রাইভার
116 | Password     | পাসওয়ার্ড
117 | Account      | অ্যাকাউন্ট
118 | Server       | সার্ভার
119 | Data         | ডেটা
120 | Database     | ডাটাবেস
121 | Network      | নেটওয়ার্ক
122 | Cloud        | ক্লাউড
123 | Security     | সিকিউরিটি
124 | File         | ফাইল
125 | Folder       | ফোল্ডার
126 | Message      | ম্যাসেজ
127 | Notification | নোটিফিকেশন
128 | Subscribe    | সাবস্ক্রাইব
129 | Like         | লাইক
130 | Comment      | কমেন্ট
131 | Share        | শেয়ার
132 | Upload       | আপলোড
133 | Download     | ডাউনলোড
134 | Stream       | স্ট্রিম
135 | Storage      | স্টোরেজ
136 | Taka         | টাকা
137 | Dollar       | ডলার
138 | Euro         | ইউরো
139 | Pound        | পাউন্ড
140 | Rupee        | রুপি
141 | Riyal        | রিয়াল
142 | Dinar        | দিনার
143 | Yen          | ইয়েন
144 | Won          | ওয়ন
145 | Baht         | বাথ
146 | Coronavirus  | করোনাভাইরাস
147 | Covid        | কোভিড
148 | Vaccine      | ভ্যাকসিন
149 | Hospital     | হসপিটাল
150 | Pharmacy     | ফার্মেসি
151 | Clinic       | ক্লিনিক
152 | Mask         | মাস্ক
153 | Test         | টেস্ট
154 | Market       | মার্কেট
155 | Shopping     | শপিং
156 | Store        | স্টোর
157 | Delivery     | ডেলিভারি
158 | Order        | অর্ডার
159 | Payment      | পেমেন্ট
160 | Bill         | বিল
161 | Discount     | ডিসকাউন্ট
162 | Offer        | অফার
163 | Coupon       | কুপন
164 | Sale         | সেল
165 | Return       | রিটার্ন
166 | Refund       | রিফান্ড
167 | Bus          | বাস
168 | Train        | ট্রেন
169 | Metro        | মেট্রো
170 | Station      | স্টেশন
171 | Airport      | এয়ারপোর্ট
172 | Flight       | ফ্লাইট
173 | Ticket       | টিকিট
174 | Taxi         | ট্যাক্সি
175 | Motorcycle   | মোটরসাইকেল
176 | Cycle        | সাইকেল
177 | Hotel        | হোটেল
178 | Restaurant   | রেস্টুরেন্ট
179 | Cafe         | ক্যাফে
180 | Park         | পার্ক
181 | Beach        | বিচ
182 | Mall         | মল
```

### 9.1 Properties (verified)

* 183 entries. All roman values are ASCII, alphanumeric, length ≥ 3 (shortest: `App`, `Bus`, `Won`,
  `Yen`). All Bangla keys are non-ASCII, length ≥ 2. No duplicate values.
* No value contains a regex metacharacter, `<` or `>`.
* Two values contain the literal substring `rm` (`Germany`, `Pharmacy`) — worth noting because the
  markers are `<rm>` / `</rm>`, but neither can match inside a marker (`Germany` and `Pharmacy` are
  longer than and unequal to any marker substring), so **there is no marker-collision bug**. Keep this
  invariant in mind if you ever add short exceptions.

### 9.2 Marker-based flow

`find_in_remap` inserts `<rm>` / `</rm>`; `_process_remapped` removes them. If **every** segment is a
marked one (`manual_required == False`), the fast path just strips the markers and never invokes the
transliterator at all.

### 9.3 The four order-dependent collisions (real, user-visible)

Because entries are applied in order and match as substrings, a shorter value listed earlier consumes
part of a longer word listed later:

| earlier | idx | later | idx | actual `parse()` result | what the later entry alone would have given |
|---|---|---|---|---|---|
| `America` | 26 | `American` | 84 | `american` → `আমেরিকান` | `আমেরিকান` — same by coincidence (`America` wins, leftover `n` transliterates to `ন`) |
| `Express` | 83 | `AliExpress` | 92 | `aliexpress` → `আলিএক্সপ্রেস` | `আলিএক্সপ্রেস` — same by coincidence (leftover `ali` transliterates to `আলি`) |
| `Drive` | 87 | `Driver` | 115 | `driver` → **`ড্রাইভর`** | `ড্রাইভার` — **differs** |
| `Data` | 119 | `Database` | 120 | `database` → **`ডেটাবাসে`** | `ডাটাবেস` — **differs** |

The last two are the ones that will show up as "bad word suggestions nobody can trace". Lock them in
with tests.

---

## 10. What NOT to port

`to_bijoy`, `to_unicode`, `reverse`, `rearrange_unicode_text`, `rearrange_bijoy_text`,
`reverse_with_rules`, `count_vowels`, `count_consonants`, the 219-entry `BIJOY_MAP` and every
`_async` / `_iter` wrapper are unreachable from Likhi's channel 4.

If you ever *do* need reverse transliteration, note that `_reverse_output_generator`
(`main.py:318-352`) has **no `cur_end` skip** — it re-examines every character even inside an already
consumed multi-character match, which produces visibly wrong output. Verified:

```
reverse("কষ্ট", remap_words=False) == "kosto্t"      # the halant leaks through
reverse("ওয়া", remap_words=False) == "wz়a"
```

Do not port this as a model of anything. `reverse("বাংলা") == "bangla"` only works because the word
happens to avoid the bug.

---

## 11. Golden test vectors

All produced by the live package. Run these against the Rust port. Column 2 is
`parse(t)` (i.e. `remap_words=True`, what Likhi gets); column 3 is `parse(t, remap_words=False)`.

| input | `parse(t)` | `parse(t, remap_words=False)` |
|---|---|---|
| `""` | `""` | `""` |
| `ami` | আমি | আমি |
| `bangla` | বাংলা | বাংলা |
| `amar` | আমার | আমার |
| `bus` | বাস | বুস |
| `busy` | বাসইয় | বুস্য |
| `apple` | অ্যাপল | আপ্পলে |
| `app` | অ্যাপ | আপ্প |
| `bangladesh` | বাংলাদেশ | বাংলাদেশ |
| `Bangladesh` | বাংলাদেশ | বাংলাদেশ |
| `BANGLADESH` | বাংলাদেশ | বাঞলাডেষ |
| `ami bangladeshi` | আমি বাংলাদেশই | আমি বাংলাদেশি |
| `american` | আমেরিকান | আমেরিচান |
| `database` | ডেটাবাসে | দাতাবাসে |
| `driver` | ড্রাইভর | দ্রিভের |
| `aliexpress` | আলিএক্সপ্রেস | আলিএক্সপ্রেসস |
| `gaming` | গেমিং | গামিং |
| `kajkorbo` | কাজকরব | কাজকরব |
| `kkhoma` | ক্ষমা | ক্ষমা |
| `rri` | ঋ | ঋ |
| `OI` | ঐ | ঐ |
| `oi` | অই | অই |
| `boi` | বই | বই |
| `doi` | দই | দই |
| `koi` | কই | কই |
| `rZ` | র‍্য | র‍্য |
| `ry` | র‍্য | র‍্য |
| `rrZ` | রর‍্য | রর‍্য |
| `` a` `` | া | া |
| `aZ` | অ্যা | অ্যা |
| `o` | অ | অ |
| `oo` | উ | উ |
| `` oo` `` | ু | ু |
| `w` | ও | ও |
| `wa` | — | ওয়া |
| `wo` | — | ওয় |
| `we` | — | ওয়ে |
| `awa` | — | আওা |
| `kw` | ক্ব | ক্ব |
| `kwa` | — | ক্বা |
| `aw` | আও | আও |
| `x` | এক্স | এক্স |
| `xylo` | এক্স্যল | এক্স্যল |
| `y` | ইয় | ইয় |
| `ky` | ক্য | ক্য |
| `ay` | আয় | আয় |
| `e` | এ | এ |
| `ee` | ঈ | ঈ |
| `` ee` `` | ী | ী |
| `` i` `` | ি | ি |
| `` rri` `` | — | ৃ |
| `` OI` `` | — | ৈ |
| `` t`` `` | — | ৎ |
| `` t` `` | — | ত` |
| `^` | — | ঁ |
| `` ^` `` | — | `^` |
| `,,` | — | ্‌ |
| `,` | — | `,` |
| `$` | — | ৳ |
| `:` | — | ঃ |
| `` :` `` | — | `:` |
| `Z` | — | ্য |
| `zZ` | — | য্য |
| `hobe` | হবে | হবে |
| `korchi` | করছি | করছি |
| `rasta` | রাস্তা | রাস্তা |
| `srkar` | স্রকার | স্রকার |
| `sundor` | সুন্দর | সুন্দর |
| `biSh` | বিষ | বিষ |
| `kobita` | কবিতা | কবিতা |
| `protha` | প্রথা | প্রথা |
| `tarpor` | তারপর | তারপর |
| `na` | না | না |
| `naa` | নাআ | নাআ |
| `aa` | — | আআ |
| `ea` | — | এয়া |
| `ia` | — | ইয়া |
| `ua` | — | উয়া |
| `oa` | — | অয়া |
| `kea` | — | কেয়া |
| `nea` | — | নেয়া |
| `haa` | হাআ | হাআ |
| `ha` | হা | হা |
| `joy` | জয় | জয় |
| `Joy` | জয় | জয় |
| `JOY` | জোয় | জোয় |
| `priyo` | প্রিয় | প্রিয় |
| `grrr` | গ্ররর | গ্ররর |
| `ttt` | ত্তত | ত্তত |
| `ss` | সস | সস |
| `q` | ক | ক |
| `q1` | — | ক১ |
| `a1` | — | আ১ |
| `1a` | — | ১আ |
| `1234` | ১২৩৪ | ১২৩৪ |
| `ngo` | — | ঙ্গ |
| `nga` | — | ঙ্গা |
| `ng` | — | ং |
| `jonno` | — | জন্ন |
| `dhonnobad` | — | ধন্নবাদ |
| `valo` | — | ভাল |
| `bhalo` | — | ভাল |
| `English` | — | এংলিশ |
| `ENGLISH` | — | এঞলীষ |
| `KHOJ` | — | খোজ |
| `hello world` | হেল্ল ওয়রলদ | হেল্ল ওয়রলদ |
| `.` | । | । |

(`—` means I did not record that column; the recorded column is authoritative.)

Additional intermediate-stage vectors:

```
fix_string_case("BANGLADESH")   == "baNGlaDeSh"
fix_string_case("KhOJ")         == "khOJ"
fix_string_case("Ami BhaLo")    == "ami bhalo"

find_in_remap("bus")             == ("<rm>বাস</rm>", False)
find_in_remap("busy")            == ("<rm>বাস</rm>y", True)
find_in_remap("apple")           == ("<rm>অ্যাপল</rm>", False)
find_in_remap("ami bangladeshi") == ("ami <rm>বাংলাদেশ</rm>i", True)
```

---

## 12. Performance baseline

Measured on this machine, venv Python 3.12, cache-miss path (2000 distinct 14-character words):

```
parse(word, remap_words=False):  ~994 µs / word
parse(word)  [remap on]:        ~1394 µs / word
```

The Python implementation is ~1.4 ms per word because `exact_find_in_pattern` does a **linear scan of
all 273 (then 20) patterns at every cursor position**, and `find_in_remap` compiles and applies 183
regexes per call. Against Likhi's ~30 ms per-keystroke budget that is a third of the budget for a
single word.

Rust targets:

* Replace the linear scan with a **static trie / Aho-Corasick-style longest-prefix automaton** keyed on
  the (at most 5-character) `find` strings. Two automata — one for the 273, one for the 20 — keeps the
  two passes exact.
* Replace the 183 regex compilations with a single pass: either an Aho-Corasick over the lowercased
  roman values with leftmost-longest… **no** — that would change behaviour (§9.3). It must remain
  "for each exception in order, replace all occurrences". A cheap faithful version: lowercase the input
  once, then for each exception do an ASCII-case-insensitive `find` loop. 183 × short-needle searches
  on a ≤30-char word is trivial.
* Expect low single-digit microseconds per word. Budget impact should be negligible.

---

## 13. Uncertain

Flagged honestly; a Rust implementer should treat these as open questions rather than settled facts.

1. **Unicode case folding in `find_in_remap`.** Python's `re.IGNORECASE` performs full Unicode
   case-insensitive matching. I verified that `re.match(re.escape("k"), "\u212A", re.I)` is `True` and
   that `parse("\u212Aingdom") == "কিংডম"` (U+212A KELVIN SIGN matched the `k` of `Kingdom`); likewise
   `i` matches U+0130 and U+0131, and `s` matches U+017F. A pure ASCII-case-insensitive Rust port will
   **not** reproduce this. I believe this is irrelevant for Likhi (the IME feeds ASCII from a physical
   keyboard) but I have not audited every path that can reach `parse()`. If some path can inject
   exotic Unicode, decide deliberately.
2. **Whether Likhi should keep exception remapping on at all.** `remap_words=True` is what Likhi gets
   today and it produces `busy → বাসইয়`, `bangladeshi → বাংলাদেশই`, `database → ডেটাবাসে`. These are
   arguably wrong as *suggestions*, but they are the current channel-4 behaviour and the scoring model
   was tuned against them. Porting faithfully is the safe default; changing it is a product decision
   outside this spec's scope, and would need a re-tune.
3. **`chain.from_iterable` on a generator of strings.** I reason it is equivalent to `"".join(gen)`
   because flattening strings to characters and re-joining is identity. I did not construct an
   adversarial case (e.g. a `replace` containing a surrogate pair) to prove it. Bangla is all BMP, so
   I am confident, but not certain, that no case distinguishes them.
4. **Stability of the "first match == longest match" property across avro.py versions.** I proved it
   for 2025.11.3 only. Pin the version, or re-run the check in §5.1 on any upgrade.
5. **`reverse` key semantics.** I documented `reverse` as present/absent because Likhi does not use it,
   and I did not fully characterise `reverse_with_rules` (`processor.py:187-228`), which appends an
   `"o"` suffix under conditions involving `AVRO_KAR` / `AVRO_SHONGKHA` / `AVRO_SHORBORNO` /
   `AVRO_IGNORE`. If reverse transliteration is ever needed, that function needs its own spec — and it
   has the bug described in §10.

---

## 14. Reproduction commands

Everything in this document can be regenerated:

```powershell
$py = "C:\Users\khaled\src\likhi\.venv\Scripts\python.exe"
& $py -c "from avro.resources import DICT; import json; print(json.dumps(DICT['avro'], ensure_ascii=False, indent=1))" > avro_rules.json
```

The line numbers in §7.1 and §8 were obtained by walking `resources/dictionary.py` with `ast` and
reading `elt.lineno` / `elt.end_lineno` for each element of the `patterns` list, zipped against the
runtime `DICT["avro"]["patterns"]` (both length 293, so the pairing is exact).
