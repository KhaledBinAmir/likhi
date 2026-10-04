# Developing Likhi

How Likhi works inside, and how to build, test, measure and release it. For what Likhi is and how
to use it, see the [README](../README.md).

## How it works

Likhi follows the recipe behind Gboard's transliteration keyboards: a model that has learned from
real human romanizations, combined with a Bangla word lexicon, a spelling list, and a personal
model that learns from your choices. Details, sources, and the accuracy targets are in
[PLAN.md](PLAN.md) and [research](research).

**The Likhi model** (since 0.6.0) is one small network, built for Likhi, that does three jobs. It
reads the words before the one you are typing and the letters you typed, and

- *transliterates* them: what you typed, as Bangla, in this sentence;
- *completes* them: the word they are the beginning of;
- *predicts the next word*.

It replaces two models: IndicXlit, which transliterated a word alone, and a separate next-word
model. On held-out text it gets the first suggestion right more often than 0.5's engine (76.1%
against 71.1% on the Dakshina word list), and it runs in 41 ms per word instead of 68, behind the
typing rather than in its way: a keystroke only ever waits for the tables, about half a millisecond.

```
keystroke -> TSF text service (a DLL, loaded into the app) -> engine (one background process)
             engine: normalize -> candidates (lexicon index | Likhi model | rule literal | raw Latin)
                     -> rank (model score x lexicon x spelling x personal history) -> top 5
             after each word: the Likhi model's next-word guess, shown as Tab when it is sure
```

The text service and the engine talk over a named pipe, with a local socket as a fallback. The pipe
is what lets Store applications work: they run in an AppContainer, which cannot open a loopback
socket at all.

## Repository layout

```
engine/             the engine: Rust, one binary, no interpreter
shell/              the Windows text service: Rust, a TSF DLL for each architecture
app/                the settings window (C#, WinForms)
installer/          Inno Setup script and the keyboard setup scripts

server/             the telemetry ingest service (Python, runs in a container)
scripts/            dataset download, model conversion, builds
tests/goldens/      recorded behaviour the engine must reproduce
docs/               plan, research notes, design decisions, this file
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
to be argued for rather than re-recorded away. They replay the engine as it was recorded --
IndicXlit, the ranker weights of that time (`tests/goldens/weights.json`), no spelling list -- so
they pin the ranking code, whatever model is installed.

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

`build_rust_data.py` converts IndicXlit (the model before 0.6.0, still read when no Likhi model is
installed) and the Avro rule tables, and stays in Python because its sources are a NumPy `.npz` and
a Python module. It runs once per model, on a developer machine, and its output is what the engine
maps.

The Likhi model, in `models/rust/likhi`, is built separately and is not reproduced from this
repository. `LIKHI_MODEL=<model dir> cargo test --release --test likhimodel` checks that the engine
computes exactly the reference answers that come with a model, and that its 8-bit word table changes
nothing that matters.

Measuring and tuning, which is where ranking work happens:

```
cd engine
cargo build --release --features tools

# accuracy: ~2 minutes across all cores
target/release/likhi-eval words --dataset dakshina-dev

# in context, as typed: every chat word after the words before it, with how often the first
# choice is correctly spelt
target/release/likhi-eval context --dataset banglatlit-val

# the next-word and Tab guess, exactly as the keyboard asks for it
target/release/likhi-eval predict --dataset banglatlit-val --model ../models/rust/likhi

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
uv run python scripts/build_client.py       # dist/likhi/config.json, the default settings
ISCC.exe installer/likhi.iss                # dist/LikhiSetup-<version>.exe
```

Baselines and results live in `results/` and are summarized by `likhi-eval report`.

The README's screenshots are taken by the typing harness from the installed build, with the same
care as a test run (usage reporting off, the personal dictionary and settings put back), and each is
cropped to Notepad's text area and Likhi's list, or to the Likhi window:

```
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/type_test.ps1 -Shots docs/images
```

## Publishing a release

**Since 0.5.0, publishing a release is shipping it.** Every installation checks once a day, and a
release that carries a signed manifest is offered to all of them. So a release is published only
when it is meant for everyone, and a test build is never uploaded with a manifest.

```
# 0. test the installed build for real: types into Notepad through the actual keyboard for about
#    four minutes -- leave the machine alone -- and restores everything it touched afterwards
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/type_test.ps1

# 1. the one place the version number is written
#    installer/likhi.iss:   #define AppVersion "x.y.z"      (and a changelog entry above it)

uv run python scripts/build_engine.py      # reads the version from likhi.iss and bakes it in
uv run python scripts/build_shell.py
uv run python scripts/build_app.py
uv run python scripts/build_client.py
ISCC.exe installer/likhi.iss               # dist/LikhiSetup-x.y.z.exe

# 2. sign: writes latest.json and latest.json.sig, and checks them against the keys the engine trusts
engine/target/release/likhi-sign.exe manifest --installer dist/LikhiSetup-x.y.z.exe --version x.y.z --out dist/release-x.y.z

# 3. publish all three files together, at the full commit hash
gh release create vx.y.z dist/LikhiSetup-x.y.z.exe dist/release-x.y.z/latest.json dist/release-x.y.z/latest.json.sig --target <commit sha> --prerelease
```

The signing key lives at `%USERPROFILE%\.likhi\update-signing.key`, outside the repository, and
only `likhi-sign` reads it. Anyone who holds it can publish an update every installation will
accept, so it belongs in a password manager and nowhere else. Losing it is recoverable -- ship a
build that trusts a new key, which testers install by hand once -- but leaking it is not.

Telemetry from a pilot is handled by `likhi-report`, which is also Rust and also behind the `tools`
feature, so it is never part of an installation:

```
target/release/likhi-report show                       # what this machine holds, shares nothing
target/release/likhi-report pull --out pilot           # needs the admin key, never shipped
target/release/likhi-report collect --drop pilot --out feedback.jsonl
```
