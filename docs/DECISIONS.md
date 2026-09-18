# Decision log

## 2026-09-18: In a sandboxed application, the engine draws the candidate list

- **Context.** Likhi typed Bangla in Telegram but showed no suggestions and committed whatever it
  had ranked first. Three earlier explanations were wrong, including two of mine: that Store
  applications answer `BeginUIElement` with `show = FALSE` (they answer TRUE), and that the problem
  was Store applications at all.
- **What it actually is.** A window created by a process inside an **AppContainer** never reaches
  the desktop. `CreateWindowExW` returns a handle, `SetWindowPos` reports success, and nothing is
  composed. Established by enumerating every window on the machine while typing:
  `LikhiCandidateWindow` was present in Code, Notepad and WhatsApp, and absent from Telegram, while
  Telegram's own log recorded showing a list at the correct screen coordinates on every keystroke.
- **WhatsApp is the control.** Also a Store application, also immersive
  (`TF_TMF_IMMERSIVEMODE`), and it works -- because `WhatsApp.Root.exe` runs at full trust. The
  dividing line is the sandbox, not the Store, and it is not something an application can opt out
  of. There is no fix available from inside the process.
- **So the engine draws it.** It is an ordinary user process, so its windows do reach the desktop
  and a topmost one sits above the sandboxed application. Two operations, `ui_show` and `ui_hide`.
  This is what Microsoft's own IMEs do, for the same reason.
- **Only the drawing moved.** What the candidates are, which is highlighted, when to commit and
  when to refine all stay in the text service, which is the only side that knows what is being
  typed. The engine is a remote control for a window.
- **Routed on the token, not the path.** `TokenIsAppContainer`, because "packaged" and "sandboxed"
  are different properties and only the second one matters. Applications that work today are
  untouched and still draw in-process, which is the faster path and the better tested one.
- **Cost measured before trusting it.** The added round trip per keystroke: p50 0.038 ms, p95
  0.083 ms, worst of 200 calls 0.82 ms, against a 30 ms budget. The engine's side only posts to a
  channel; the drawing happens on its own UI thread.
- **The window is one shared crate, not a copy each.** Two copies would mean the list looked
  different depending on which application you were typing in, and every fix would have to be made
  twice.
- **A window that outlives its client needs a watchdog.** An application killed mid-word would
  leave a list on the desktop that nothing owns and nobody can dismiss. Twenty seconds without an
  update and the engine takes it down; the ordinary path hides it long before that.

## 2026-09-18: The text service starts the engine

- A Windows update restarted the machine and the engine did not come back. The `Run` key only fires
  at sign-in and nothing supervises the process, so the keyboard did nothing in every application
  with no indication why. For a pilot that is the worst kind of failure: it reads as the whole
  product being broken and produces no usable bug report.
- When neither transport answers there is very likely no engine, so the text service starts it,
  found relative to its own DLL. Not waited for -- the keystroke that discovers the problem still
  goes without suggestions -- and at most once every 30 seconds, because a failure to start is
  usually permanent and retrying per keystroke would fork a process per key.
- It cannot work from inside a sandboxed application, which may not launch an executable outside its
  package. That is acceptable: the same person is typing in other applications that are not
  sandboxed, and the first keystroke in any of them brings the engine back for all of them.

## 2026-09-18: The Python is retired; the goldens become a fixed record

- **Context.** After the engine was ported, the Python was kept as the reference implementation and
  the research harness. That meant two implementations of every ranking decision, and a rule that
  changing behaviour meant changing the Python first, re-recording goldens, then making the Rust
  agree. With the harness itself ported, keeping the Python meant maintaining a second engine purely
  to certify the first.
- **Each tool was ported and cross-checked before anything was deleted.** Not by review; by running
  both on the same data and requiring the same numbers. `words`: delta 0.0 on five metrics across
  three datasets. `tune`: both report `start top1: feedback-words 78.26, macro 78.26`. `sentences`:
  `wer=22.55  wer_bengali_tokens=22.57` from both. `replay`: `keystrokes=1368 saved_top1=4.24%
  saved_top5=17.98% never_top1=12.33% never_top5=4.67%` from both. `stress` was checked at the
  generator, since its output is a sample: the full 88-variant sequence from one seeded generator
  matches, which is the check that catches a style consuming a different number of draws. `report`
  was checked on the real four-install pilot: `show` identical, `collect` identical, and the 30-row
  feedback file identical byte for byte.
