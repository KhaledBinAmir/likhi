# Third-party components

Likhi's own code is MIT (see LICENSE). This lists everything else that ends up on your machine when
you install it, with its licence and where to get the source, so any part can be rebuilt or
replaced.

Nothing here is modified. Where a component is statically linked into one of Likhi's binaries, that
is said explicitly.

## What is no longer included

Versions before 0.3.0 shipped an embedded CPython with NumPy and marisa-trie to run the engine, and
before 0.2.0 they shipped PIME as the Text Services Framework host. None of those are installed any
more: the text service and the engine are Likhi's own native binaries. An upgrade removes the old
`runtime` directory.

## Statically linked into `engine\likhi-server.exe` and `shell\*\LikhiTextService.dll`

Rust crates, compiled into the binaries. Versions are those in `engine/Cargo.lock` and
`shell/Cargo.lock`; `cargo metadata` in either directory reprints this list for the exact build you
have.

| Crate | Licence | Source |
|---|---|---|
| windows, windows-core, windows-implement, windows-interface, windows-numerics, windows-result, windows-strings, windows-link, windows-threading, windows-collections, windows-future | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| serde, serde_core, serde_derive, serde_json | MIT OR Apache-2.0 | https://github.com/serde-rs |
| memmap2 | MIT OR Apache-2.0 | https://github.com/RazrFalcon/memmap2-rs |
| unicode-normalization | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-normalization |
| rusqlite | MIT | https://github.com/rusqlite/rusqlite |
| libsqlite3-sys | MIT | https://github.com/rusqlite/rusqlite |
| ahash, bitflags, cfg-if, hashbrown, hashlink, itoa, once_cell, smallvec, proc-macro2, quote, syn | MIT OR Apache-2.0 | https://crates.io |
| fallible-iterator, fallible-streaming-iterator | MIT or Apache-2.0 | https://github.com/sfackler/rust-fallible-iterator |
| memchr | Unlicense OR MIT | https://github.com/BurntSushi/memchr |
| tinyvec | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/tinyvec |
| unicode-ident | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| zerocopy | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zmij | MIT | https://crates.io/crates/zmij |

### SQLite

`libsqlite3-sys` bundles the SQLite amalgamation, which its authors have placed in the **public
domain**. It stores the personal dictionary at `%LOCALAPPDATA%\Likhi\personal.sqlite`.

- Source: https://www.sqlite.org

## Data shipped in `engine\models`

Derived artifacts, not the original datasets.

| Artifact | Derived from | Licence |
|---|---|---|
| `indicxlit` | IndicXlit (AI4Bharat) | MIT |
| `avro.json` | the rule tables of avro.py | MIT OR Apache-2.0 |
| `lexicon` unigram counts and phonetic keys | Bengali Wikipedia via Dakshina (Google), OpenSubtitles frequency lists, BanglaTLit | CC BY-SA 4.0, attribution below |
| `lexicon` romanization index | Dakshina (CC BY-SA 4.0), Aksharantar (CC0 / CC-BY), BanglaTLit (MIT) | CC BY-SA 4.0 as the strongest term |

`avro.json` holds the rule tables of **avro.py**, converted to JSON by
`scripts/build_rust_data.py`. The rules are data; the parser that reads them is Likhi's own port of
avro.py's algorithm.

- Source: https://github.com/hitblast/avro.py
- Licence: MIT OR Apache-2.0

## Fonts

Installed only if not already present, and left in place when Likhi is uninstalled. Each carries
its own licence file in `<install dir>\fonts`.

| Font | Licence |
|---|---|
| Noto Sans Bengali | SIL Open Font License 1.1 |
| Anek Bangla | SIL Open Font License 1.1 |
| Hind Siliguri | SIL Open Font License 1.1 |
| Tiro Bangla | SIL Open Font License 1.1 |

## Attribution for the CC BY-SA parts

- Dakshina dataset, Google Research, https://github.com/google-research-datasets/dakshina
- Bengali Wikipedia, https://bn.wikipedia.org
- FrequencyWords (OpenSubtitles), https://github.com/hermitdave/FrequencyWords
- BanglaTLit, https://github.com/farhanishmam/BanglaTLit
- Aksharantar, AI4Bharat, https://huggingface.co/datasets/ai4bharat/Aksharantar

Likhi is not affiliated with Avro Keyboard, OmicronLab, AI4Bharat, Google, or Microsoft.
