# Decision log

Short records of choices that are not obvious from the code. Newest first.

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
- **marisa's iteration order had to be reproduced, not improved.** `core.py` inserts candidates in
  the order the trie yields them and then sorts stably, so equal scores are broken by that order --
  and marisa enumerates a LOUDS traversal, not lexicographic: for "screensaver" it returns
  স্ক্রিনসেভারের before its own prefix স্ক্রিনসেভার. Eleven percent of sampled prefixes differ from
  key order, and the order also decides which candidates are cheap enough to send to the model, so
  it is not cosmetic. Each entry in `romans.lkx` carries its position in that enumeration. Without
  it, 3 of 2400 `suggest` cases differed.
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
