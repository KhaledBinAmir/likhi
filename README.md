# Likhi (লিখি)

**A Gboard-style Bangla phonetic input method for Windows.** Type Bangla the way you already
romanize it in chat, with no spelling rules to learn, and get the right word first, with a few
alternatives you can pick with a key. It learns from you, runs fully offline, and works in every
app through the Windows Text Services Framework.

> Status: **in pilot.** There is an installer, and the keyboard works system-wide including Store
> applications. Suggestion quality is still being tuned on real typing. See
> [docs/PLAN.md](docs/PLAN.md) and [docs/STATUS.md](docs/STATUS.md).

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
keystroke -> TSF text service (a DLL, loaded into the app) -> engine (one background process)
             engine: normalize -> candidates (lexicon index | transliteration model | rule literal | raw Latin)
                     -> rank (transliteration score x language model x personal history) -> top 5
```

The text service and the engine talk over a named pipe, with a local socket as a fallback. The pipe
is what lets Store applications work: they run in an AppContainer, which cannot open a loopback
socket at all.

## Repository layout

Two implementations of the same engine, on purpose.

```
engine/             the engine that ships: Rust, one binary, no interpreter
shell/              the Windows text service: Rust, a TSF DLL for each architecture
app/                the settings window (C#, WinForms)
installer/          Inno Setup script and the keyboard setup scripts

src/likhi/          the reference implementation, in Python
src/likhi/eval/     evaluation harness, metrics, baselines, personal test set tools
src/likhi/data/     data pipeline: lexicon, frequencies, language model, model conversion

scripts/            dataset download, model conversion, golden generation, builds
tests/goldens/      recorded Python behaviour that the Rust must reproduce
docs/               plan, research notes, design decisions
data/               datasets (raw/processed are git-ignored; see scripts/fetch_datasets.py)
results/            evaluation results tracked over time
```

The Python is not dead code. It is the reference the Rust is tested against and the harness the
research is done in: `scripts/dump_goldens.py` records roughly 96,500 of its results and
`engine/tests/goldens.rs` requires the Rust engine to return the same thing. Changing suggestion
behaviour means changing the Python, re-recording, and making the Rust agree.

## Development

The engine and shell need the Rust toolchain and the Visual Studio Build Tools C++ workload; the
research side needs Python 3.12 and [uv](https://docs.astral.sh/uv/).

```
uv sync --all-extras
uv run python scripts/fetch_datasets.py --all                      # datasets + IndicXlit checkpoint
uv run python scripts/convert_indicxlit.py --src data/raw/indicxlit --npz models/indicxlit-np
uv run likhi-data lexicon                                           # unigrams, romanizations, phonetic keys
uv run likhi-eval words --system likhi --dataset dakshina-test      # measure
uv run pytest                                                       # the Python engine's own tests
```

Building the data the engine loads:

```
cd engine
cargo run --release --features build --bin likhi-lexicon -- \
    --raw ../data/raw --out ../models/rust/lexicon      # unigrams, romanizations, keys, bigrams
cd ..
uv run python scripts/build_rust_data.py --skip-tries   # model weights and the Avro rules
```

The lexicon builder is Rust and writes the `.lkx` tables directly. `build_rust_data.py` still
converts the transliteration model and the Avro rule tables, because their sources are a NumPy
`.npz` and a Python module; it can also convert old `.marisa` tries with `--skip-model --skip-avro`,
which is only useful for comparing the two builders.

Building what ships:

```
uv run python scripts/dump_goldens.py       # record the Python's behaviour
cd engine && cargo test --release           # require the Rust to reproduce it
uv run python scripts/build_engine.py       # dist/engine
uv run python scripts/build_shell.py        # dist/shell (x64 and x86)
uv run python scripts/build_app.py          # dist/Likhi.exe
ISCC.exe installer/likhi.iss                # dist/LikhiSetup-<version>.exe
```

Baselines and results live in `results/` and are summarized by `uv run likhi-eval report`.

## Data and model licenses

Likhi's code is MIT. It builds on open data and models whose licenses are respected as follows:

| Resource | License | Use in Likhi |
|---|---|---|
| [Dakshina](https://github.com/google-research-datasets/dakshina) | CC BY-SA 4.0 | evaluation, training; derived data files released under CC BY-SA 4.0 |
| [Aksharantar](https://huggingface.co/datasets/ai4bharat/Aksharantar) | CC0 (mined) / CC-BY (manual) | training |
| [BanglaTLit](https://github.com/farhanishmam/BanglaTLit) | MIT | chat-style tuning and evaluation |
| [IndicXlit](https://github.com/AI4Bharat/IndicXlit) | MIT | transliteration model (weights converted to a flat memory-mapped format) |
| [avro.py](https://github.com/hitblast/avro.py) | MIT or Apache-2.0 | the Avro Phonetic rule tables, used as one candidate channel |
| [IndicCorp v2](https://huggingface.co/datasets/ai4bharat/IndicCorpV2) | CC0 | word frequencies, language model |
| [FrequencyWords](https://github.com/hermitdave/FrequencyWords) (OpenSubtitles) | CC BY-SA 4.0 | conversational word frequencies |
| Bengali Wikipedia | CC BY-SA | language model |

Likhi is not affiliated with Avro Keyboard, OmicronLab, Google, or Microsoft.

## License

MIT. See [LICENSE](LICENSE).
