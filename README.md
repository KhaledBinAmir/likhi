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
- Learning data stays on your machine, and the personal dictionary is exportable.
- Usage reporting is **on by default** during the pilot and is turned off in one click. It never
  sends anything you type successfully. See [Privacy](#privacy).

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

## Privacy

Likhi is an input method, so it sees everything you type. What it does with that is worth being
exact about.

**Never leaves your machine, ever:** the words you type, the text you produce, and what Likhi learns
from your choices. The personal model is a SQLite database in `%LOCALAPPDATA%\Likhi` and is yours.

**Usage reporting is on by default in the pilot builds**, including the installer attached to the
releases here, and sends two things to a collection endpoint:

- *Counters.* How many words were committed, how often the first suggestion was the one taken, which
  position was chosen, how long suggestions took, and the name of the application. No text at all.
- *Struggle words.* **Only when the first suggestion was wrong**: the roman string you typed, the
  word you picked instead, and the word Likhi wrongly put first. A word accepted first time is never
  recorded, because it teaches us nothing.

Before anything is written, it is filtered: anything containing a digit, `@`, `:`, `/` or `\` is
dropped, so identifiers, passwords, times, money and URLs never qualify. Anything longer than 32
characters is dropped, which excludes pasted or concatenated text. A field the application marks
secure, such as a password box, records nothing whatsoever, not even a counter. No timestamp is
finer than the hour, and the machine is identified by a random installation id and nothing else.

**To turn it off:** open **Likhi** from the Start menu and clear **"Share anonymous usage data to
improve suggestions"**. That writes a per-user setting, so it needs no administrator and does not
decide for anyone else sharing the machine. To see exactly what is held before deciding, the files are plain JSON Lines in
`%LOCALAPPDATA%\Likhi` (`metrics.jsonl` and `events.jsonl`) and can be read in Notepad; deleting
them is enough to erase them.

The reason it defaults to on: this is a pilot, and the struggle words are the only honest signal for
which words the engine gets wrong. They become the test set every later change is measured against.
That is a real trade against your privacy, which is why it is written out here rather than buried.

## Repository layout

```
engine/             the engine: Rust, one binary, no interpreter
shell/              the Windows text service: Rust, a TSF DLL for each architecture
app/                the settings window (C#, WinForms)
installer/          Inno Setup script and the keyboard setup scripts

server/             the telemetry ingest service (Python, runs in a container)
scripts/            dataset download, model conversion, builds
tests/goldens/      recorded behaviour the engine must reproduce
docs/               plan, research notes, design decisions
data/               datasets (raw/processed are git-ignored; see scripts/fetch_datasets.py)
results/            evaluation results tracked over time
```

Everything that runs on someone's machine is Rust: the engine, the text service, the lexicon
builder, the evaluation harness, the tuner, the stress tool and the telemetry operator tool. What
is left in Python is the ingest service, which runs in a container rather than on a machine, and
the scripts that prepare data on a developer's machine.

It began as a Python engine with a Rust port beside it. Each tool was moved only once it produced
the same numbers as the one it replaced, and the Python was deleted after the last of them did.
`engine/tests/goldens.rs` replays 96,537 of the original's recorded results and requires the engine
to return the same thing. Those files cannot be regenerated, which is deliberate: they are a fixed
record of behaviour that a second implementation once agreed with, so a change that moves them has
to be argued for rather than re-recorded away.

## Development

The engine and shell need the Rust toolchain and the Visual Studio Build Tools C++ workload.
Preparing data and running the builds needs Python 3.12 and [uv](https://docs.astral.sh/uv/).

```
cd engine && cargo test --release --features tools    # the engine's own tests and the goldens
uv sync --all-extras
uv run python scripts/fetch_datasets.py --all         # datasets + IndicXlit checkpoint
uv run python scripts/convert_indicxlit.py --src data/raw/indicxlit --npz models/indicxlit-np
uv run pytest                                         # ingest service, key map, file encodings
```

Building the data the engine loads:

```
cd engine
cargo run --release --features tools --bin likhi-lexicon -- \
    --raw ../data/raw --out ../models/rust/lexicon      # unigrams, romanizations, keys, bigrams
cd ..
uv run python scripts/build_rust_data.py               # model weights and the Avro rules
```

The lexicon builder writes the `.lkx` tables directly. It refuses to run without `weights.json`,
the tuned ranker weights produced by `likhi-tune`: they are not a build output, and a lexicon
missing them costs about seven points of top-1 silently.

`build_rust_data.py` converts the transliteration model and the Avro rule tables, and stays in
Python because its sources are a NumPy `.npz` and a Python module. It runs once per model, on a
developer machine, and its output is what the engine maps.

Measuring and tuning, which is where ranking work happens:

```
cd engine
cargo build --release --features tools

# accuracy: ~2 minutes across all cores
target/release/likhi-eval words --dataset dakshina-dev

# ranker weights: cache features once, then search over weights in seconds
target/release/likhi-tune cache  --dataset dakshina-dev --dataset banglatlit-val-words
target/release/likhi-tune search --metric top1
```

`likhi-tune search` refuses to overwrite an existing `weights.json` from a small cache. Those
weights are worth roughly seven points of top-1, and a search over a few hundred items will happily
produce worse ones; use `--out` to write elsewhere while experimenting.

Building what ships:

```
cd engine && cargo test --release --features tools   # including the goldens
cd ..
uv run python scripts/build_engine.py       # dist/engine
uv run python scripts/build_shell.py        # dist/shell (x64 and x86)
uv run python scripts/build_app.py          # dist/Likhi.exe
ISCC.exe installer/likhi.iss                # dist/LikhiSetup-<version>.exe
```

Baselines and results live in `results/` and are summarized by `likhi-eval report`.

Telemetry from a pilot is handled by `likhi-report`, which is also Rust and also behind the `tools`
feature, so it is never part of an installation:

```
target/release/likhi-report show                       # what this machine holds, shares nothing
target/release/likhi-report pull --out pilot           # needs the admin key, never shipped
target/release/likhi-report collect --drop pilot --out feedback.jsonl
```

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
