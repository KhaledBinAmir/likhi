# Porting specification — `likhi.engine.personal` (PersonalStore)

**Source of truth:** `C:\Users\khaled\src\likhi\src\likhi\engine\personal.py` (141 lines, unchanged in
substance since commit `2719a36`, 2026-09-16).
**Spec written:** 2026-09-18. **Verified against:** a byte copy of the live user database at
`C:\Users\khaled\AppData\Local\Likhi\personal.sqlite` (368 `selections` rows, 358 `words` rows) using
`C:\Users\khaled\src\likhi\.venv\Scripts\python.exe` (Python 3.12.10, SQLite 3.49.1).

> **Hard requirement.** This file is the user's learned dictionary. A Rust port must open an existing
> `personal.sqlite` and read every row as the Python version reads it. Any change to the DDL, to the
> key normalization, or to the decay arithmetic silently de-references existing rows: the data is
> still on disk but the lookup never finds it, and the only visible symptom is "my suggestions got
> worse", which nobody can trace back to the port.

---

## 1. What the module is

Two learned signals, both in one small SQLite file the user owns:

| Table | Key | Meaning |
| --- | --- | --- |
| `selections` | (normalized roman, chosen word) | "when I type *this*, I pick *that*". Also how English passthrough is learned — picking the raw Latin `ok` a few times makes Latin the top candidate for `ok`. |
| `words` | chosen word | A personal unigram that lifts the user's own vocabulary (names, slang, workplace words) everywhere, including words the public lexicon never saw. |

Both counts are *decayed* counts, not integers: recency-weighted evidence, consumed by the ranker as
a log-linear feature. Nothing here leaves the machine.

The only two consumers in the codebase are:

* `src\likhi\engine\core.py:417` — `sel = self.personal.selections(r)`
* `src\likhi\engine\core.py:428` — `ft.personal_word = self.personal.word_count(word)`

and the single writer `src\likhi\engine\core.py:252` — `self.personal.learn(r, chosen, tuple(context))`.

---

## 2. Constants — reproduce exactly

| Name | Value | Location |
| --- | --- | --- |
| `DEFAULT_DB` | `%LOCALAPPDATA%\Likhi\personal.sqlite` | `personal.py:25` |
| `DEFAULT_DB` fallback | `<user home>\Likhi\personal.sqlite` when `LOCALAPPDATA` is unset | `personal.py:25` |
| `HALF_LIFE_DAYS` | `90.0` | `personal.py:26` |
| seconds per day | `86400.0` | `personal.py:44` |
| decay λ (default) | `math.log(2) / (90.0 * 86400.0)` = **`8.91392979115156e-08`** s⁻¹ | `personal.py:44` |
| half-life in seconds | `7776000.0` | derived |
| `ln 2` | `0.6931471805599453` | Python `math.log(2)` |
| increment per pick | `+1.0` | `personal.py:62`, `personal.py:68` |
| `known_words` threshold | `count >= 1` (raw stored count, **not** decayed) | `personal.py:119` |
| roman keep-set | `[^a-z0-9']+` removed, i.e. keep ASCII `a-z`, `0-9`, `U+0027` only | `textnorm.py:93` |
| WAL pragma | `PRAGMA journal_mode=WAL` | `personal.py:36` |

`DEFAULT_DB` is computed **at module import time** (`personal.py:25`), not per instance. Changing
`LOCALAPPDATA` after import has no effect in Python; a Rust `LazyLock`/`OnceLock` matches this, a
per-call `env::var` does not.

Verbatim:

```python
DEFAULT_DB = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Likhi" / "personal.sqlite"
HALF_LIFE_DAYS = 90.0
```
— `personal.py:25-26`

---

## 3. On-disk schema

### 3.1 DDL as issued by the code

```python
self.db.execute(
    "CREATE TABLE IF NOT EXISTS selections (roman TEXT, word TEXT, count REAL, last REAL, PRIMARY KEY (roman, word))"
)
self.db.execute(
    "CREATE TABLE IF NOT EXISTS words (word TEXT PRIMARY KEY, count REAL, last REAL)"
)
self.db.commit()
```
— `personal.py:37-43`

### 3.2 DDL as it actually exists in the user's database

Read back from `sqlite_master` on a copy of the live file:

```
('table', 'selections', 'selections', 2, 'CREATE TABLE selections (roman TEXT, word TEXT, count REAL, last REAL, PRIMARY KEY (roman, word))')
('table', 'words',      'words',      4, 'CREATE TABLE words (word TEXT PRIMARY KEY, count REAL, last REAL)')
('index', 'sqlite_autoindex_selections_1', 'selections', 3, None)
('index', 'sqlite_autoindex_words_1',      'words',      5, None)
```

**Confirmed: the stored DDL is character-for-character the string in the source, minus
`IF NOT EXISTS`** (SQLite strips that when recording the schema).

### 3.3 Columns

`selections`:

| cid | name | decl type | notnull | default | pk |
| --- | --- | --- | --- | --- | --- |
| 0 | `roman` | `TEXT` | 0 | NULL | 1 |
| 1 | `word` | `TEXT` | 0 | NULL | 2 |
| 2 | `count` | `REAL` | 0 | NULL | 0 |
| 3 | `last` | `REAL` | 0 | NULL | 0 |

`words`:

| cid | name | decl type | notnull | default | pk |
| --- | --- | --- | --- | --- | --- |
| 0 | `word` | `TEXT` | 0 | NULL | 1 |
| 1 | `count` | `REAL` | 0 | NULL | 0 |
| 2 | `last` | `REAL` | 0 | NULL | 0 |

