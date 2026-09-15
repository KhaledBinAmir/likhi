# Transliteration keyboards and fuzzy/learning Indic input engines (verified 2026-09-15)

## 1. Google / Gboard, Apple, SwiftKey

Hellsten et al. 2017, "Transliterated mobile keyboard input via weighted finite-state transducers", FSMNLP 2017 (https://aclanthology.org/W17-4002.pdf):
- Transliteration model = pair language model (joint multigram, Bisani & Ney 2008 / Phonetisaurus lineage). EM aligns native codepoints to Latin letters with multi-symbol merges; at most one insertion/deletion in a row. n-gram over pair symbols trained with OpenGrm, encoded as a WFST with backoff arcs.
- Training data: for each of 150k vocabulary words, romanizations from ~5 speakers. LM: pruned trigram, 750k n-grams.
- `T = I ∘ P ∘ O`, input side determinized with auxiliary symbols; weight pushing of transliteration costs. `L ∘ G` built as character trie per LM state, 8-bit quantized, LOUDS-encoded (Hindi 85.3 MB → 6.72 MB).
- Joint→conditional normalization: divide each word's score by the sum over paths emitting it, giving a "conditional lexicon" L′ so T ∘ L′ ∘ G is a proper joint.
- Decoding: static C ∘ T (~100k arcs, 1.7 MB) composed on the fly with static L ∘ G (~3M arcs, 6.7 MB). Constraints: ≤20 ms latency, ~10 MB models.
- Results (blind test, >3000 words per modality/language): Hindi WER 16.4 vs 19.5 baseline HMM; latency/char 0.5 ms vs 3.0; Tamil 22.6 vs 26.2. Launched for 22 languages incl. Bengali in H1 2017.
- N-best presentation, raw-Latin candidate, and personalization are not described in the paper (only the generic rule that the literal wins unless a candidate beats it by a margin).
- Blog: https://research.google/blog/the-machine-intelligence-behind-gboard/

Later Google work:
- Wolf-Sonkin et al. 2019 (Latin-script keyboards): pair 3-gram models pruned to 110k n-grams; ~110k Hindi words × 3.1 romanizations; Hindi WER 22.6% → 10.5% at ~1.0 ms/tap, ~11 MB; notes personalization needs extra integration. https://aclanthology.org/W19-3114.pdf
- Dakshina LREC 2020: baselines pair 6-gram (Witten-Bell), LSTM, small transformer.
- Google Maps blog 2021-01-22: ensemble of an FST model "trained similarly to Gboard's on-device transliteration models" + LSTM; Gboard on-device was still FST-based as of early 2021. https://research.google/blog/improving-indian-language-transliterations-in-google-maps/
- Kirov et al., Computational Linguistics 50(2), 2024: pair 6g + LSTM + transformer + fine-tuned ByT5 ensembles, FST 4-gram KN LMs. https://aclanthology.org/2024.cl-2.2.pdf
- No Google publication on a neural on-device Gboard Indic transliteration model was found.

Apple: tech specs list "Bangla (Alphabetic, InScript, Transliteration)" for QuickType and autocorrection (https://support.apple.com/en-us/121032); iOS 18 multi-language Indic keyboards (https://support.apple.com/en-in/121233). No technical publication.
SwiftKey: transliteration for 12 languages incl. Bangla, Android only; "will still learn words you type … but currently will not learn new transliteration maps". https://support.microsoft.com/en-us/topic/which-languages-support-transliteration-and-how-does-it-work-in-microsoft-swiftkey-for-android-00819751-f597-4f17-b38c-3d2e1a8c946e

## 2. Open-source engines

- OpenBangla Keyboard (GPL-3.0; Linux IBus/Fcitx; release 2.0.0 2020-10-01; last commit 2026-05-10): Avro Phonetic + dictionary prediction, user-editable autocorrect, fixed layouts; typed English word added as last suggestion. https://github.com/OpenBangla/OpenBangla-Keyboard
- riti (Rust, MPL-2.0, last commit 2026-07-06): deps `okkhor` (rule-based Avro parser, MIT), `upodesh` (dictionary suggester, MIT, FST-based, 21×–58× faster than regex), `regex`, `edit-distance`. `src/phonetic/suggestion.rs`: autocorrect first; dictionary candidates from `upodesh::avro::Suggest`; suffix handling; rule-based okkhor output always appended; raw typed English last when `suggestion_include_english`; per-word cache; user selection memory = `HashMap<String,String>` of previous selections, `get_prev_selection()` pre-selects the previously chosen candidate (persistence in frontend). https://github.com/OpenBangla/riti ; https://github.com/OpenBangla/upodesh
- Classic Avro dictionary search (ibus-avro, MPL-1.1): `dbsearch.js` picks tables by first letter ('a' → a, aa, e, oi, o, nya, y; 'c' → c, ch, k; 's' → s, sh, ss), builds `'^' + AvroRegex(input) + '$'` over each table. `avroregexlib.js` replaces each Roman chunk by an alternation over Bengali codepoints, e.g. `"a" → "(([অএ]্যা?)|[অআএ]|([‍‌]?(্য)?া)|(য়া))"`, `"kh" → "(খ|(ক্ষ)|(ক(্?)(হ|ঃ|(হ্‌?))))"`. `suggestionbuilder.js` merges dictionary + rule output + autocorrect + suffixes, sorts by Levenshtein distance to the rule-based conversion, persists `_candidateSelections` to `~/.candidate-selections.json`.
  - https://raw.githubusercontent.com/omicronlab/ibus-avro/master/dbsearch.js ; avroregexlib.js ; suggestionbuilder.js
- Varnam / govarnam (Go, AGPL-3.0, v1.9.1 2024-04-07): learns words from user/corpora with frequency-based confidence; `-train pattern=word`; export/import VLF; Bengali scheme exists (`schemes/bn`); Linux IBus, macOS, Android; Windows "Coming Soon" (varnam-windows = ime-rs fork, tag v1.0.0 2023-12 without assets). https://github.com/varnamproject/govarnam ; https://varnamproject.com/docs/learning/
- libindic indic-trans (AGPL-3.0, 16 languages incl. Bengali, "ML system" + rule fallback, beam k-best; model type UNVERIFIED). Indic Keyboard (Android, Apache-2.0) engine not named. Ridmik-Parser (BSD-3, rule-based). Lipika: rule-based schemes, no learning (UNVERIFIED, repo 404).

## 3. Algorithm families and Bengali numbers

- Dakshina dev, single word Latin→bn, WER% (top-1 acc): pair 6g 54.0 (46.0), transformer 50.6 (49.4), LSTM 54.7 (45.3); CER 14.2/13.2/13.9.
- Kirov 2024 single-word bn CER: pair 6g 14.3, transformer 13.0, LSTM 13.7, ByT5 12.4; ensemble macro CER 10.4. Full-sentence bn WER: non-contextual ensemble 30.9 → +word LM 20.6 → context ensemble 14.8 (dev); test 20.5 / 15.0.
- IndicXlit: Dakshina bn top-1 55.4, top-3 75.7, top-5 81.6; with unigram-LM reranking top-1 69.4. Aksharantar bn test top-5 (no rerank) 89.6/80.9/61.3/68.6 across Freq/Uni/NEF/NEI.
- 2025 LLM benchmark: Dakshina bn GPT-4o 62.54% vs IndicXlit 55.26%. https://arxiv.org/pdf/2505.19851
- NADIR (Jan 2026), Aksharantar bn: IndicXlit CER 15.01 / word acc 52.29 / 189.94 s per test set (CPU?); NADIR 18.06 / 46.29 / 14.11 s. https://arxiv.org/pdf/2601.12389
- Sentence-level: BanglaTLit best BLEU 36.07; LREC 2026 bn: IndicXlit word-by-word WER 0.55 vs IndiXform 0.24, mT5 0.20. http://www.lrec-conf.org/proceedings/lrec2026/pdf/2026.lrec2026-1.61.pdf
- Toolkits: Sequitur G2P, Phonetisaurus, m2m-aligner, DirecTL+.

## 4. Python-on-Windows-CPU status

- pynini 2.1.7: PyPI manylinux only; conda-forge has win-64 builds (2025-11-07, py3.10–3.13). https://anaconda.org/conda-forge/pynini/files
- sequitur-g2p 1.0.1668.30 (2024-07-03): win_amd64 wheels cp39–cp312. https://pypi.org/project/sequitur-g2p/
- Phonetisaurus: no Windows.
- onnxruntime 1.30.0: win_amd64; BeamSearch contrib op for enc-dec.
- CTranslate2 4.8.2: win_amd64 py3.9–3.14; `ct2-fairseq-converter` supports `transformer` (IndicXlit is a standard fairseq transformer; conversion UNVERIFIED). CPU int8 ~3× faster than PyTorch. https://opennmt.net/CTranslate2/guides/fairseq.html
- marisa-trie 1.4.1 (2026-04-08) win_amd64; DAWG2 0.13.3; symspellpy 6.10.0 (pure Python).

## UNVERIFIED / could not find
- Gboard n-best/raw-Latin/personalization details; any Google neural on-device Indic transliteration; Apple/SwiftKey internals; CTranslate2 conversion of IndicXlit and its measured latency; DirecTL+/Phonetisaurus Bengali numbers.
