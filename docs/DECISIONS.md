# Decision log

Short records of choices that are not obvious from the code. Newest first.

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
