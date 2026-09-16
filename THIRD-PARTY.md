# Third-party components

Likhi's own code is MIT (see LICENSE). The installer redistributes the following unmodified
components. Each remains under its own licence, and the sources are linked so anyone can rebuild
or replace them.

## PIME

The Text Services Framework host that lets a Python program act as a Windows input method.
Redistributed unmodified.

- Source: https://github.com/EasyIME/PIME
- Licence: LGPL-2.1 (with Apache-2.0 and PSF-licensed parts), see the repository
- Version: the `version.txt` beside `PIMELauncher.exe` in the installed folder

LGPL-2.1 gives you the right to replace this component. The installer places it in
`<install dir>\pime`, so a rebuilt `PIMETextService.dll` or `PIMELauncher.exe` can be dropped in
directly. Re-register with `regsvr32` after replacing the DLL.

## CPython (embedded distribution)

- Source: https://www.python.org/downloads/windows/
- Licence: Python Software Foundation License, included as `runtime\python\LICENSE.txt`

## NumPy

- Source: https://github.com/numpy/numpy
- Licence: BSD-3-Clause

## marisa-trie

- Source: https://github.com/pytries/marisa-trie
- Licence: MIT (wrapping libmarisa, BSD-2-Clause / LGPL-2.1)

## avro.py

The rule-based Avro Phonetic parser, used as one candidate source and as a fallback.

- Source: https://github.com/hitblast/avro.py
- Licence: MIT

## Models and data

Shipped inside `runtime\models`. These are derived artifacts, not the original datasets.

| Artifact | Derived from | Licence |
|---|---|---|
| `indicxlit-np` | IndicXlit (AI4Bharat) | MIT |
| `lexicon` unigram counts and phonetic keys | Bengali Wikipedia via Dakshina (Google), OpenSubtitles frequency lists, BanglaTLit | CC BY-SA 4.0, attribution below |
| `lexicon` romanization index | Dakshina (CC BY-SA 4.0), Aksharantar (CC0 / CC-BY), BanglaTLit (MIT) | CC BY-SA 4.0 as the strongest term |

Attribution for the CC BY-SA parts:

- Dakshina dataset, Google Research, https://github.com/google-research-datasets/dakshina
- Bengali Wikipedia, https://bn.wikipedia.org
- FrequencyWords (OpenSubtitles), https://github.com/hermitdave/FrequencyWords
- BanglaTLit, https://github.com/farhanishmam/BanglaTLit
- Aksharantar, AI4Bharat, https://huggingface.co/datasets/ai4bharat/Aksharantar

Likhi is not affiliated with Avro Keyboard, OmicronLab, AI4Bharat, Google, or Microsoft.
