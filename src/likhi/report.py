"""likhi-report: see what telemetry holds, ship it, and aggregate a pilot.

Client side (on a tester's machine):
    likhi-report show                      what is in my local files, nothing leaves the machine
    likhi-report sync                      ship new lines now, to wherever this machine is configured
    likhi-report purge                     delete every local telemetry file

Collector side, pulling from the ingest endpoint (needs the admin key, never shipped to clients):
    likhi-report pull --out pilot          download everything into the standard chunk layout
    likhi-report collect --drop pilot [--min-installs 2] [--out feedback.jsonl]

`pull` and `sync` read `pilot.local.json` at the repository root when arguments are omitted, so the
keys stay out of version control and out of shell history.

``collect`` prints per-install health (words typed, how often the first suggestion was taken,
latency) and the struggle words ranked by how many *different* installs hit them. Words seen on
only one install are held back by default, because that is how a person's own name or a private
term looks; raise --min-installs for a stricter bar or set 1 to review everything.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

from likhi.telemetry import DEFAULT_DIR, Telemetry

REPO = Path(__file__).resolve().parents[2]
# Deployment secrets live outside version control. The ingest key ships inside clients; the admin
# key never does and is the only way to read collected data back.
PILOT_SECRETS = REPO / "pilot.local.json"


def _secrets() -> dict:
    for path in (PILOT_SECRETS, Path.home() / ".likhi-pilot.json"):
        try:
            if path.exists():
                return json.loads(path.read_text(encoding="utf-8-sig"))
        except Exception:
            pass
    return {}


def _read_jsonl(path: Path) -> list[dict]:
    rows = []
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        rows.append(json.loads(line))
                    except Exception:
                        pass
    except FileNotFoundError:
        pass
    return rows


def cmd_show(args: argparse.Namespace) -> int:
    d = Path(args.dir) if args.dir else DEFAULT_DIR
    metrics = _read_jsonl(d / "metrics.jsonl")
    events = _read_jsonl(d / "events.jsonl")
    print(f"telemetry files in {d}")
    print(f"  metrics rows : {len(metrics)}")
    print(f"  struggle rows: {len(events)}")
    if metrics:
        words = sum(r.get("words", 0) for r in metrics)
        top1 = sum(r.get("top1_taken", 0) for r in metrics)
        print(
            f"  words committed: {words}   first suggestion taken: {100 * top1 / max(1, words):.1f}%"
        )
    print("\nEvery line that would be shared (this is the whole content):")
    for r in events[: args.limit]:
        print(
            f"  typed {r.get('roman', ''):<16} chose {r.get('chose', ''):<14} we ranked first {r.get('we_said', '')}"
        )
    if len(events) > args.limit:
        print(f"  ... {len(events) - args.limit} more")
    return 0


def cmd_sync(args: argparse.Namespace) -> int:
    drop, endpoint, key = args.drop, args.endpoint, args.key
    if not drop and not endpoint:  # fall back to whatever the installed shell is configured with
        from likhi.server import _telemetry_config

        cfg = _telemetry_config()
        drop, endpoint, key = cfg["drop"], cfg["endpoint"], cfg["key"]
    if not drop and not endpoint:
        sec = _secrets()
        endpoint, key = sec.get("endpoint"), sec.get("ingest_key")
    t = Telemetry("metrics", directory=args.dir, drop=drop, endpoint=endpoint, key=key)
    result = t.sync()
    print(json.dumps(result, ensure_ascii=False))
    return 0 if not result.get("error") else 1


def cmd_pull(args: argparse.Namespace) -> int:
    """Download everything the endpoint holds into the same layout `collect` reads."""
    import urllib.request

    sec = _secrets()
    endpoint = args.endpoint or sec.get("endpoint") or ""
    admin_key = args.admin_key or sec.get("admin_key") or ""
    if not endpoint or not admin_key:
        print("need an endpoint and an admin key (arguments, or pilot.local.json)")
        return 1
    base = endpoint.rsplit("/v1/", 1)[0]
    url = f"{base}/v1/export"
    if args.since:
        url += f"?since={args.since}"
    req = urllib.request.Request(url, headers={"X-Likhi-Admin-Key": admin_key})
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            body = resp.read().decode("utf-8")
    except Exception as e:
        print(f"pull failed: {type(e).__name__}: {e}")
        return 1

    out = Path(args.out)
    chunks: dict[tuple[str, str], list[str]] = defaultdict(list)
    truncated = False
    for line in body.splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get("_truncated"):
            truncated = True
            continue
        key = (row.pop("_install", "unknown"), row.pop("_chunk", "chunk.jsonl"))
        chunks[key].append(json.dumps(row, ensure_ascii=False))
    for (install, name), lines in chunks.items():
        d = out / install
        d.mkdir(parents=True, exist_ok=True)
        (d / name).write_text("\n".join(lines) + "\n", encoding="utf-8")
    installs = len({i for i, _ in chunks})
    print(f"pulled {sum(len(v) for v in chunks.values())} rows from {installs} installs into {out}")
    if truncated:
        print("NOTE: the export hit its size cap; pull again with --since to continue")
    print(f"next: likhi-report collect --drop {out}")
    return 0


def cmd_purge(args: argparse.Namespace) -> int:
    d = Path(args.dir) if args.dir else DEFAULT_DIR
    n = 0
    for name in ("metrics.jsonl", "events.jsonl", "sync_state.json"):
        p = d / name
        if p.exists():
            p.unlink()
            n += 1
    print(f"deleted {n} telemetry files from {d} (install_id and learning data untouched)")
    return 0


def cmd_collect(args: argparse.Namespace) -> int:
    drop = Path(args.drop)
    if not drop.exists():
        print(f"drop folder not found: {drop}")
        return 1
    installs = sorted(p for p in drop.iterdir() if p.is_dir())
    print(f"{len(installs)} installs reporting in {drop}\n")
    print(
        f"{'install':18} {'words':>8} {'top1%':>7} {'retyped':>8} {'p50ms':>7} {'p95ms':>7} {'struggles':>10}"
    )
    total_words = total_top1 = 0
    word_installs: dict[tuple[str, str], set[str]] = defaultdict(set)
    word_wrong: dict[tuple[str, str], Counter[str]] = defaultdict(Counter)
    for inst in installs:
        metrics = [r for f in sorted(inst.glob("metrics-*.jsonl")) for r in _read_jsonl(f)]
        events = [r for f in sorted(inst.glob("events-*.jsonl")) for r in _read_jsonl(f)]
        words = sum(r.get("words", 0) for r in metrics)
        top1 = sum(r.get("top1_taken", 0) for r in metrics)
        retyped = sum(r.get("retyped", 0) for r in metrics)
        lat50 = [r["lat_p50"] for r in metrics if "lat_p50" in r]
        lat95 = [r["lat_p95"] for r in metrics if "lat_p95" in r]
        total_words += words
        total_top1 += top1
        print(
            f"{inst.name:18} {words:8} {100 * top1 / max(1, words):7.1f} {retyped:8} "
            f"{(sum(lat50) / len(lat50) if lat50 else 0):7.1f} {(sum(lat95) / len(lat95) if lat95 else 0):7.1f} {len(events):10}"
        )
        for e in events:
            key = (e.get("roman", ""), e.get("chose", ""))
            if key[0] and key[1]:
                word_installs[key].add(inst.name)
                if e.get("we_said"):
                    word_wrong[key][e["we_said"]] += 1
    print(
        f"\ntotal words {total_words}, first suggestion taken {100 * total_top1 / max(1, total_words):.1f}%"
    )

    shared = {k: v for k, v in word_installs.items() if len(v) >= args.min_installs}
    print(
        f"\nstruggle words seen on >= {args.min_installs} installs: {len(shared)} of {len(word_installs)}"
    )
    print(f"{'typed':<18} {'they chose':<16} {'we ranked first':<16} {'installs':>8}")
    ranked = sorted(shared.items(), key=lambda kv: (-len(kv[1]), kv[0][0]))
    for (roman, chose), insts in ranked[: args.limit]:
        wrong = word_wrong[(roman, chose)].most_common(1)
        print(f"{roman:<18} {chose:<16} {(wrong[0][0] if wrong else ''):<16} {len(insts):8}")
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            for (roman, chose), insts in ranked:
                f.write(
                    json.dumps(
                        {
                            "roman": roman,
                            "gold": [chose],
                            "tags": ["pilot"],
                            "installs": len(insts),
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
        print(
            f"\nwrote {len(ranked)} rows to {args.out} (review, then append to data/feedback/words.jsonl)"
        )
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="likhi-report",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("show", help="print local telemetry, share nothing")
    s.add_argument("--dir")
    s.add_argument("--limit", type=int, default=40)
    s.set_defaults(fn=cmd_show)
    s = sub.add_parser("sync", help="ship new lines now (defaults to the configured destination)")
    s.add_argument("--drop")
    s.add_argument("--endpoint")
    s.add_argument("--key")
    s.add_argument("--dir")
    s.set_defaults(fn=cmd_sync)
    s = sub.add_parser("pull", help="download collected telemetry from the endpoint")
    s.add_argument("--endpoint")
    s.add_argument("--admin-key")
    s.add_argument("--since", help="hour bucket to start from, e.g. 2026-09-16T00")
    s.add_argument("--out", default="pilot", help="directory to write the chunk layout into")
    s.set_defaults(fn=cmd_pull)
    s = sub.add_parser("purge", help="delete local telemetry files")
    s.add_argument("--dir")
    s.set_defaults(fn=cmd_purge)
    s = sub.add_parser("collect", help="aggregate a pilot from the drop folder")
    s.add_argument("--drop", required=True)
    s.add_argument("--min-installs", type=int, default=2)
    s.add_argument("--limit", type=int, default=60)
    s.add_argument("--out", help="write reviewed struggle words as a feedback JSONL")
    s.set_defaults(fn=cmd_collect)
    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
