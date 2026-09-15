# Contributing to Likhi

Thanks for your interest. Likhi is early; the most valuable contributions right now are
**test data and error reports**, not code.

## The most useful thing you can do

Send romanized Bangla the way *you* actually type it, with the Bangla you meant. Even 50
sentences help, because everyone romanizes differently ("amr"/"amar"/"aamar", "korci"/"korchi").
Use `likhi-collect` to produce a JSONL file, or open an issue with pairs like:

```
roman: ami ekhon office e jacchi
bangla: আমি এখন অফিসে যাচ্ছি
```

Please only share text you wrote yourself and are comfortable making public.

## Code contributions

- Python 3.12, managed with `uv`. Run `uv sync --all-extras` then `uv run pytest`.
- Format and lint with `uv run ruff format` and `uv run ruff check`.
- The engine (`src/likhi/engine`) must stay pure Python with optional heavy extras, and every
  change that can affect accuracy must be accompanied by `likhi-eval` results on the standard
  sets (the CI regression gate will refuse accuracy drops).
- Keep per-keystroke latency in mind: the engine budget is 15 ms at p95 on a laptop CPU.

## Licensing

By contributing code you agree it is released under the MIT license. Do not add data or models
whose license is unknown or incompatible (for example GPL-licensed word lists).