Notes a port must honour:

* Neither table is `WITHOUT ROWID`. Both are ordinary rowid tables with an implicit
  `sqlite_autoindex_*` unique index over the PK columns.
* `words.word TEXT PRIMARY KEY` on a rowid table is **nullable** in SQLite (the historical
  `PRIMARY KEY`-is-not-`NOT NULL` quirk). `selections`'s composite PK is likewise nullable per
  column. The code never writes NULL (`personal.py:56` rejects empty strings), but a port that adds
  `NOT NULL` changes the schema and must not.
* Collation is default `BINARY` on every column. Lookups are byte-exact on UTF-8.
* `last` is a **Unix epoch float in seconds** (`time.time()`), not milliseconds, not an integer.
  Observed range in the live DB: `1789503347.0072174` … `1789648778.7058256`.

### 3.4 No explicit indexes

There are no `CREATE INDEX` statements anywhere in the module. The only indexes are the two
PK autoindexes. Query plans on the live file:

```
SELECT word, count, last FROM selections WHERE roman=?
    SEARCH selections USING INDEX sqlite_autoindex_selections_1 (roman=?)
SELECT count, last FROM words WHERE word=?
    SEARCH words USING INDEX sqlite_autoindex_words_1 (word=?)
SELECT word FROM words WHERE count >= 1
    SCAN words
SELECT roman, word, count, last FROM selections
    SCAN selections
```

### 3.5 File-level pragmas

Measured on a freshly created store and on the live file:

| Pragma | Value | Set by |
| --- | --- | --- |
| `journal_mode` | `wal` (persistent in the file header) | `personal.py:36`, explicit |
| `page_size` | `4096` | SQLite default |
| `encoding` | `UTF-8` | SQLite default |
| `user_version` | `0` | never set |
| `application_id` | `0` | never set |
| `auto_vacuum` | `0` (none) | SQLite default |
| `synchronous` | `2` (FULL) | Python/SQLite default, never changed |
| `wal_autocheckpoint` | `1000` pages (~4 MB at 4096 B) | SQLite default |
| `busy_timeout` | `5000` ms | **Python `sqlite3` default** (`timeout=5.0`), never set explicitly |
| `foreign_keys` | `0` | default |
| `locking_mode` | `normal` | default |
| `cache_size` | `-2000` (2 MB) | default |

`busy_timeout` is the one that bites: `rusqlite`/plain `libsqlite3` default to **0 ms** (fail
immediately on `SQLITE_BUSY`). The Python build gets 5000 ms for free. A Rust port must call
`busy_timeout(Duration::from_secs(5))` or it will start returning `database is locked` where the
Python version quietly waited.

The live directory carries the expected WAL sidecars: `personal.sqlite` 77,824 B,
`personal.sqlite-wal` 4,136,512 B, `personal.sqlite-shm` 32,768 B. The WAL sitting at ~4 MB is just
the 1000-page autocheckpoint threshold, not corruption (`PRAGMA integrity_check` → `ok`).

---

## 4. Construction

```python
self.path = Path(path) if path is not None else DEFAULT_DB
self.path.parent.mkdir(parents=True, exist_ok=True)
self.db = sqlite3.connect(str(self.path), check_same_thread=False)
self.db.execute("PRAGMA journal_mode=WAL")
... CREATE TABLE IF NOT EXISTS x2 ...
self.db.commit()
self.decay = math.log(2) / (half_life_days * 86400.0)
self._sel_cache: dict[str, dict[str, float]] = {}
self._word_cache: dict[str, float] = {}
```
— `personal.py:33-47`

Ordered obligations for the port:

1. `mkdir -p` the **parent directory** of the DB path. Opening must not fail on a first run.
2. Open (creating if absent).
3. `PRAGMA journal_mode=WAL` **before** the DDL.
4. `CREATE TABLE IF NOT EXISTS` both tables, then commit. Must be a no-op on an existing DB.
5. Compute λ from the instance's `half_life_days` (constructor keyword, default `HALF_LIFE_DAYS`),
   **not** from a global.
6. Start with empty caches.

`half_life_days` is a per-instance override used by tests (`tests\test_personal.py:28` passes `1.0`).
Keep it as a constructor parameter.

---

## 5. The decay formula

```python
def _decayed(self, count: float, last: float, now: float) -> float:
    return count * math.exp(-self.decay * max(0.0, now - last))
```
— `personal.py:93-94`

with

```python
self.decay = math.log(2) / (half_life_days * 86400.0)
```
— `personal.py:44`

i.e.

```
decayed(count, last, now) = count * exp( -(ln2 / (half_life_days * 86400)) * max(0, now - last) )
```

Exactness rules:

* **The `max(0.0, …)` clamp is load-bearing.** A `last` in the future (clock skew, a restored backup,
  a DST-confused writer) must yield the *undecayed* count, never a count that grows. Verified:
  `word_count(w, now=-1e6)` on a row with `last=0.0, count=1.0` returns exactly `1.0`.
* Arithmetic is IEEE-754 binary64 throughout (`f64` in Rust). SQLite `REAL` is an 8-byte double, so
  storage is lossless.
* λ is computed once per store as `ln2 / (days * 86400.0)`. Do **not** algebraically refactor to
  `exp(-ln2 * dt / half_life_seconds)` or `0.5f64.powf(dt / half_life_seconds)`; those differ in the
  last ulp and drift over repeated read-modify-write cycles, because `learn()` writes the decayed
  value back to disk (§6). Reproduce the two-step form: divide first, then multiply by `dt`.
