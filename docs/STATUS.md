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
| IndicXlit, NumPy port, beam 5 (before the beam fix) | 57.9 | 75.7 | 79.9 | 12.5 | | |
| IndicXlit, NumPy port, beam 5, corrected finalization | **59.6** | 79.5 | **84.6** | 12.5 | | |
| IndicXlit + word-frequency rerank | 69.1 | 78.9 | 79.9 | 9.5 | | |
| Likhi v0 (hand-set weights) | **70.6** | 82.6 | 83.7 | 11.2 | 31.4 → 43.6 after the gap/raw-logp ranker fix (first 4000 pairs) | **89.9** (top-3 94.8, top-5 95.2, CER 5.4) |
| Likhi v0 (tuned weights, dev macro 68.1 → 70.1) | 70.5 | 81.2 | 82.3 | 11.3 | 42.9 (4000 pairs) | **90.5** (top-3 94.6, top-5 94.9, CER 5.2); val words 84.2 |
| Likhi v0 + beam-score fix + confidence-scaled OOV (weights not yet re-tuned) | 65.9 | **89.9** | **91.6** | 10.5 | **55.4** (all 9300 pairs; top-5 78.4) | 89.5 (top-5 95.4); feedback set 17/17 |

The last row trades Dakshina top-1 for much higher recall everywhere (top-5 +8 points on Dakshina,
Aksharantar top-1 +12); the weights were tuned before these features existed, so a re-tune on
habit-augmented dev data is running.

Published for reference: IndicXlit 55.4 top-1 (69.4 with rerank), Google 2020 transformer 49.4.

A first Likhi v0 run on chat words (61.2 top-1 / 86.0 top-5) was discarded: the aligned golds had
lost their final vowel signs to a `\W` regex (see DECISIONS.md). Everything downstream of the
BanglaTLit alignment was rebuilt.

Sentence-level word error (word-by-word, no context): Avro 61.5% on Dakshina test, 65.7% on
BanglaTLit test (fixed tokenizer). Likhi v0 with bigram context: being computed.

Latency (this i9-10900K, single thread): IndicXlit beam 4 ≈ 45 ms/word; Likhi v0 ≈ 100 ms/word
(16 model-scored candidates). Stage 2 target is p95 ≤ 15 ms.

## Feedback from the first typing session (2026-09-16)

Instant, English loanwords excellent (পার্টিসিপেশন, ইন্টারন্যাশনাল, হিউমিডিটি, স্ক্রিনসেভার), Bangla
digits fine, Space/Enter behave as intended. Reported misses are in `data/feedback/words.jsonl`
(eval set `feedback-words`). Backlog created from the session:

- Ranking: "koria/korea" gave করে first (phonetic-key + frequency beat the model's কোরিয়া); the
  weight tuner and personal learning both address this.
- Candidate window: PIME's built-in Win32 window; a modern picker and correct composition text
  colour in dark-mode apps need our own renderer or libIME2 changes (Stage 3 polish).
- Next-word prediction when the buffer is empty (we already have bigram tables) — Stage 3.
- Windows resets the input method per app window by default; set Likhi as the default input
  method (done via `Set-WinDefaultInputMethodOverride`) or turn off "Let me set a different
  input method for each app window" in Settings → Time & language → Typing → Advanced keyboard
  settings.

## Backlog: community data (post v0.1, opt-in only)

Collect only struggle events, never accepted first suggestions: (a) user picked candidate 2..5
(record roman, chosen word, and the word we ranked first); (b) user backspaced and retyped the
same word with another spelling (record abandoned spelling, final spelling, chosen word); (c) raw
Latin committed and later typed in Bangla. Word level only; drop anything with digits/symbols,
secure fields, context, timestamps, identifiers. Local name filter (lexicon + model confidence),
review-before-send, one-click delete, and a minimum-installs threshold on the server. First step
is a manual `likhi-collect export` people can post consciously.

## Next

1. Read the Likhi v0 numbers, tune weights on the dev caches, re-measure (Stage 1 gate).
2. Sentence-level evaluation with the bigram context.
3. Latency: fewer NumPy ops per decoder step, prefix caching, or an onnxruntime graph built
   without torch if onnxruntime proves stable on this machine.
4. Khaled: personal test set; PIME smoke test; `gh auth login` so the repo can be pushed.
