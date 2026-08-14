#!/usr/bin/env python3
"""Generate/check the checked-in authored Dig/Pet asset manifest.

The Rust runtime receives the repository's ``assets/dig`` and ``assets/pets``
trees at the exact ``/app/assets`` paths retained by ``Dockerfile.rust``.
This manifest records every regular file in those trees, including Pet
component sidecars, so a missing file, byte change, or untracked addition is
an explicit review/CI failure rather than a silent procedural fallback.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ASSET_ROOTS = ("dig", "pets")
TARGET = ROOT / "rust" / "crates" / "cama-app" / "src" / "dig_assets" / "authored_asset_manifest.json"


def _entries() -> list[dict[str, int | str]]:
    entries: list[dict[str, int | str]] = []
    for root_name in ASSET_ROOTS:
        root = ROOT / "assets" / root_name
        for path in sorted(path for path in root.rglob("*") if path.is_file()):
            relative = path.relative_to(ROOT / "assets").as_posix()
            data = path.read_bytes()
            entries.append(
                {
                    "path": relative,
                    "bytes": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(),
                }
            )
    return entries


def render_manifest() -> bytes:
    payload = {"schema": 1, "files": _entries()}
    return (json.dumps(payload, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail when the manifest is stale")
    mode.add_argument("--write", action="store_true", help="write the regenerated manifest")
    args = parser.parse_args(argv)

    rendered = render_manifest()
    entries = json.loads(rendered)["files"]
    if args.write:
        TARGET.write_bytes(rendered)
        print(f"wrote {len(entries)} authored assets to {TARGET}")
        return 0
    if args.check:
        try:
            actual = TARGET.read_bytes()
        except OSError as error:
            print(f"missing authored asset manifest {TARGET}: {error}", file=sys.stderr)
            return 1
        if actual != rendered:
            print(
                "stale authored asset manifest: regenerate with "
                "rust/scripts/generate_authored_asset_manifest.py --write",
                file=sys.stderr,
            )
            return 1
        print(f"authored asset manifest is current ({len(entries)} files)")
        return 0
    sys.stdout.buffer.write(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
