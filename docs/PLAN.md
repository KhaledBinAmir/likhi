# Gboard-style Bangla phonetic input for Windows: research summary and plan

Date: 2026-09-15. Status: proposal awaiting Khaled's approval. Research notes with sources are in `research/01..03-*.md`.

## 1. What the research found

### How Gboard-style transliteration keyboards work
- Gboard (Hellsten et al., 2017): a pair n-gram transliteration model learned from ~5 human romanizations per word for 150k words, compiled to a weighted finite-state transducer, composed on the fly with a word lexicon and a pruned trigram word language model. Constraints were ≤20 ms per key and ~10 MB of models. Hindi word error rate 16.4%. The "literal" typed string wins unless a candidate beats it by a margin. Personalization and n-best UI are not described in the paper. As of 2021 Google's on-device transliteration was still FST-based; no neural on-device publication exists.
- Apple and SwiftKey publish nothing technical. SwiftKey learns new words but not new romanization maps.
- Takeaway: the product is transliteration model × lexicon × word LM × personalization. The transliteration model alone is not enough.

### What accuracy is achievable for Bengali (published)
| Setting | Best published | Source |
|---|---|---|
| Isolated word, Dakshina bn, top-1 | 55.4% (IndicXlit); 69.4% with word-frequency rerank | Aksharantar paper 2022/2023 |
| Isolated word, Dakshina bn, top-5 | 81.6% | same |
| Sentence context, Dakshina bn, word error | 33% no context; 18.6% with trigram LM; 15.0% contextual ensemble (test) | Dakshina 2020; Kirov 2024 |
| Gboard Hindi, 2017 | 16.4% word error | Hellsten 2017 |

Isolated top-1 is capped by genuine ambiguity of romanizations; frequency and context add 15–20 points. This is why rule-based tools cannot get there and why the engine must be lexicon- and LM-centric.

### Data available (license-checked)
| Resource | Bengali content | License | Use |
|---|---|---|---|
| Dakshina (Google) | 25k words, 94.5k roman pairs, 10k romanized sentences, 130k aligned word pairs | CC BY-SA 4.0 | eval + training; ShareAlike on derived data |
| Aksharantar (AI4Bharat) | 1.23M word pairs, 5k human test set | CC0 mined, CC-BY manual | training |
| BanglaTLit (EMNLP 2024) | 42.7k Bangladeshi chat sentence pairs + 245k unlabeled romanized texts | MIT | chat-style tuning + eval; Banglish LM |
| IndicXlit model | 11M-param char transformer, 82% top-5 on Dakshina | MIT | generative channel via CTranslate2 |
| IndicCorp v2 | 926M tokens | CC0 | word frequencies, bigram LM |
| OpenSubtitles bn frequency list | 100k words | CC BY-SA 4.0 | conversational frequencies |
| Bengali Wikipedia | 191k articles | CC BY-SA | LM |
| Shibli 2022, SKNahin | 5k + 5k chat pairs | none stated | eval only |
| hunspell bn_BD | 87.6k words | GPL-2 | do not bundle |
| riti/Avro dictionary | 159k words | unclear | diagnostics only until clarified |

### Existing projects
None combines a data-driven loose-romanization engine with system-wide Windows input and learning. Rule-based Windows tools: Avro (keyboard hook + SendInput, source frozen 2012), Borno (closed), Microsoft's Bangla Phonetic IME, BornoTSF (skeleton). Data-driven converters are web apps or cloud LLM calls (ShobdoSearch, AI_Power-Banglish-keyboard). OpenBangla Keyboard merged a Windows TSF port on 2026-07-12 (unreleased, still Avro rules). Varnam learns word weights and has a Bengali scheme but no Windows release.