- **The goldens can no longer be regenerated, on purpose.** They are 96,537 recorded results that a
  second, independent implementation once agreed with. Frozen, they are a stronger guarantee than a
  file that can be re-recorded whenever the engine changes: a change that moves one now has to be
  justified in the commit that moves it. What is lost is the ability to add new golden coverage for
  existing behaviour, which is a real cost and is accepted.
- **What stays in Python, and why it is not an exception.** `server/ingest_server.py` runs in a
  container, not on anyone's machine, so none of the reasons for the port apply to it. `scripts/`
  prepares data and drives builds on a developer's machine; `build_rust_data.py` converts model
  weights in NumPy and writes the result into the blob the engine maps, so the arithmetic happens
  once, here, rather than at every startup. The rule is not "no Python" but "nothing on a user's
  machine is Python".
- **Coverage was moved, not dropped.** Deleting 13 Python test files would have quietly removed
  coverage of things that had already failed once in production, so those were ported first: config
  discovery and the per-user override (9 tests, including the byte-order-mark case that silently
  disabled telemetry on fresh installs), the privacy guarantees of each telemetry mode (5 tests),
  and the ad-hoc sync rule (4 tests). The ingest server keeps its own tests in Python; the client
  half of that conversation moved to `engine/tests/ingest.rs`.
- **Two real bugs were found by doing this rather than by reading.** `likhi-report sync --drop`
  would have advanced a live install's sync offsets, so telemetry shipped to a debug folder would
  never have reached the collector -- the exact failure the Python's `sync_state` tests were written
  about after it happened once. And `pyrandom`'s `randbelow` computed its bit length against a
  64-bit width for a 32-bit value, making it 32 too large for every input; a release build masks the
  resulting over-wide shift back onto the intended amount, so every result was correct and nothing
  failed until a debug build ran the assertion.

Short records of choices that are not obvious from the code. Newest first.

## 2026-09-18: The lexicon is built in Rust, and marisa leaves the build

- **What changed.** `src/likhi/data/` built `marisa` tries and a conversion step turned them into
  the `.lkx` tables the engine reads. `engine/src/bin/lexicon.rs` now writes `.lkx` directly, so
  `marisa_trie` is no longer needed to build anything. The Python engine gained a `.lkx` reader
  (`src/likhi/lkx.py`) so both implementations can be measured on identical tables.
- **How faithful it is.** Four of the six tables -- unigrams, keys, bigrams, bigram_totals -- are
  byte-identical to the Python builder's output. Two differences remain: 83 of 124,267 prefix
  entries, and 2 of 1,280,779 romanizations. Accuracy on dakshina-dev is *identical to full
  floating-point precision* on all five metrics across 9,279 words, so neither difference has any
  effect.
- **The Python builder was not reproducible.** `build_lexicon.py` iterates a `set` to build its key
  items, and CPython randomises string hashing per process -- verified directly, three runs give
  three orders. That is the source of the 83 prefix entries. The Rust builder is deterministic.
- **Three tie-breaks had to be reproduced, not tidied.** Python resolves ties by insertion order
  (`Counter.most_common`, and `sorted` being stable). Using lexicographic order instead silently
  changed the chosen spelling of 117 words, 9,571 prefix entries and 12,850 bigrams. Insertion
  order is now tracked explicitly wherever a tie is decided.
- **Romanization scan order is a free choice, and the first claim here was wrong.** An earlier
  version of this entry said 7 points of top-1 depended on marisa's LOUDS enumeration order. That
  was a misdiagnosis: the builder had not carried `weights.json` forward, so the engine fell back
  to untuned defaults (unigram 1.0 against 0.5, key_fine 1.2 against 2.0, oov -3 against -6). The
  giveaway, missed for an hour: two completely different orderings measured 63.50107394906413 --
  identical to fourteen figures. Order has no measurable effect. It is now attestation count
  descending, chosen for being defined rather than an artifact of trie layout, and the builder
  refuses to start without the weights file rather than warning after the fact.
- **One visible behaviour change.** For `screensaver`, three candidates tie at exactly
  -17.591683475492697 and the scan order decides which comes third and which fourth. That is 6 of
  2,400 golden cases; the goldens were regenerated from the Python engine reading the new tables,
  not from the Rust output, so Python remains the oracle.
- **`scripts/build_runtime.py` is gone.** It packaged the embedded CPython runtime that 0.3.0
  stopped shipping.

