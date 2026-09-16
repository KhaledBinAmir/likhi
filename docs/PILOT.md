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

## Collecting logs over the internet (the production path)

Use this rather than a LAN folder if you want the pilot to exercise the same mechanism a public
release will use. The client posts newline-delimited JSON over HTTPS with the Python standard
library, so the keyboard gains no dependency.

Client config on each machine:

```json
"telemetry": "full",
"telemetry_endpoint": "https://likhi-ingest-xxxxx.run.app/v1/ingest",
"telemetry_key": "a-long-random-string",
"telemetry_sync_seconds": 3600
```

### Deploying the endpoint on Google Cloud

`server/ingest_server.py` is the whole server, standard library plus one client library when it
writes to Cloud Storage. It stores the same object layout as the folder drop, so everything
downstream is unchanged.

From Cloud Shell or any machine with `gcloud`. `--source server` is a path on the machine running
the command, so fetch the repository first; Cloud Shell starts with an empty home directory.

```
git clone https://github.com/KhaledBinAmir/likhi.git
cd likhi

SECRET=$(openssl rand -hex 24)   # keep this, the clients need it
echo "$SECRET"

gcloud storage buckets create gs://likhi-telemetry --location us-central1

gcloud run deploy likhi-ingest --source server --region us-central1 \
    --set-env-vars LIKHI_INGEST_BUCKET=likhi-telemetry,LIKHI_INGEST_KEY=$SECRET \
    --allow-unauthenticated --min-instances 0
```

`--allow-unauthenticated` is required because the keyboards post without Google credentials; the
shared key is what authenticates them. `--min-instances 0` lets the service scale to zero, so an
idle pilot runs no instances at all.

Then give the service's identity permission to write to the bucket, otherwise every upload returns
503:

```
PROJECT=$(gcloud config get-value project)
NUMBER=$(gcloud projects describe "$PROJECT" --format='value(projectNumber)')
gcloud storage buckets add-iam-policy-binding gs://likhi-telemetry \
    --member="serviceAccount:${NUMBER}-compute@developer.gserviceaccount.com" \
    --role=roles/storage.objectAdmin
```

Check it end to end. The second command should answer `{"ok": true, "stored": 1}` and the third
should list the object:

```
URL=$(gcloud run services describe likhi-ingest --region us-central1 --format='value(status.url)')
curl -s "$URL/v1/health"

curl -sS -X POST "$URL/v1/ingest" \
    -H "X-Likhi-Install: abc123def456" -H "X-Likhi-Stream: events" -H "X-Likhi-Seq: 1" \
    -H "X-Likhi-Key: $SECRET" -H "Content-Type: application/x-ndjson" \
    --data-binary '{"h":"test","roman":"amr","chose":"আমরা","we_said":"আমার","pos":2,"retyped":false}'

gcloud storage ls -r gs://likhi-telemetry
```

Posting without the key must return 401. Deleting the test object afterwards keeps the pilot data
clean: `gcloud storage rm gs://likhi-telemetry/abc123def456/events-00001.jsonl`.

To read the data back:

```
gcloud storage cp -r gs://likhi-telemetry/* ./pilot
likhi-report collect --drop ./pilot
```

### What it costs

Always-free allowances, as published by Google at the time of writing:

| Service | Always free per month |
|---|---|
| Cloud Run | 2 million requests, 180,000 vCPU-seconds, 360,000 GB-seconds, 1 GB egress from North America |
| Cloud Storage | 5 GB-months in `us-east1`, `us-west1` or `us-central1`, 5,000 Class A operations, 50,000 Class B operations |
| Firestore (alternative store) | 1 GiB, 50,000 reads / 20,000 writes / 20,000 deletes per day |

A ten-person pilot sends roughly one small upload per stream per active hour. At
`telemetry_sync_seconds: 3600` that is about 3,800 uploads a month, inside both the Cloud Run
request allowance and the 5,000 free Cloud Storage writes, so the pilot costs nothing. Dropping to
15-minute syncs is more responsive and pushes writes past the free allowance, costing a few cents
a month. Storage itself is negligible: the data is a few kilobytes per person per day.

For a public release at, say, ten thousand users syncing once a day, that is about 300,000 requests
a month, still inside Cloud Run's free tier, with Cloud Storage writes running to a dollar or two.
Firestore is the alternative if you would rather query than download files; its 20,000 writes a day
covers far more users, at the cost of a different layout than `likhi-report collect` expects.

### Self-hosting instead

The same file runs on any always-on machine with no cloud account:

```
python server/ingest_server.py --data D:\likhi-telemetry --key a-long-random-string --port 8098
```

Put it behind a reverse proxy for a certificate, or pass `--certfile` and `--keyfile`. Clients then
point at `https://your-host/v1/ingest`.

### What the endpoint guarantees

- The install id and stream name are matched against strict patterns before any path is built, so a
  hostile header cannot escape the data directory. Covered by a test.
- A repeated chunk sequence number is accepted and ignored, which makes a client retry after a
  half-finished upload harmless. Covered by a test.
- Bodies are capped at 1 MB and must be newline-delimited JSON objects.
- Client IP addresses are not recorded.
- Requests without the shared key are refused.

One Cloud Run quirk, measured rather than assumed: the platform answers `/healthz` itself and that
request never reaches the container, so the health path is `/v1/health`. Every other path arrives
normally.

## Collecting logs from several machines on a LAN

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
