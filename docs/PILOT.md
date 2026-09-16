# Running an office pilot

How to put Likhi on a handful of machines, see whether it is actually working, and get back the
words it gets wrong. Written for the person running the pilot, who is also the IT administrator.

## What gets collected, exactly

Telemetry is **off by default**. Two modes turn it on, and both write plain text files the user can
open, in `%LOCALAPPDATA%\Likhi\`.

| Mode | Contents | Contains typed text? |
|---|---|---|
| `metrics` | one row per 20 words: words committed, how often candidate 1 was taken, which position was chosen, retype count, latency p50/p95, per application | **no** |
| `full` | the above, plus one row per *struggle event* | only the struggle words |

A struggle event is recorded **only when the user did not take the first suggestion**, or corrected
the word with backspace while composing. The row is:

```json
{"h": "2026-09-16T15", "roman": "amr", "chose": "আমরা", "we_said": "আমার", "pos": 2, "retyped": false}
```

That is the entire content: the typed letters, the word chosen, and the word Likhi wrongly put
first. Words accepted first time are never written, because they teach the engine nothing.

Dropped before anything is written:

- anything containing a digit, `@`, `:`, `/` or `\` (one-time codes, addresses, URLs, money, times)
- anything longer than 32 characters (pasted or run-together text)
- anything a caller marks as a secure field
- sentences, surrounding words, exact timestamps, user names, machine names

Identity is a random 16-character install id generated once per machine. Nothing links a file to a
person except the folder you copy it from.

Note on password boxes: Windows normally does not route password fields through a text service, so
composition does not happen there. The digit and symbol rules are the real safety net, not that
behaviour, which varies by application.

## Collecting logs from several machines

No server software, no open ports. Each machine appends to its own local file, and a background
task copies **only the new bytes** to a folder you own, as immutable numbered chunks:

```
\\SERVER\likhi-pilot\
  3f9a12c7d4e5b601\  metrics-00001.jsonl  metrics-00002.jsonl  events-00001.jsonl
  a71b93ff02c4d5e8\  metrics-00001.jsonl  events-00001.jsonl
```

Why chunks rather than one shared file: many machines writing to one file over SMB is a corruption
risk, and a retry after a dropped connection could duplicate lines. Immutable numbered files make a
retry harmless, need no locking, and can be re-sent safely if a copy fails halfway.

Set up on each machine, in `C:\Program Files (x86)\PIME\python\input_methods\likhi\config.json`:

```json
"telemetry": "full",
"telemetry_drop": "\\\\SERVER\\likhi-pilot"
```

Then restart the engine. Sync runs every five minutes on a background thread, never on the path of
a keystroke, and silently does nothing when the share is unreachable (laptop at home, VPN down);
the next attempt sends the same bytes.

Permissions: give each tester's account write access to the share and no read access to other
folders. Even redacted struggle words are someone's typing.

## Looking at the data

On a tester's machine, showing them exactly what would leave it:

```
likhi-report show
```

On your machine, pointed at the drop folder:

```
likhi-report collect --drop \\SERVER\likhi-pilot
likhi-report collect --drop \\SERVER\likhi-pilot --out pilot-words.jsonl
```

`collect` prints per-install health and the struggle words ranked by **how many different installs
hit them**. Words seen on only one install are held back by default (`--min-installs 2`), because
that is what a person's own name or a private term looks like. Words several people hit are real
engine gaps.

`--out` writes them in the same format as `data/feedback/words.jsonl`. Review that file by eye,
then append it, and every future change is measured against those words:

```
likhi-eval words --system likhi --dataset feedback-words
```

## Telling testers

Worth saying plainly, because people are right to ask what a keyboard records:

- Everything runs on their machine. There is no internet connection and no account.
- What is shared with you: numbers about speed and accuracy, plus the words where Likhi guessed
  wrong. Not their messages, not anything with digits or symbols.
- They can see it all with `likhi-report show` and erase it with `likhi-report purge`.
- Turning it off is one word in a config file, or ask you.

## Turning it off

Set `"telemetry": "off"` and restart the engine. To erase what a machine already holds, run
`likhi-report purge`, which deletes the telemetry files and leaves the personal learning database
alone.
