"""Stamp a ready-to-install client: the text service plus a config with the pilot keys baked in.

Testers then configure nothing. This is the step the installer will call.

    python scripts/build_client.py                       # keys from pilot.local.json
    python scripts/build_client.py --endpoint https://... --key ... --telemetry full
    python scripts/build_client.py --telemetry off       # a build that reports nothing

Secrets come from ``pilot.local.json`` at the repository root (git-ignored):

    {"endpoint": "https://likhi-ingest-xxx.run.app/v1/ingest",
     "ingest_key": "...",           <- baked into the client, write-only by design
     "admin_key":  "..."}           <- NEVER baked in; only `likhi-report pull` uses it

An embedded key is not a secret: anyone who installs the client can read it out. That is why the
ingest endpoint accepts appends only, and why reading collected data needs the separate admin key.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SOURCE = REPO / "windows" / "pime" / "likhi"
SECRETS = REPO / "pilot.local.json"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--out", type=Path, default=REPO / "dist" / "likhi")
    ap.add_argument("--endpoint")
    ap.add_argument("--key", help="ingest key to bake in")
    ap.add_argument("--telemetry", choices=["off", "metrics", "full"], default="full")
    ap.add_argument("--sync-seconds", type=int, default=3600)
    args = ap.parse_args(argv)

    secrets = {}
    if SECRETS.exists():
        secrets = json.loads(SECRETS.read_text(encoding="utf-8-sig"))
    endpoint = args.endpoint or secrets.get("endpoint", "")
    key = args.key or secrets.get("ingest_key", "")
    if args.telemetry != "off" and not (endpoint and key):
        print(
            f"telemetry is '{args.telemetry}' but no endpoint/key given.\n"
            f"Pass --endpoint and --key, or create {SECRETS.name} (see this script's docstring),\n"
            f"or build with --telemetry off.",
            file=sys.stderr,
        )
        return 2
    if "admin_key" in secrets and secrets.get("admin_key") == key:
        print("refusing to bake the admin key into a client", file=sys.stderr)
        return 2

    out: Path = args.out
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    for item in SOURCE.iterdir():
        if item.name in ("__pycache__", "config.pilot.example.json"):
            continue
        shutil.copy2(item, out / item.name)

    cfg = json.loads((SOURCE / "config.json").read_text(encoding="utf-8-sig"))
    cfg["telemetry"] = args.telemetry
    cfg["telemetry_endpoint"] = endpoint if args.telemetry != "off" else ""
    cfg["telemetry_key"] = key if args.telemetry != "off" else ""
    cfg["telemetry_sync_seconds"] = args.sync_seconds
    # utf-8 without a byte-order mark: the readers tolerate one, but do not create one
    (out / "config.json").write_text(
        json.dumps(cfg, indent=1, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    print(f"built {out}")
    print(
        f"  telemetry : {args.telemetry}" + (f" -> {endpoint}" if args.telemetry != "off" else "")
    )
    print(f"  sync every: {args.sync_seconds}s")
    print(f"  files     : {', '.join(sorted(p.name for p in out.iterdir()))}")
    print("\nInstall on a machine (elevated):")
    print(f'  powershell -File windows\\pime\\install_dev.ps1 -Source "{out}"')
    return 0


if __name__ == "__main__":
    sys.exit(main())