* Rust: `count * (-self.decay * (now - last).max(0.0)).exp()`.

Test vector (`tests\test_personal.py:27-30`, re-verified):
`half_life_days=1.0`, one pick at `t=0`, read at `t=86400.0` → exactly `0.5`.

Default half-life test vector (measured): a single pick decays to `1.9377368409253052e-39` at
`now - last = 1e9` s. Useful as a bit-exactness check for the port's `exp`.

---

## 6. `learn()` — the only writer

```python
def learn(
    self, roman: str, chosen: str, context: tuple[str, ...] = (), *, when: float | None = None
) -> None:
    r = normalize_roman(roman)
    w = chosen if chosen == roman else canonical(chosen)
    if not r or not w:
        return
    now = when if when is not None else time.time()
    cur = self.db.execute(
        "SELECT count, last FROM selections WHERE roman=? AND word=?", (r, w)
    ).fetchone()
    count = self._decayed(cur[0], cur[1], now) + 1.0 if cur else 1.0
    self.db.execute(
        "INSERT OR REPLACE INTO selections (roman, word, count, last) VALUES (?,?,?,?)",
        (r, w, count, now),
    )
    cur = self.db.execute("SELECT count, last FROM words WHERE word=?", (w,)).fetchone()
    wcount = self._decayed(cur[0], cur[1], now) + 1.0 if cur else 1.0
    self.db.execute(
        "INSERT OR REPLACE INTO words (word, count, last) VALUES (?,?,?)", (w, wcount, now)
    )
    self.db.commit()
    self._sel_cache.pop(r, None)
    self._word_cache.pop(w, None)
```
— `personal.py:51-74`

Semantics, in order:

1. `r = normalize_roman(roman)` — see §9.
2. `w = chosen if chosen == roman else canonical(chosen)`. **Note the comparison is against the raw
   `roman` argument, not against `r`.** When the chosen string is byte-identical to the roman the
   user typed (the English-passthrough case), the word is stored **un-canonicalized**. Otherwise it
   is NFC-canonicalized. In practice `core.py:252` passes an already-normalized `r` as `roman`, so
   the equality only fires for lowercase-ASCII passthrough; but the port must reproduce the branch,
   because the two paths differ for any non-ASCII `chosen` that happens to equal `roman`.
3. Empty-guard: if `r` is empty **or** `w` is empty, return without writing. Verified:
   `learn("!!!", "কি")` and `learn("ki", "")` both write nothing.
4. `now = when if when is not None else time.time()`. `when` is a keyword-only float override used
   by tests; keep it in the port's API so the test vectors stay runnable.
5. **Decay-then-increment, written back to disk.** The existing row is decayed *to `now`* and `+1.0`
   is added; the new `count` and `last = now` replace the row. The stored number is therefore a
   running decayed total, not a raw tally. This is why the live DB has values like
   `33.84478251535172` for `('amar', 'আমার')`.
