# Romanized-Bangla → Bengali transliteration: open data & models (verified 2026-09-15)

Everything below was checked against the live page/PDF/API on 2026-09-15 unless marked UNVERIFIED.

## 1. Dakshina (Google Research, 2020)

- Contents/splits (bn): native-script Wikipedia (train/valid by page), romanization lexicon, romanized Wikipedia sentences. Lexicon TSV = `native word ⇥ romanization ⇥ attestation count`. 25,000 native word types train, 2,500 dev, 2,500 test; no dev word shares a lemma with train. Sentences: 10,000 validation-set sentences romanized by native speakers, halved into dev/test. Paper Table 1 (bn): 895.1K Wikipedia training sentences, 3.8 romanizations/type, 12.1 native words/sentence.
  - https://github.com/google-research-datasets/dakshina ; paper https://aclanthology.org/2020.lrec-1.294.pdf
- Exact bn counts (pulled from the tarball): `bn.translit.sampled.train.tsv` 94,546 pairs over 25,000 types, 132,288 attestations; dev 9,279 pairs / 2,500 types; test 9,228 pairs / 2,500 types. `bn.romanized.rejoined.tsv` = 10,000 sentence pairs; `bn.romanized.rejoined.aligned(.cased_nopunct).tsv` = 130,378 aligned word pairs. Tarball 2,008,340,480 bytes.
  - https://storage.googleapis.com/gresearch/dakshina/dakshina_dataset_v1.0.tar
- Romanization style: elicited from native-speaker annotators ("How would you write this word in the Latin script?" + "other ways"); attestation count = number of annotators, not usage frequency. Sentence annotators: "as they would if writing in Latin script". Words sampled from Wikipedia (frequency > 1): formal vocabulary, not chat. All sentences NFC-normalized. Annotator country not stated (UNVERIFIED BD vs IN).
- License: dataset CC BY-SA 4.0.
- Baselines (paper, bn, dev set, mean of 5 runs):
  - Single-word Latin→native (Table 2): pair 6-gram FST CER 14.2 / WER 54.0; transformer CER 13.2 / WER 50.6 (best); LSTM CER 13.9 / WER 54.7.
  - Full-sentence Latin→native WER (Table 4, whitespace eval): single-word pair6g 35.0, single-word transformer 32.5, noisy channel (pair6g + Katz trigram LM) 18.6, whitespace transformer 19.7.

## 2. Aksharantar + IndicXlit (AI4Bharat, 2022)

- Dataset (bn): train 1,231K pairs, validation 11K, test 5,009 = AK-Freq 1,071 + AK-Uni 1,198 + AK-NEF 1,059 + AK-NEI 1,681. Sources (thousands): existing 104 (Dakshina 95, FIRE-2013 5, BrahmiNet 8), Wikidata 107, Samanantar-mined 193, IndicCorp-mined 1,115, manual 14. Mined pairs are mostly named entities/loanwords from Indian news. Test set = manual data, no word overlap with Dakshina test.
- Licenses: models MIT; benchmark + manually created data CC-BY; mined data CC0. HF id `ai4bharat/Aksharantar` (viewer currently broken).
  - https://huggingface.co/datasets/ai4bharat/Aksharantar ; https://arxiv.org/abs/2205.03018
- IndicXlit model: character-level multilingual transformer, 6+6 layers, d=256, FFN 1024, 4 heads, 11M params; fairseq; beam 4; top-4 rescoring with word-unigram LM (α=0.9).
- Accuracy (Top-1): Dakshina test bn 55.49 (Roark 49.40); with unigram re-ranking 69.41. Aksharantar test bn without→with reranking: AK-Freq 63.03→79.74; AK-Uni 60.47→69.22; AK-NEF 36.43→36.31; AK-NEI 40.50→43.14; micro-avg 54.06→65.90. Top-3 75.7, Top-5 81.6 on Dakshina bn (no rerank); Aksharantar top-5 89.6/80.9/61.3/68.6.
  - https://github.com/AI4Bharat/IndicXlit ; https://aclanthology.org/2023.findings-emnlp.4.pdf
- Package: `pip install ai4bharat-transliteration` 1.1.3 (2022-09-14), pure-python wheel depending on fairseq/torch/flask/etc. API: `XlitEngine("bn", beam_width=10, rescore=True).translit_word("amr", topk=5)`.
- Maintenance: repo last push 2023-10-13, MIT, 25 open issues. Windows: fairseq 0.12.2 (2022) has wheels only for cp36–cp38 manylinux/macOS; Windows install issues #21/#25/#31 still open. Use WSL/Docker/Colab or convert to CTranslate2.

## 3. Chat-style Banglish→Bangla data (Bangladesh)

