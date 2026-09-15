# Personal test set

`personal.jsonl` (git-ignored) holds sentence pairs in *your* romanization style. It is the most
important accuracy gate in the plan: public datasets are Indian Wikipedia/news style or noisy chat,
and neither is exactly how you type.

Format, one JSON object per line:

```json
{"roman": "amr ekhon office e jete hobe", "gold": ["আমার এখন অফিসে যেতে হবে"], "tags": ["daily"]}
{"roman": "jonne", "gold": ["জন্যে", "জন্য"], "tags": ["short"]}
```

- `gold` is a list so alternates are allowed.
- Suggested tags: `daily`, `work`, `names`, `slang`, `english` (loanwords you want in Bangla script),
  `mixed` (sentences with real English words that must stay Latin), `numbers`.
- Target: ~300 sentences, ~2,500 words, at least 50 items tagged `names`/`slang`/`english`.

How to build it honestly:

1. Export or copy romanized Bangla you have actually sent in the past (WhatsApp/Messenger exports
   of your own messages). Do not tidy the spelling.
2. Put one sentence per line in a text file and run `uv run likhi-collect import mymsgs.txt`.
3. Run `uv run likhi-collect fill mymsgs.template.jsonl` and type the Bangla you meant for each line
   (Gboard on your phone is a fine way to produce the gold quickly; paste it in).
4. `uv run likhi-collect stats` and `uv run likhi-collect validate`.

The file is split deterministically into dev (even lines) and test (odd lines) by the harness, so
tuning never sees the test half.