6. The same decay-then-increment runs a second time for the `words` row, keyed on `w` alone.
7. Single `commit()` at the end covering both writes.
8. Cache invalidation: pop `r` from `_sel_cache`, pop `w` from `_word_cache`. Note the **asymmetry** —
   only the exact `r` and the exact `w` are dropped. Other roman keys whose `selections` set is
   unchanged stay cached (correct), but any *other* cached `_word_cache` entry is untouched (also
   correct, since only `w`'s row moved).
9. `context` is accepted and **never used**. It is dead in this module (the bigram context lives in
   `core.py:272` `context_adjust`). Keep the parameter for API compatibility or drop it, but do not
   invent behaviour for it.

### 6.1 Transaction shape (exact)

Python's `sqlite3` legacy transaction control (`isolation_level == ''`, verified) auto-`BEGIN`s a
**DEFERRED** transaction before the first DML statement only. `SELECT` does not open one. So the
real sequence on the wire is:

```
SELECT ...                       -- outside any transaction
BEGIN DEFERRED                   -- implicit, at the first INSERT OR REPLACE
INSERT OR REPLACE INTO selections ...
SELECT ...                       -- inside the transaction
INSERT OR REPLACE INTO words ...
COMMIT
```

Consequences the port must be aware of:

* The two `INSERT`s are atomic with respect to each other. `selections` and `words` cannot diverge
  by a crash between them. Verified indirectly: zero `selections` rows lack a matching `words` row
  in the live DB.
* The first `SELECT` is **not** in the same transaction as its `INSERT`. Two concurrent `learn()`
  calls for the same key can both read the old count and one increment is lost. Python gets away
  with this because the layer above serializes writes (§10); a Rust port that keeps the same shape
  must keep the same external serialization, or wrap the whole thing in `BEGIN IMMEDIATE`.
* `synchronous=FULL` in WAL means each `commit()` fsyncs the WAL. That is one fsync per committed
  word. Measured cost on the real 368-row DB: `learn()` p50 **1.07 ms**, p90 1.22 ms, p99 4.11 ms,
  max 5.36 ms. That fits the ~30 ms per-keystroke budget with room to spare; a port should not
  "optimize" to `synchronous=NORMAL` without a decision, since that trades the user's learned
  dictionary against a millisecond.

### 6.2 `INSERT OR REPLACE` churns rowids

`INSERT OR REPLACE` is delete-then-insert, so the rowid changes on every update (verified: rowid 1 →
2 after a second `learn` of the same pair). Nothing reads the rowid, so this is behaviourally inert —
but it means **full-table-scan order is not insertion order and is not stable**. See §8.3.

---

## 7. `forget()`

```python
def forget(self, roman: str | None = None, word: str | None = None) -> None:
    if roman is not None and word is not None:
        self.db.execute(
            "DELETE FROM selections WHERE roman=? AND word=?", (normalize_roman(roman), word)
        )
    elif word is not None:
        self.db.execute("DELETE FROM selections WHERE word=?", (word,))
        self.db.execute("DELETE FROM words WHERE word=?", (word,))
    else:
        self.db.execute("DELETE FROM selections")
        self.db.execute("DELETE FROM words")
    self.db.commit()
    self._sel_cache.clear()
    self._word_cache.clear()
```
— `personal.py:76-89`

Three branches, and the third is a trap:

| Call | Effect |
| --- | --- |
| `forget(roman=R, word=W)` | Deletes one `selections` row for `(normalize_roman(R), W)`. **Leaves the `words` row alone** — the personal unigram still lifts that word everywhere. |
| `forget(word=W)` | Deletes every `selections` row with that word, and the `words` row. |
| `forget()` | Wipes both tables. |
| `forget(roman=R)` *(word omitted)* | **Falls into the `else` branch and wipes both tables entirely.** |

That last row is confirmed empirically, not inferred: after `learn("ki","কি")`, `learn("na","না")`,
`forget(roman="ki")` → `selections("na")` is `{}` and `words` has 0 rows.

Port faithfully, and flag it to the maintainer — it looks like a latent bug rather than intent, but
changing it is a behaviour change, not a port.

Also note the asymmetry in normalization: `roman` is passed through `normalize_roman`, `word` is
**not** passed through `canonical`. A caller holding a precomposed-nukta word (`য়` U+09DF) will not
match the stored decomposed form (`য` U+09AF + `়` U+09BC). See §9.3.

`forget()` clears **both** caches wholesale (unlike `learn`, which pops single keys).

---

## 8. Reads

### 8.1 `selections()`

```python
def selections(self, roman: str, *, now: float | None = None) -> dict[str, float]:
    """Decayed counts of words chosen for this exact roman string."""
    r = normalize_roman(roman)
    if r in self._sel_cache:
        return self._sel_cache[r]
    now = now if now is not None else time.time()
    rows = self.db.execute(
        "SELECT word, count, last FROM selections WHERE roman=?", (r,)
    ).fetchall()
    out = {w: self._decayed(c, last, now) for w, c, last in rows}
    self._sel_cache[r] = out
    return out
```
— `personal.py:96-107`

* Query: `SELECT word, count, last FROM selections WHERE roman=?` with the **normalized** roman.
  Exact equality on `roman` — no prefix matching, no `LIKE`, no fuzzy lookup.
* Returns a map `word -> decayed count`. Missing roman → empty map (not an error).
* **Row order.** No `ORDER BY`, but the plan is a `SEARCH … USING INDEX sqlite_autoindex_selections_1
  (roman=?)`, so rows come back **ascending by `word` in BINARY (UTF-8 byte) order** within the
  matched `roman`. Verified on the live DB: for `roman='khaiteche'`, `খাটিয়েছ` (…U+09BE…) precedes
  `খেটেছি` (…U+09C7…). Python preserves that order in the returned dict; `core.py:419` iterates
  `sel.items()` in that order to build the candidate feature table, so **the order is observable
  downstream** and affects tie-breaks. A Rust port must use an order-preserving map (e.g.
  `IndexMap`) or otherwise return rows in UTF-8-byte-ascending `word` order — a `HashMap` will
  scramble it.
* `core.py:418` computes `total = sum(sel.values()) or 1.0` and `core.py:424`
  `ft.personal_share = count / total`. The `or 1.0` catches a total of exactly `0.0` (all rows
  decayed to zero), avoiding a divide-by-zero. Float summation order follows the row order above, so
  matching the order also matches the last bits of `total`.

### 8.2 `word_count()`

```python
def word_count(self, word: str, *, now: float | None = None) -> float:
    if word in self._word_cache:
        return self._word_cache[word]
    now = now if now is not None else time.time()
    row = self.db.execute("SELECT count, last FROM words WHERE word=?", (word,)).fetchone()
    v = self._decayed(row[0], row[1], now) if row else 0.0
    self._word_cache[word] = v
    return v
```
— `personal.py:109-116`

* **No normalization of the argument.** The lookup key is the raw string as given. Verified:
  `learn("r", "ড়")` stores `['0x9a1','0x9bc']` (decomposed), then `word_count("ড়")` (precomposed
  U+09DC) returns `0.0` while `word_count("ড" + "়")` returns `1.0`. Callers are expected to pass
  already-canonical words; `core.py:428` does, because its candidate words come from the lexicon in
  canonical form.
* Missing word → `0.0`, and **the `0.0` is cached** (see §8.4).

### 8.3 `known_words()` and `export_jsonl()`

```python
def known_words(self) -> list[str]:
    return [w for (w,) in self.db.execute("SELECT word FROM words WHERE count >= 1")]
```
— `personal.py:118-119`

* Filters on the **raw stored `count`**, with no decay applied and no `now` involved. Verified:
  after one `learn`, `known_words()` returns the word; forcing `count=0.99` drops it. A row that has
  decayed far below 1 in real terms is still "known" as long as its stored value is ≥ 1.
* Full table `SCAN` in rowid order → unstable ordering (see §6.2). No caller in the repo relies on
  the order; do not add an `ORDER BY` unless you also accept the change.

```python
def export_jsonl(self, path: Path | str) -> int:
    import json
    n = 0
    with open(path, "w", encoding="utf-8") as f:
        for roman, word, count, last in self.db.execute(
            "SELECT roman, word, count, last FROM selections"
        ):
            f.write(
                json.dumps(
                    {"roman": roman, "word": word, "count": count, "last": last},
                    ensure_ascii=False,
                )
                + "\n"
            )
            n += 1
    return n
```
— `personal.py:121-137`

* Exports **`selections` only** — the `words` table is not exported. An export/import round trip
  therefore loses the personal unigram.
* UTF-8 file, `ensure_ascii=False` (raw Bengali in the output, not `\uXXXX` escapes).
* Key order in each JSON object is exactly `roman`, `word`, `count`, `last`.
* Returns the row count written.
* `count`/`last` are serialized by Python's `json` float repr (shortest round-tripping form, e.g.
  `33.84478251535172`). A Rust port should use `ryu`-style shortest round-trip formatting to match.