- DL Sprint 3.0 (Kaggle `dlsprint3`) is Bengali math QA, NOT transliteration. No Bengali.AI transliteration contest found.
- BanglaTLit (EMNLP 2024 Findings): 42,705 human back-transliterated pairs (train 38,705 / val 1,500 / test 2,500), ≈10 words each, from TrickBD (35,613), Facebook, YouTube, blogs, Wikipedia; 12 native annotators; annotator–expert BLEU 72.55. BanglaTLit-PT: 245,727 unlabeled romanized texts. Best test BLEU 36.07 / ROUGE-L 79.75 (TB-XLM_R+BanglaT5_NMT); GPT-4 Turbo 0-shot 26.56. MIT (repo + HF). HF `aplycaebous/BanglaTLit` train split = PT corpus; annotated pairs are in the GitHub repo.
  - https://aclanthology.org/2024.findings-emnlp.859.pdf ; https://github.com/farhanishmam/BanglaTLit ; https://huggingface.co/datasets/aplycaebous/BanglaTLit
- SKNahin/bengali-transliteration-data: 5,006 sentence pairs (TrickBD-style), no license. https://huggingface.co/datasets/SKNahin/bengali-transliteration-data
- nahidstaq/bangla-transliteration-data: 45,675 pairs, no license, provenance unstated (likely BanglaTLit-derived, UNVERIFIED).
- Shibli et al. 2022 (Iran J. Comput. Sci.): 9-tool pipeline, BLEU-1 81.28 / WER 29.21 on 1,000 texts; releases 1K + 5K human-annotated pairs (xlsx), no license. https://github.com/shahariar-shibli/Automatic-Back-Transliteration-of-Romanized-Bengali-to-Bengali
- arXiv 2511.22769 (Nov 2025): 975,215 synthetic (rule-romanized bnwiki) sentence pairs, MIT, HF `sk-community/romanized_bangla`. Pre-training only.
- Other HF: `kawsarahmd/banglish_dataset_v3` (224,913 monolingual Banglish texts, no license); `ShayonSarker/Bengali-to-Banglish-Dataset` (206,926 generated word variants, MIT); `istiaqfuad/bangla-english-banglish-pairs` (2.4M LLM-generated triplets, CC-BY-4.0).

## 4. Lexicon / LM corpora (offline)

- IndicCorp v2: bn 926M tokens; CC0; HF `ai4bharat/IndicCorpV2` (`data/ben_Beng`).
- OSCAR 23.01: bn 3.47M docs / 1.09B words / 19.1 GB; gated.
- CC-100: `bn.txt.xz` 860 MB.
- Bengali Wikipedia: 191,343 articles; dump 2026-09-01 541 MB bz2; CC BY-SA.
- wordfreq 3.1.1 (2023-11-21): bn "large" list; data frozen at 2021; CC BY-SA 4.0 data.
- Leipzig: `ben_community_2017` (1.2M sentences, 645k types), `ben_newscrawl_2014_300K`, `ben_wikipedia_2018_300K`; CC BY.
- hermitdave FrequencyWords 2018/bn: `bn_full.txt` 99,993 words (OpenSubtitles); CC-BY-SA-4.0. https://github.com/hermitdave/FrequencyWords/tree/master/content/2018/bn
- hunspell bn_BD (LibreOffice): 87,639 entries, GPL-2. https://github.com/LibreOffice/dictionaries/tree/master/bn_BD
- OpenBangla riti data: `dictionary.json` 4,089,797 B = 159,426 words in 47 phonetic buckets; `autocorrect.json` 5,314 entries in Avro phonetic encoding; `suffix.json` 737. riti MPL-2.0 (last push 2026-07-06); data license UNVERIFIED (from OmicronLab). https://github.com/OpenBangla/riti/tree/master/data
- `bengaliAI/bethik-lexicon` (2026-07): ~667 MB word list + grapheme→Latin map, license unknown.

## 5. Unicode pitfalls (Bengali)

- Composition exclusions: U+09DC ড়, U+09DD ঢ়, U+09DF য় never appear in NFC/NFD. NFC(U+09DC) = U+09A1 U+09BC. Avro emits precomposed forms; Dakshina is NFC. Normalize both sides.
- Combining classes: nukta ccc=7, virama ccc=9 reorder canonically; but candrabindu/anusvara/visarga and vowel signs are ccc=0, so `কাঁ` ≠ `কঁা` under NFC. Core spec: candrabindu comes at the end of the sequence.
- ZWJ/ZWNJ: র‍্য = <U+09B0, U+200D, U+09CD, U+09AF>; ZWNJ after hasant forces visible hasant. Joiners survive NFC; canonicalize before lexicon matching.
- Khanda ta: U+09CE (older texts: <U+09A4, U+09CD, U+200D>).
- Two-part vowels compose under NFC (ে+া → ো U+09CB). Assamese ৰ/ৱ look-alikes.
- Sources: https://www.unicode.org/Public/UCD/latest/ucd/CompositionExclusions.txt ; https://www.unicode.org/reports/tr15/ ; https://www.unicode.org/versions/Unicode15.1.0/ch12.pdf ; https://www.unicode.org/faq/bengali.html

## UNVERIFIED / could not find
- Dakshina annotator nationality; IndicXlit CPU latency; any Bengali.AI Banglish contest; licenses of nahidstaq/tensorlabco/SKNahin sets; hunspell bn_BD authorship; separate license of riti/Avro dictionary data.
