# Status

Updated 2026-09-16. Stage numbers refer to docs/PLAN.md section 4.

## Stage 0 (foundations, fail-fast) — done except the two items that need Khaled

| Item | State |
|---|---|
| Repo, tooling (git, uv), package skeleton, tests, lint | done |
| Datasets fetched: Dakshina bn, Aksharantar bn, BanglaTLit, FrequencyWords, bnwiki dump, IndicXlit | done |
| Evaluation harness (word top-k / CER / MRR / latency, sentence WER, pairs vs grouped views) | done |
| Avro rule-based baseline measured | done |
| IndicXlit running offline on Windows (pure NumPy; CTranslate2 crashed here) | done, matches paper |
| Personal test set (~300 sentences) | **waiting for Khaled** |
| PIME smoke test (needs admin install) | **passed**: PIME 1.3.0 + Likhi text service typing system-wide on Windows 11 26200; first-user verdict "70% Gboard feel" |

## Stage 1 (engine v0) — in progress

Built: lexicon (263k words, unigram counts from Wikipedia + OpenSubtitles + chat), romanization
index (1.28M attested roman→word pairs), fine/coarse phonetic keys (527k), IndicXlit generative
channel + teacher-forced candidate scoring, Avro literal, log-linear ranker, weight tuner,
personalization store, bigram context table, engine server, PIME text-service skeleton.

## Numbers so far (word level, `pairs` view unless noted)

| System | Dakshina test top-1 | top-3 | top-5 | CER | Aksharantar test top-1 | BanglaTLit chat words top-1 |
|---|---|---|---|---|---|---|
| Avro rules (what Windows users have today) | 17.9 | 17.9 | 17.9 | 32.8 | 12.0 | 47.5 (grouped) |
| IndicXlit, NumPy port, beam 5 | 57.9 | 75.7 | 79.9 | 12.5 | | |
| IndicXlit + word-frequency rerank | 69.1 | 78.9 | 79.9 | 9.5 | | |
| Likhi v0 (hand-set weights) | running | | | | 31.4 → 43.6 after the gap/raw-logp ranker fix (first 4000 pairs) | **89.9** (top-3 94.8, top-5 95.2, CER 5.4) |

Published for reference: IndicXlit 55.4 top-1 (69.4 with rerank), Google 2020 transformer 49.4.

A first Likhi v0 run on chat words (61.2 top-1 / 86.0 top-5) was discarded: the aligned golds had
lost their final vowel signs to a `\W` regex (see DECISIONS.md). Everything downstream of the
BanglaTLit alignment was rebuilt.

Sentence-level word error (word-by-word, no context): Avro 61.5% on Dakshina test, 65.7% on
BanglaTLit test (fixed tokenizer). Likhi v0 with bigram context: being computed.

Latency (this i9-10900K, single thread): IndicXlit beam 4 ≈ 45 ms/word; Likhi v0 ≈ 100 ms/word
(16 model-scored candidates). Stage 2 target is p95 ≤ 15 ms.

## Next

1. Read the Likhi v0 numbers, tune weights on the dev caches, re-measure (Stage 1 gate).
2. Sentence-level evaluation with the bigram context.
3. Latency: fewer NumPy ops per decoder step, prefix caching, or an onnxruntime graph built
   without torch if onnxruntime proves stable on this machine.
4. Khaled: personal test set; PIME smoke test; `gh auth login` so the repo can be pushed.