* No `ORDER BY`; scan order, unstable.

### 8.4 `close()`

```python
def close(self) -> None:
    self.db.close()
```
— `personal.py:139-140`

No checkpoint, no `PRAGMA optimize`, no cache flush. The WAL is left for SQLite to checkpoint.

---

## 9. Key normalization (`likhi.engine.textnorm`)

`personal.py:23` imports exactly two helpers:

```python
from likhi.engine.textnorm import canonical, normalize_roman
```

Neither `match_key` nor `to_output` is used here. Do not substitute them.

### 9.1 `normalize_roman` — the `selections.roman` key

```python
_RE_ROMAN_KEEP = re.compile(r"[^a-z0-9']+")

def normalize_roman(text: str) -> str:
    """Loose-romanization key: case-folded, ASCII letters/digits/apostrophe only.

    Case is deliberately dropped: unlike Avro, Likhi never relies on capitalisation to disambiguate.
    """
    return _RE_ROMAN_KEEP.sub("", text.casefold())
```
— `textnorm.py:93-101`

Two steps: **full Unicode case folding**, then delete every character outside `[a-z0-9']`.

**This is the single easiest thing to get wrong in Rust.** `str::casefold()` in Python is *full case
folding*, not lowercasing. Measured divergences that survive the filter:

| input | `casefold()` → `normalize_roman` | `lower()` (what `to_lowercase` gives) |
| --- | --- | --- |
| `ß` U+00DF | `ss` → **`ss`** | `ß` → would be stripped to `""` |
| `ﬁ` U+FB01 | `fi` → **`fi`** | `ﬁ` → `""` |
| `ﬀ` U+FB00 | `ff` → **`ff`** | `ﬀ` → `""` |
| `ﬄ` U+FB04 | `ffl` → **`ffl`** | `ﬄ` → `""` |
| `Straße` | `strasse` → **`strasse`** | `straße` → `strae` |
| `İ` U+0130 | `i̇` (i + U+0307) → **`i`** | same |
| `K` U+212A | `k` → **`k`** | same |

Rust `str::to_lowercase` is **not** equivalent. Use a full-case-folding implementation (e.g. the
`caseless` crate's `default_case_fold_str`, or `unicase`'s folding) and only then strip.

Other exact points:

* The kept apostrophe is `U+0027 APOSTROPHE` only. The typographic `’` U+2019 is **stripped**
  (verified: `normalize_roman("’")` → `""`).
* Full-width ASCII (`Ａ` U+FF21) casefolds to full-width, which is then stripped →
  `normalize_roman("ＡＢ")` → `""`. No NFKC anywhere.
* Idempotent. Verified against the live DB: **0 of the 363 distinct stored `roman` values differ from
  `normalize_roman(roman)`.**
* Live-DB shape: roman lengths 1–16 characters, 0 rows contain an apostrophe, 1 row contains a digit.

### 9.2 `canonical` — the `word` key

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

Three steps, **in this order**:

1. **Unicode NFC.**
2. Legacy khanda-ta: regex `ত্\u200d` (`U+09A4 U+09CD U+200D`) → `ৎ` `U+09CE`.
   Source: `_RE_LEGACY_KHANDA_TA = re.compile("ত" + VIRAMA + ZWJ)` — `textnorm.py:42`.
3. Candrabindu reorder: regex `ঁ([া-ৄেৈোৌৗৢৣ]+)` → `\1ঁ`, i.e. move `U+0981` to *after* the run of
   dependent vowel signs. Source: `textnorm.py:40`.

The vowel-sign class expands (verified by code-point dump — note the literal `-` at index 1 is a
range operator) to:

```
U+09BE .. U+09C4   (AA, I, II, U, UU, VOCALIC R, VOCALIC RR)
U+09C7, U+09C8     (E, AI)
U+09CB, U+09CC     (O, AU)
U+09D7             (AU LENGTH MARK)
U+09E2, U+09E3     (VOCALIC L, VOCALIC LL)
```

`U+09CD` (virama) is deliberately **not** in the class.

Relevant character constants (`textnorm.py:22-27`): `ZWNJ` U+200C, `ZWJ` U+200D, `VIRAMA` U+09CD,
`NUKTA` U+09BC, `CANDRABINDU` U+0981, `KHANDA_TA` U+09CE.

What `canonical` does **not** do, and a port must not add: it does not remove joiners, does not fold
Assamese `ৰ`/`ৱ`, and does not recompose nukta letters. Those live in `match_key` (`textnorm.py:66`)
and `to_output` (`textnorm.py:73`), neither of which this module calls.

### 9.3 Nukta: the storage form is DECOMPOSED — confirmed on real data

