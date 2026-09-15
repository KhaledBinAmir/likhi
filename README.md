# Likhi (লিখি)

**A Gboard-style Bangla phonetic input method for Windows.** Type Bangla the way you already
romanize it in chat, with no spelling rules to learn, and get the right word first, with a few
alternatives you can pick with a key. It learns from you, runs fully offline, and works in every
app through the Windows Text Services Framework.

> Status: **pre-alpha, engine under construction.** There is no installable keyboard yet.
> The core engine is being built and measured first; the Windows shell comes after the engine
> clears its accuracy gates. See [docs/PLAN.md](docs/PLAN.md).

## Why

Every existing Windows option (Avro Keyboard, Microsoft's Bangla Phonetic IME, Borno) is
rule-based: you must know the exact roman spelling convention, case rules, and conjunct tricks.
Type "amr", "aamar", "korci", or "jonne" and they produce the wrong word or nothing sensible.
Gboard and the macOS Bangla transliteration keyboard just understand you. Likhi brings that
experience to the Windows desktop, as open source.

## What it will do

- Loose, case-insensitive romanization: `amr`, `amar`, `aamar` all give আমার first.
- Top suggestion right the vast majority of the time; 2 to 4 alternatives selectable by number
  keys or arrows. Space or Enter commits the first.
- Reasonable output for words it has never seen: names, slang, English loanwords in Bangla script.
- Learns from you: keep picking the second suggestion and it becomes the first.
- System-wide: Chrome, VS Code, WhatsApp, Word, Slack, Windows Terminal.
- Imperceptible latency; everything runs locally on a normal laptop CPU.
- Quick Bangla/English toggle, English passthrough for words you clearly mean as English.
- Bangla punctuation (।) and numerals handled sensibly, Western numerals optional.
- Local-only learning data, no telemetry, exportable personal dictionary.

## How it works (short)

Likhi follows the recipe behind Gboard's transliteration keyboards: a transliteration model
that has learned from real human romanizations, combined with a Bangla word lexicon, a word
language model for context, and a personal model that learns from your choices. Details,
sources, and the accuracy targets are in [docs/PLAN.md](docs/PLAN.md) and
[docs/research](docs/research).

```
keystroke -> TSF text service (in the app) -> Python backend
             engine: normalize -> candidates (lexicon index | transliteration model | rule literal | raw Latin)
                     -> rank (transliteration score x language model x personal history) -> top 5
```

## Repository layout

```
src/likhi/engine/   core engine (pure Python, platform independent)
src/likhi/eval/     evaluation harness, metrics, baselines, personal test set tools
src/likhi/data/     data pipeline: lexicon, frequencies, language model, model conversion
scripts/            one-off scripts (dataset download, model conversion)
tests/              unit tests
docs/               plan, research notes, design decisions
data/               datasets (raw/processed are git-ignored; see scripts/fetch_datasets.py)
results/            evaluation results tracked over time
```

## Development

Requires Python 3.12 and [uv](https://docs.astral.sh/uv/).

```
uv sync --all-extras
uv run python scripts/fetch_datasets.py --all                      # datasets + IndicXlit checkpoint
uv run python scripts/convert_indicxlit.py --src data/raw/indicxlit --npz models/indicxlit-np
uv run likhi-data lexicon                                           # unigrams, romanizations, phonetic keys
uv run likhi-eval words --system likhi --dataset dakshina-test      # measure
uv run pytest
```

Baselines and results live in `results/` and are summarized by `uv run likhi-eval report`.

## Data and model licenses

Likhi's code is MIT. It builds on open data and models whose licenses are respected as follows:

| Resource | License | Use in Likhi |
|---|---|---|
| [Dakshina](https://github.com/google-research-datasets/dakshina) | CC BY-SA 4.0 | evaluation, training; derived data files released under CC BY-SA 4.0 |
| [Aksharantar](https://huggingface.co/datasets/ai4bharat/Aksharantar) | CC0 (mined) / CC-BY (manual) | training |
| [BanglaTLit](https://github.com/farhanishmam/BanglaTLit) | MIT | chat-style tuning and evaluation |
| [IndicXlit](https://github.com/AI4Bharat/IndicXlit) | MIT | transliteration model (converted to CTranslate2) |
| [IndicCorp v2](https://huggingface.co/datasets/ai4bharat/IndicCorpV2) | CC0 | word frequencies, language model |
| [FrequencyWords](https://github.com/hermitdave/FrequencyWords) (OpenSubtitles) | CC BY-SA 4.0 | conversational word frequencies |
| Bengali Wikipedia | CC BY-SA | language model |

Likhi is not affiliated with Avro Keyboard, OmicronLab, Google, or Microsoft.

## License

MIT. See [LICENSE](LICENSE).