## 2026-09-18: The shipped engine is Rust; the Python stays as the reference

- **Context.** The engine was the last Python in the product: an embedded CPython with NumPy,
  marisa-trie and the Likhi package, about 65 MB, taking 564-630 ms to start.
- **How correctness was established.** Not by review. `scripts/dump_goldens.py` records what the
  Python returns for ~96,500 calls and `engine/tests/goldens.rs` requires the Rust to return the
  same thing. String and integer results match exactly; the transformer's float32 outputs are
  checked on *ranking* -- same words, same order -- with scores inside 1e-5, because a different
  summation order in a matrix product cannot be eliminated and does not matter at the scale
  candidates are separated by. `suggest` and `fast_suggest` are the acceptance tests: they are what
  the text service calls.
- **marisa's iteration order had to be reproduced to keep the port exact.** `core.py` inserts
  candidates in the order the trie yields them and then sorts stably, so equal scores are broken by
  that order -- and marisa enumerates a LOUDS traversal, not lexicographic: for "screensaver" it
  returns স্ক্রিনসেভারের before its own prefix স্ক্রিনসেভার. Eleven percent of sampled prefixes
  differ from key order. Each entry in `romans.lkx` therefore carries its position in that
  enumeration; without it, 3 of 2400 `suggest` cases differed.
  *(Later correction, 2026-09-18: the order turned out to be free. Replacing it entirely leaves
  accuracy identical to full precision on dakshina-dev, so reproducing it mattered for making the
  port provably exact, not for quality. See the 2026-09-18 entry above.)*
- **Footprint is a trade, not a win on every axis.** marisa is a compressed trie and `.npz` is a
  zip; the replacements are laid out to be memory-mapped rather than unpacked, so they are larger
  at rest and smaller after compression. Measured: installer 57.8 -> 39.2 MB, installed 110 -> 136
  MB, resident memory 121.7 -> 61.6 MB, and private (unreclaimable) memory 236.1 -> 5.8 MB. The
  last number is the one that matters on the 4-8 GB office machines this runs on: nearly all of the
  Rust engine's memory is file pages the OS can evict. That is why the model is not stored
  compressed, which would save 25 MB of disk and make all 45 MB resident.
- **A shared word table was measured and rejected.** The obvious way to shrink the tables is to
  store each Bengali word once and reference it by id. Measured: 1,122,197 distinct romans for
  1,280,777 entries (1.14 words per roman) and 1,240,052 distinct words, so there is very little
  reuse to exploit -- projected saving about 26 MB for a redesign of the data layer. Not taken.
- **What stays in Python.** `src/likhi/eval` and `src/likhi/data`: the research harness and the
  dataset builders, where Python is the right tool and nothing ships. `src/likhi/` remains the
  reference implementation and is what the goldens are generated from.
  *(Superseded 2026-09-18 by the entry below: the harness was ported too and the Python deleted.)*

## 2026-09-16: Transliteration model runs in pure NumPy, not CTranslate2

- **Context.** IndicXlit (11M-parameter character transformer, MIT) is the generative channel.
  The plan was to convert it to CTranslate2 for fast CPU inference.
- **What happened.** The conversion works (float32 47 MB, int8 13 MB), but the CTranslate2 4.8.2
  Windows wheel crashes with an access violation when loading *any* model on the development
  machine (i9-10900K, Windows 11 26200), inside and outside the tool sandbox, with single-thread
  OpenMP, and with `CT2_FORCE_CPU_ISA=GENERIC`. A 60 MB native dependency that can crash per-CPU
  is also a poor fit for a keyboard that runs inside every user's session.
- **Decision.** `likhi.engine.xlit_np` implements the model with NumPy only (about 300 lines):
  faithful fairseq pre-norm transformer, sinusoidal positions with padding offset, exact-erf GELU,
  beam search with length normalization. Weights ship as a 21 MB float16 `.npz`.
- **Consequences.** Word decode is ~30 ms at beam 5 today; the engine will cache by prefix and
  only call the model when the lexicon is unsure, and the code is easy to optimize or distil.
  CTranslate2 remains available behind the `ct2` extra for machines where it works.

## 2026-09-16: Ranker weights are chosen on real typing data, synthetic habits second

- The habit-augmented tuner produced weights that were much better on synthetic vowel-dropping
  (+12 points) but worse on real chat words (-2) and on Khaled's reported words (14/17 vs 17/17).
  The cause is structural: shorthand makes the model read acronyms, so the tuner learned to mute
  the model, which hurts every model-driven case (loanwords, dialect verbs).