`ড়` U+09DC, `ঢ়` U+09DD, `য়` U+09DF are Unicode composition exclusions, so **NFC decomposes them**.
`canonical("ড়")` → `U+09A1 U+09BC`, and it is idempotent there.

Live-database evidence:

* 33 `words` rows contain `U+09BC NUKTA`.
* **0** rows contain any of U+09DC / U+09DD / U+09DF.
* e.g. `আয়` is stored as `['U+0986', 'U+09AF', 'U+09BC']`.
* 0 rows contain ZWJ or ZWNJ; 1 row contains `ৎ` U+09CE; 1 row contains `ঁ` U+0981, stored as
  `বাঁশি` = `U+09AC U+09BE U+0981 U+09B6 U+09BF` — candrabindu *after* the vowel sign, exactly as the
  reorder rule requires.
* **0 of the 358 distinct stored words (union of `selections.word` and `words.word`) differ from
  `canonical(word)`.**

A Rust port that normalizes to NFC with `unicode-normalization` gets this right. A port that
"helpfully" emits precomposed nukta on write, or that skips normalization because "the strings
already look fine", will write rows that never match the existing ones — the dictionary appears to
reset itself for every word containing য়, which is a large fraction of real Bangla.

---

## 10. Threading and locking

**There is no lock inside `PersonalStore`.** The only concurrency accommodation in the module is:

```python
self.db = sqlite3.connect(str(self.path), check_same_thread=False)
```
— `personal.py:35`

which merely disables Python's same-thread assertion. Safety comes from three things outside the
module:

1. **SQLite is compiled `THREADSAFE=1`** (verified via `pragma_compile_options`), i.e. serialized
   mode — individual statements on one connection are mutex-protected by SQLite itself.
   `sqlite3.threadsafety == 3`.
