#!/usr/bin/env python3
"""Generate/check the Rust Dig artifact presentation snapshot.

The production Rust module reads the checked-in JSON snapshot and never
imports or parses Python.  This script is the explicit parity gate: whenever
``services/dig_data/artifacts.py`` changes, ``--check`` fails until the
snapshot is regenerated.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict, is_dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

SOURCE = ROOT / "services" / "dig_data" / "artifacts.py"
TARGET = ROOT / "rust" / "crates" / "cama-app" / "src" / "dig_artifact_catalog.json"


def _normalize(value: object) -> object:
    if is_dataclass(value):
        return {key: _normalize(item) for key, item in asdict(value).items()}
    if isinstance(value, tuple):
        return [_normalize(item) for item in value]
    if isinstance(value, list):
        return [_normalize(item) for item in value]
    if isinstance(value, dict):
        return {str(key): _normalize(item) for key, item in value.items()}
    return value


def render_catalog() -> bytes:
    # Import only the immutable authored data module.  Do not import the Dig
    # service, Discord adapter, or SQLite repository from this generator.
    from services.dig_data.artifacts import ALL_ARTIFACTS

    artifacts = [_normalize(artifact) for artifact in ALL_ARTIFACTS]
    lines = [
        json.dumps(artifact, ensure_ascii=False, separators=(",", ":"))
        + ("," if index + 1 < len(artifacts) else "")
        for index, artifact in enumerate(artifacts)
    ]
    return ("[\n" + "\n".join(lines) + "\n]\n").encode("utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail when the snapshot is stale")
    mode.add_argument("--write", action="store_true", help="write the regenerated snapshot")
    args = parser.parse_args(argv)

    rendered = render_catalog()
    if args.write:
        TARGET.write_bytes(rendered)
        print(f"wrote {len(json.loads(rendered))} artifacts to {TARGET}")
        return 0
    if args.check:
        actual = TARGET.read_bytes()
        if actual != rendered:
            print(
                f"stale Dig artifact snapshot: regenerate {TARGET} from {SOURCE}",
                file=sys.stderr,
            )
            return 1
        print(f"Dig artifact snapshot is current ({len(json.loads(rendered))} entries)")
        return 0
    sys.stdout.buffer.write(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