- Two targeted fixes recovered most of it without muting the model: acronym readings (segments of
  Bengali letter names) no longer get the confident-unknown-word relief, and the semivowel য়
  never creates a consonant slot in the phonetic key (koria/koriya/korea all reach কোরিয়া).
- Deployed "blend C": retuned key weights (2.0/1.2), attested-spelling weights (2.5 + 0.8·log),
  model vote restored (top-1 bonus 1.0, log-prob 0.5). Selection table in STATUS.md.
- Rejected: gating weights by vowel ratio of the typed string; it does not separate shorthand from
  English loanwords ("volt", "chat").

## 2026-09-16: Beam search finalization follows fairseq exactly

- Khaled's word "khacche" exposed a beam-search bug: the model's own best hypothesis খাচ্ছে was
  missing at beam 4 (present at beam 5). Cause: any end-of-word candidate among the top 2×beam
  was finalized, so weak short words filled the finished list and stopped the search before the
  longer correct word completed. fairseq finalizes only EOS candidates ranked within the top
  `beam`. Fixed in `xlit_np.beam_search`; a speculative early-stop shortcut was removed as well.
- Lesson: keep a few real typing reports as a committed regression set (`data/feedback/words.jsonl`,
  eval set `feedback-words`); one word found a bug that thousands of benchmark items hid.
- Same session, "khaitecho": the beam's own hypotheses never received a model score, because the
  pre-ranking that chooses which 16 candidates to score penalizes unscored words. Beam outputs
  now carry their un-normalized beam log-prob directly, and only other candidates are scored.
  The unknown-word penalty and the unigram floor are also scaled down when the model is confident
  (log P > -3), so dialect forms the lexicon lacks but the model knows (খাইতেছো) can win.

## 2026-09-16: Never use `\W` on Bengali text

- A punctuation stripper written as `[\W_]+$` silently removed final vowel signs, virama, nukta
  and candrabindu from Bengali tokens, because Python's `\w` excludes combining marks. It
  corrupted the aligned BanglaTLit word pairs (golds like কিন্ত for কিন্তু) and, through them, the
  chat romanizations in the lexicon ("korci" → করছ). Caught by reading eval misses.
- Rule: tokenizers and strippers treat the whole Bengali block U+0980–U+09FF plus ZWJ/ZWNJ as
  word characters (`tests/test_datasets.py` guards this). Results computed before the fix were
  deleted from `results/` and re-run.

## 2026-09-16: onnxruntime is out too (for now)

- onnxruntime 1.30.0 crashes with an access violation at import on the same machine, in the uv
  venv, in a plain `python -m venv`, and with the base interpreter. torch 2.14 and CTranslate2
  4.8.2 fail the same way at DLL load, while NumPy (OpenBLAS) and marisa-trie work. No system
  exploit-protection mitigations are set. Whatever the cause on this box, three independent
  native ML runtimes failing is a good reason to keep the shipped engine NumPy-only and treat
  faster runtimes as optional accelerators detected at start-up, never as requirements.

## 2026-09-16: No torch anywhere

- The fairseq checkpoint is read by `scripts/fairseq_ckpt.py`, a small unpickler that maps torch
  storages to NumPy arrays. torch 2.14's Windows wheel also failed to load on the dev machine
  (`c10.dll` initialization error), which made this the reliable path as well as the light one.

## 2026-09-16: Evaluation views

- Word sets exist in two views: `pairs` (one item per attested roman/native pair, unweighted,
  comparable to published numbers) and `grouped` (one item per roman string, all attested
  natives accepted, attestation-weighted, closer to what a typist experiences). The headline
  comparison against IndicXlit uses `pairs`; gates on the personal set use `grouped`.
- BanglaTLit word pairs come from positional alignment of same-length sentences. It is noisy
  (a few golds are misaligned) but it is the only public Bangladeshi chat-register word data.

## 2026-09-16: Repository and licensing

- Code MIT. Derived data from CC BY-SA sources (Dakshina, Wikipedia, OpenSubtitles) will be
  released as separate files under CC BY-SA 4.0. GPL word lists (hunspell bn_BD) and data of
  unclear license (Avro/riti dictionary) are not bundled.
- Repo lives outside the OneDrive junction (`C:\Users\khaled\src\likhi`); datasets and models are
  git-ignored and fetched by `scripts/fetch_datasets.py`.