2. **The GIL** makes each `dict` get/set on `_sel_cache` / `_word_cache` atomic.
3. **The layer above serializes writes.** `src\likhi\server.py:59-68` — `SuggestService` holds a
   `threading.Lock`, and `server.py:139-141`:

   ```python
   def learn(self, roman: str, chosen: str, context: tuple[str, ...]) -> None:
       with self.lock:
           self.engine.learn(roman, chosen, context)
       self.full_cache.clear()
   ```

   The comment at `server.py:64-67` is explicit that **reads are deliberately lock-free** ("Holding a
   lock around the model call would make the fast path wait for the model, defeating the deadline").
   The server is a `ThreadingTCPServer` with `daemon_threads = True` (`server.py:209-215`), so
   `selections()` and `word_count()` genuinely run concurrently with a `learn()`.

Contract to reproduce in Rust:

* One connection, shared across threads. `rusqlite::Connection` is `Send` but not `Sync`, so the
  practical port is `Mutex<Connection>` (or a small pool). A `Mutex` around **every** operation is
  *stricter* than Python and is safe — reads are 7.5 µs uncached (measured), so the contention cost
  is negligible and nowhere near the ~30 ms keystroke budget.
* Set `busy_timeout` to 5000 ms (§3.5) — Python gets it by default, Rust does not.
* The caches need interior mutability across threads (`Mutex<HashMap>` / `RwLock`). Note that the
  Python caches are **racy by construction**: a reader can re-insert a stale `_sel_cache[r]` entry
  *after* a concurrent `learn()` popped it, because the read (`personal.py:102`) and the write-back
  (`personal.py:106`) are not atomic with respect to `learn`'s `pop` (`personal.py:73`). The window
  is microseconds and the consequence is one stale suggestion set; the Python code tolerates it. A
  Rust port holding one lock across read-and-insert removes the race — that is an improvement, not a
  deviation to worry about, but be aware the two implementations can differ under a stress test.

Measured latencies on a copy of the real 368-row DB (Python, warm):

| operation | p50 | p99 |
| --- | --- | --- |
| `learn()` (incl. fsync) | 1.07 ms | 4.11 ms |
| `selections()` cache miss | 7.5 µs | 10.6 µs |
| `selections()` cache hit | 0.6 µs | 0.7 µs |
| store open (existing DB) | 0.65 ms | — |

---

## 11. The in-memory caches — the subtlest behaviour in the file

```python
# small in-memory caches, invalidated on learn()
self._sel_cache: dict[str, dict[str, float]] = {}
self._word_cache: dict[str, float] = {}
```
— `personal.py:45-47`

Four properties, all verified, all of which a naive port will get wrong:

1. **The caches are not time-aware.** `selections(roman, now=X)` checks the cache *before* it looks
   at `now` (`personal.py:99-101`), so the first call's `now` freezes the decayed values for the
   life of the entry. Verified:

   ```
   selections("ki", now=0.0)    -> {'কি': 1.0}
   selections("ki", now=1e9)    -> {'কি': 1.0}          # cached, NOT re-decayed
   fresh store, now=1e9         -> {'কি': 1.9377368409253052e-39}
   ```

   `word_count` has the identical shape (`personal.py:110-111`). The port must cache *without*
   re-decaying, i.e. cache the computed float, not the raw row. Adding a TTL, or keying the cache by
   `now`, changes suggestion output.

2. **Misses are cached.** `word_count` stores `0.0` for a word with no row (`personal.py:114-115`),
   and that `0.0` is never invalidated unless a `learn()` for that exact word pops it. Verified: a
   direct `INSERT` behind the store's back is not seen. Unbounded negative cache — no eviction, no
   size cap, on either cache.

3. **`selections()` returns the cached dict object itself, not a copy.** Verified:
   `ps.selections("ki") is ps.selections("ki")` → `True`. Any caller that mutated the returned map
   would corrupt the cache. `core.py:417-425` only reads it. In Rust, return a reference/`Arc` or a
   clone — cloning is the safe choice and is not observably different for the existing callers.

4. **Invalidation granularity differs by method.** `learn()` pops exactly one key from each cache
   (`personal.py:73-74`); `forget()` clears both entirely (`personal.py:88-89`).

Also note the separate, unrelated cache one level up: `core.py:242` `self._cache` (suggestion cache,
4096 entries, `OrderedDict`), invalidated per-roman at `core.py:253-254`, and `server.py:142`
`self.full_cache.clear()`. Those are not part of this module but they are why a stale
`PersonalStore` cache is usually invisible in practice.

---

## 12. Schema migration

**There is none, and none is needed.**

* No `PRAGMA user_version` read or write anywhere in the repo (grep for `user_version`, `ALTER TABLE`
  → 0 hits outside this spec). Live DB has `user_version = 0`, `application_id = 0`.
* No `ALTER TABLE`, no version table, no column back-fill.
* The only "migration" mechanism is `CREATE TABLE IF NOT EXISTS`, which is additive and idempotent.
* Git history for the file is two commits — `2719a36` (introduction, 2026-09-16) and `fb2d396`
  (formatting only, verified by reading the diff: every hunk is line re-wrapping). **The DDL string
  has never changed since the table was first created**, so there is exactly one schema version in
  the wild.

Port obligations:

* Run the same `CREATE TABLE IF NOT EXISTS` statements on open and accept an existing file unchanged.
* Do **not** introduce `user_version` stamping as part of the port unless the maintainer decides to;
  a Rust version that writes `user_version = 1` into an existing file is harmless today but breaks
  the ability to run the Python and Rust engines against the same file during the transition.
* Both engines *can* share the file: the schema is identical and WAL allows a reader and a writer
  concurrently. Two *writers* (one Python server, one Rust server) would interleave increments; the
  comment at `server.py:210-213` shows the design already treats "two engines with different personal
  dictionaries" as the failure to prevent.

---

## 13. How the values are consumed (why exactness matters)

Not part of this module, but the port's correctness is only observable through these:

```python
def personal_bonus(w: dict[str, float], count: float, share: float) -> float:
    return w["personal_sel"] * math.log1p(count) * (0.5 + share)
```
— `core.py:154-161`, with `DEFAULT_WEIGHTS["personal_sel"] = 5.0` (`core.py:69`) and
`DEFAULT_WEIGHTS["personal_word"] = 0.6` (`core.py:70`).

```python
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
```
— `core.py:416-428`

and in scoring:

```python
if ft.personal_sel:
    s += personal_bonus(w, ft.personal_sel, ft.personal_share)
if ft.personal_word:
    s += w["personal_word"] * math.log1p(ft.personal_word)
```
— `core.py:512-515`

Key points for the port:

* `word_count()` is called **only for non-Latin candidates** (`core.py:427`). Latin passthrough
  candidates get `personal_sel` but never `personal_word`.
* The Latin detection compares the stored word against **both** the normalized `r` and the raw
  `roman` (`core.py:421`). The live DB has 2 such rows: `('win','Win')` and `('space','Space')` — note
  the stored word is `Win`, not `win`, so `word == r` is false and `word == roman` is what fires when
  the user's raw input was `Win`.
* `if ft.personal_sel:` / `if ft.personal_word:` are truthiness tests on floats. A decayed count that
  underflows to exactly `0.0` contributes nothing; any non-zero denormal still goes through
  `log1p`. Reproduce as `!= 0.0`, not `> epsilon`.
* `math.log1p` — use `f64::ln_1p`, not `(1.0 + x).ln()`.

---

## 14. Test vectors for the port

From `C:\Users\khaled\src\likhi\tests\test_personal.py` (all re-verified today), plus measurements.

```python
ps.learn("amr", "আমার", when=1_000_000.0)
ps.learn("amr", "আমার", when=1_000_100.0)
ps.learn("amr", "আমরা", when=1_000_200.0)
sel = ps.selections("amr", now=1_000_200.0)
assert sel["আমার"] > sel["আমরা"] > 0
assert ps.word_count("আমার", now=1_000_200.0) > 1.9
```
— `tests\test_personal.py:4-12`

```python
ps.learn("Amr", "আমার", when=0.0)
assert "আমার" in ps.selections("amr", now=0.0)          # case folding
```
— `tests\test_personal.py:15-18`

```python
ps.learn("ok", "ok", when=0.0)
assert ps.selections("ok", now=0.0) == {"ok": 1.0}      # Latin passthrough
```
— `tests\test_personal.py:21-24`

```python
ps = PersonalStore(path, half_life_days=1.0)
ps.learn("ki", "কি", when=0.0)
assert abs(ps.selections("ki", now=86400.0)["কি"] - 0.5) < 1e-6
```
— `tests\test_personal.py:27-30`

```python
ps.learn("ki", "কি", when=0.0)
ps.forget(word="কি")
assert ps.selections("ki", now=0.0) == {}
assert ps.word_count("কি", now=0.0) == 0.0
```
— `tests\test_personal.py:33-38`

Additional vectors established here that the existing suite does **not** cover — a port should add
them, because each one is a place a reasonable implementer would diverge:

| # | Setup | Expected |
| --- | --- | --- |
| A | `learn("ki","কি",when=0.0)`; `selections("ki", now=0.0)`; then `selections("ki", now=1e9)` | both `{'কি': 1.0}` — cache is not time-aware |
| B | fresh store on the same file, `selections("ki", now=1e9)` | `{'কি': 1.9377368409253052e-39}` |
| C | `word_count(w, now=-1e6)` on a row `count=1.0, last=0.0` | `1.0` — the `max(0.0, …)` clamp |
| D | `learn("ki","কি")`, `learn("na","না")`, `forget(roman="ki")` | **both tables empty** |
| E | `learn("r","ড়")` (U+09DC) → inspect stored word | `U+09A1 U+09BC` (decomposed) |
| F | after E, `word_count("ড়")` vs `word_count("ড"+"়")` | `0.0` vs `1.0` |
| G | `learn("!!!","কি")` and `learn("ki","")` | 0 rows written |
| H | `word_count("কি")` (miss), then direct `INSERT` of `('কি',5.0,0.0)`, then `word_count("কি")` | `0.0` both times — misses are cached |
| I | `learn("ki","কি")`, set `words.count=0.99`, `known_words()` | `[]` — raw count, not decayed |
| J | `normalize_roman("Straße")` | `"strasse"` — full case folding, not lowercasing |
| K | `selections()` on a roman with 2 words | iteration order = ascending UTF-8 bytes of `word` |

---

## 15. Rust implementation checklist

- [ ] `rusqlite` (bundled SQLite), one `Mutex<Connection>`.
- [ ] `busy_timeout(5s)` explicitly.
- [ ] `PRAGMA journal_mode=WAL` before DDL; leave `synchronous` at FULL.
- [ ] `CREATE TABLE IF NOT EXISTS` with the **byte-identical** DDL strings from §3.1.
- [ ] `fs::create_dir_all(path.parent())` before opening.
- [ ] `DEFAULT_DB` from `LOCALAPPDATA` (fallback home) resolved once, in a `LazyLock`.
- [ ] λ = `2f64.ln() / (half_life_days * 86400.0)`, stored per instance.
- [ ] `decayed = count * (-lambda * (now - last).max(0.0)).exp()`.
- [ ] `normalize_roman`: **full Unicode case folding** (`caseless`), then retain only `a-z0-9'`.
- [ ] `canonical`: `unicode-normalization` NFC → khanda-ta regex → candrabindu-reorder regex, in that
      order.
- [ ] `learn`: decay-then-`+1.0`, write back both tables, one commit, pop one key from each cache.
- [ ] `selections` returns an **order-preserving** map (UTF-8-byte-ascending by `word`).
- [ ] Caches store the *computed* float and ignore `now` on a hit; negative results are cached.
- [ ] `forget(Some(roman), None)` wipes everything (bug-for-bug), and `forget(Some(r), Some(w))`
      leaves the `words` row.
- [ ] `known_words` filters on the raw stored count `>= 1`.
- [ ] `export_jsonl` exports `selections` only, keys in order `roman, word, count, last`, UTF-8
      unescaped, shortest-round-trip floats.
- [ ] Do not write `user_version`.

---

## 16. Uncertain

Flagged rather than guessed:

1. **Is `forget(roman=...)` wiping the whole database intentional?** The code path is unambiguous and
   I verified the behaviour, but no caller exercises it and no test covers it. I cannot tell from the
   code whether the author meant `elif roman is not None: DELETE FROM selections WHERE roman=?`.
   **Ask before "fixing" it in the port.**
2. **The `chosen == roman` comparison in `learn` (`personal.py:55`)** — compared against the raw
   argument rather than the normalized `r`. I could not determine whether this is deliberate (so
   that a case-preserving Latin passthrough like `Win` survives unchanged) or incidental. The live DB
   contains `('win','Win')` and `('space','Space')`, which are consistent with *either* reading,
   because those rows went through the `else` branch (`canonical("Win") == "Win"`). Port the branch
   verbatim; do not rationalize it.
3. **Cache unboundedness.** `_sel_cache` and `_word_cache` have no eviction. Over a long session
   `_word_cache` grows with every distinct candidate word the ranker scores, which is a superset of
   what the user typed. I have not measured the steady-state size, so I cannot say whether the Rust
   port needs an LRU. Flagging as a memory-footprint question for the maintainer (the project tracks
   RAM as a metric), not a correctness one.
4. **Whether the Python and Rust engines will ever run against the same file concurrently.** §12 says
   the schema permits it; I do not know the intended cutover plan, and if both write at once the
   read-modify-write in `learn` (§6.1) can lose increments across processes — the in-process
   `SuggestService` lock does not span processes. If concurrent operation is planned, `learn` needs
   `BEGIN IMMEDIATE`.
5. **Non-Windows behaviour.** `LOCALAPPDATA` is Windows-only, so on Linux/macOS the path falls back to
   `~/Likhi/personal.sqlite`. I did not verify any non-Windows run; there is no XDG handling. If the
   port targets other platforms, the path policy is an open decision, not a port detail.
6. **`export_jsonl` float formatting.** I asserted that Python's `json` shortest-round-trip repr
   matches Rust `ryu`. This is true for `f64` in general, but I did not diff a full export of the
   live database between the two implementations. Worth one test if the export format is contractual.
7. **The 4.1 MB WAL next to a 77 KB database.** Consistent with the default 1000-page autocheckpoint
   and a running server holding a read transaction open, and `integrity_check` returns `ok`. I did
   not investigate whether a long-lived reader in `SuggestService` is *preventing* checkpoints. If
   the WAL keeps growing in production, that is worth a separate look — it is not a schema issue.