### Windows integration
- Text Services Framework (TSF) is the only proper mechanism: composition string, candidate UI, works in Chromium/Electron (Chrome, VS Code, Slack, WhatsApp since July 2025), Windows Terminal and conhost (since 2024), Office. Registration needs admin once; the DLL loads in-process, so x86, x64 and ARM64 builds are needed.
- Keyboard hook + SendInput (Avro's way) has documented failure modes: 1000 ms hook timeout with silent removal, UIPI blocking elevated apps, antivirus flags, backspace-and-retype artifacts, garbled output in browsers on Windows 11.
- Shell options: PIME (C++ TSF DLL + Rust launcher + Python backend API; commits July 2026; last release Jan 2023 with Python 3.8; open Win11 24H2/Office/crash issues), RIME/Weasel (mature, out-of-process, config-driven, no runtime Python), OpenBangla Windows port (C++/Qt), Rust templates (ime-rs, hufu-ime-rust).
- Toolkits with Windows wheels: CTranslate2, onnxruntime, sequitur-g2p, marisa-trie, DAWG2, symspellpy. fairseq does not install on modern Windows Python; pynini only via conda-forge.

## 2. Proposed architecture

```
keystroke → TSF DLL (in the app) → named pipe → PIME launcher → Python backend
                                                                  ├─ shell adapter: composition, candidate list, hotkeys, commit rules
                                                                  └─ engine (pure Python package, platform-independent)
                                                                       ├─ normalizer: case folding, Unicode NFC, ZWJ handling
                                                                       ├─ candidate sources
                                                                       │    ├─ lexicon index: observed romanizations + normalized "shorthand" keys → words
                                                                       │    ├─ transliteration model: IndicXlit via CTranslate2 (later: own fine-tuned model)
                                                                       │    ├─ Avro-rule literal (deterministic fallback)
                                                                       │    └─ raw Latin (English passthrough)
                                                                       ├─ ranker: log-linear(translit score, unigram/bigram LM, personal history, source prior)
                                                                       └─ personalization store: SQLite (selection memory, personal n-grams, learned words)
```

Justification of each choice:
1. Engine as a pure Python package, separate from the shell. Testable offline against datasets; maintainable by Khaled; shell can be swapped (PIME, Weasel, OBK, IBus on Linux later).
2. Four candidate sources. Lexicon index gives completions and handles shorthand; the transliteration model handles unseen words; the Avro-rule literal guarantees any Bangla string is typeable; raw Latin gives English passthrough and lets learning turn "ok"/"bro" into Latin automatically.
3. IndicXlit first, own model later. It is MIT, the best open model, and 82% top-5 on Dakshina. Converting it once to CTranslate2 (in Colab/WSL) gives a fast int8 CPU model on Windows without fairseq. We train or fine-tune our own (sequitur pair n-gram or small transformer on BanglaTLit-derived word pairs) only if gates demand it.
4. Lexicon + LM from IndicCorp v2, OpenSubtitles, Wikipedia. This is where the published +15 points come from; bigram context disambiguates short function words (ki/ke/je/ar/kore).
5. Loose romanization: learned from real human romanizations (Dakshina, Aksharantar, BanglaTLit), plus a normalized shorthand key ("amr"→"amar"), plus learning from picks.
6. Personalization like Gboard's personal LM: (normalized roman → chosen word) counts with recency, personal unigrams/bigrams, learned OOV words. Stored locally, exportable.
7. English: hotkey toggle; raw Latin always present; tokens with digits, '@', '/', ':' pass through; Stage 2 adds a char n-gram Banglish-vs-English classifier trained on BanglaTLit-PT vs English text.
8. Punctuation and numerals: '.' → '।' after a Bangla word (not inside numbers/URLs; '..' gives '.'); digits → Bangla digits by default, Western optional; '?' '!' ',' unchanged.
9. Unicode: NFC internally; output precomposed nukta letters (ড় ঢ় য়) for compatibility with existing text; canonicalize ZWJ/candrabindu order for matching.
10. Shell: PIME primary (whole TSF stack exists; we write Python only), verified by a smoke test in Stage 0 on this Windows 11 26200 machine. Fallbacks: Weasel/RIME with a generated dictionary (no learning of our kind, no neural OOV, but immediately usable); OpenBangla's Windows TSF DLL with a pipe bridge; thin Rust TSF shell from ime-rs/hufu. Hook approach rejected.
11. Composition UX: typed Latin shown underlined; candidate window shows top 5 Bangla; Space/Enter commits #1; 1–5 or arrows select; Esc cancels; option to show live Bangla instead.

### Latency and memory budget
| Component | Budget |
|---|---|
| lexicon prefix lookup + scoring | ≤ 3 ms |
| transliteration model (beam 4, int8, cached per prefix) | ≤ 10 ms, invoked only when needed |
| engine total per keystroke | p95 ≤ 15 ms, p99 ≤ 30 ms |
| PIME IPC round trip | 1–3 ms |
| end-to-end key → candidates painted | p95 ≤ 40 ms |
| backend RSS | ≤ 250 MB |

## 3. How accuracy is measured

Datasets: Dakshina bn test (attestation-weighted), Aksharantar bn test (4 subsets), BanglaTLit test (word accuracy after alignment, plus BLEU/CER for comparability), Khaled's personal set (300 sentences ≈ 2,500 words, natural style, multiple acceptable golds, 150 dev / 150 test, ≥ 50 names/slang/loanwords). Baselines: Avro rules (avro.py), IndicXlit alone, IndicXlit + unigram rerank.

Metrics: top-1/3/5 accuracy, MRR, CER of top-1, OOV-subset accuracy, in-context word error rate, keystroke-replay curve (at which prefix length the right word reaches top-1/top-5), latency p50/p95/p99, and after launch the real top-1 commit rate from local logs.

| Metric | Dataset | Reference | Target |
|---|---|---|---|
| top-1 isolated | Dakshina bn test | 55.4 / 69.4 with rerank | ≥ 72 |
| top-5 isolated | Dakshina bn test | 81.6 | ≥ 92 |
| word error in context | Dakshina bn sentences | 18.6 / 15.0 SOTA | ≤ 18 |
| top-1 in context, before learning | personal set | Avro rules: to measure | ≥ 88 |
| top-5 in context | personal set | | ≥ 97 |
| top-5 on OOV subset | personal set | | ≥ 75 |
| top-1 commit rate after 2 weeks | usage logs | Gboard Hindi 2017 ≈ 84 | ≥ 93 |
| engine latency | keystroke replay | | p95 ≤ 15 ms |
| end-to-end latency | Notepad/Chrome | | p95 ≤ 40 ms |

"Daily use" in words: fewer than one non-top pick per 8 words before learning, one per 15 after; almost never letter-by-letter; no perceptible lag (threshold ≈ 70–100 ms).

## 4. Staged plan with gates

| Stage | Work | Gate |
|---|---|---|
| 0. Foundations, fail-fast (2–3 days) | repo outside OneDrive; git + uv; download datasets; eval harness; baselines (Avro rules, IndicXlit via CTranslate2); personal-set collection tool; PIME smoke test with a trivial backend | harness runs, baseline numbers recorded, PIME works or fallback chosen |
| 1. Engine v0 (1–2 weeks) | lexicon + frequencies, romanization index, IndicXlit channel, log-linear ranker, unigram LM, terminal typing simulator | Dakshina top-1 ≥ 65; personal top-1 ≥ 80, top-5 ≥ 93; p95 ≤ 20 ms |
| 2. Engine v1 (2 weeks) | bigram context LM, personalization, English detection, chat-style fine-tune, shorthand key tuning, punctuation/numerals, Unicode policy, latency work | all targets in §3 (the approach-works decision) |
| 3. Windows shell (2 weeks) | PIME backend adapter, candidate window, hotkeys, installer (Bengali/Bangladesh profile), app matrix: Chrome, VS Code, WhatsApp, Word, Slack, Windows Terminal, Notepad, Explorer, elevated PowerShell, a UWP app; local pick logging | works in all matrix apps; end-to-end p95 ≤ 40 ms; 3 crash-free days |
| 4. Dogfood and release (2 weeks) | daily use, weekly metrics, fix top error classes, settings, dictionary import/export, docs, CI with eval regression, GitHub v0.1 installer | commit rate ≥ 93 after 2 weeks; public release |

Fail-fast rules: if Stage 1 personal top-1 < 70, stop and analyze before building more; likely fixes are chat-style fine-tuning and shorthand keys; if Stage 2 misses targets after that, reconsider the model class (bigger neural, or WFST via conda pynini) before any UI work.

## 5. Risks and unknowns
1. Romanization style mismatch: Dakshina/Aksharantar are Indian Wikipedia/news style; only BanglaTLit is Bangladeshi chat. Mitigation: BanglaTLit for tuning, personal set as the gate, learning.
2. Ambiguity ceiling: isolated top-1 tops out near 70; context and personalization carry the rest.
3. PIME stability on Windows 11 24H2+ (issues #855/#856/#888/#840), 2023 release with Python 3.8; may need building from source (MSVC + Rust) or a fallback shell.
4. Toolchain gaps on this machine: no git, MSVC, Rust, WSL. Colab for the one-time IndicXlit conversion.
5. Python latency: PIME is synchronous per key; bounded beam, caching, warm-up, int8 required.
6. In-process DLL crash takes the host app down; robust backend and benign failure path needed; test elevated and UWP apps.
7. Admin installer, unsigned binaries: SmartScreen/AV friction for public users; SignPath for OSS later.
8. Unicode canonical form and rendering (nukta, ZWJ); decide early, test in Word/Chrome search.
9. Evaluation subjectivity: multiple valid spellings; allow alternates.
10. English passthrough misfires ("ok", "app"); softened by learning and the Stage 2 classifier.
11. Solo-maintainer scope; keep the shell thin.
Unverified: CTranslate2 conversion of IndicXlit; PIME launcher pointing at a custom Python 3.12; Bengali (Bangladesh) profile registration specifics; Dakshina annotator origin.

## 6. Licensing for public release
- Code: MIT or Apache-2.0 (Khaled's choice). PIME is LGPL-2.1: using its DLLs unmodified is fine; modifications to its C++ must be released under LGPL.
- Artifacts derived from CC BY-SA sources (Dakshina, Wikipedia, OpenSubtitles) released as separate data files under CC BY-SA 4.0 with attribution; IndicXlit (MIT), Aksharantar (CC0/CC-BY), BanglaTLit (MIT), IndicCorp v2 (CC0) are compatible.
- Do not bundle hunspell bn_BD (GPL-2) or the riti/Avro dictionary until its license is clarified. Do not use "Avro" in the name.

## 7. Decisions needed from Khaled
1. Approve the approach: engine-first, PIME as primary shell, Weasel/RIME as fallback.
2. Project name and license.
3. Personal test set: 300 sentences in natural style with gold Bangla; best source is real past Banglish chat messages plus Bangla written on the phone with Gboard.
4. Permission to install git and uv on this machine now; VS Build Tools and Rust only if PIME must be rebuilt.
5. Repo location, e.g. `C:\Users\khaled\src\<name>`, outside the OneDrive junction.
6. UX defaults: Latin vs live Bangla in the composition; Bangla digits by default; toggle hotkey.
