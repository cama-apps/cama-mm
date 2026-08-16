#!/usr/bin/env python3
"""Create and verify an atomic SQLite online backup for deployment."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import sys
import time
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def backup(source_path: Path, destination_path: Path) -> dict[str, object]:
    source_path = source_path.resolve(strict=True)
    destination_path = destination_path.resolve(strict=False)
    metadata_path = destination_path.with_suffix(destination_path.suffix + ".json")
    if destination_path.exists():
        raise FileExistsError(f"refusing to overwrite backup: {destination_path}")
    if metadata_path.exists():
        raise FileExistsError(f"refusing to overwrite backup metadata: {metadata_path}")
    destination_path.parent.mkdir(parents=True, exist_ok=True)
    partial_path = destination_path.with_name(f".{destination_path.name}.{os.getpid()}.partial")
    metadata_partial_path = metadata_path.with_name(
        f".{metadata_path.name}.{os.getpid()}.partial"
    )
    if partial_path.exists():
        raise FileExistsError(f"partial backup already exists: {partial_path}")
    if metadata_partial_path.exists():
        raise FileExistsError(
            f"partial backup metadata already exists: {metadata_partial_path}"
        )

    started_at = int(time.time())
    published_destination = False
    published_metadata = False
    try:
        source = sqlite3.connect(f"file:{source_path}?mode=ro", uri=True, timeout=30)
        destination = sqlite3.connect(partial_path, timeout=30)
        try:
            source.backup(destination)
            destination.commit()
        finally:
            destination.close()
            source.close()

        verification = sqlite3.connect(f"file:{partial_path}?mode=ro", uri=True)
        try:
            quick_check = verification.execute("PRAGMA quick_check").fetchone()
            if quick_check != ("ok",):
                raise RuntimeError(f"backup quick_check failed: {quick_check!r}")
            migration_count = verification.execute(
                "SELECT COUNT(*) FROM schema_migrations"
            ).fetchone()[0]
        finally:
            verification.close()

        metadata = {
            "format_version": 1,
            "source": str(source_path),
            "backup": str(destination_path),
            "created_at_unix": started_at,
            "size_bytes": partial_path.stat().st_size,
            "sha256": sha256(partial_path),
            "quick_check": "ok",
            "schema_migration_count": migration_count,
        }
        with metadata_partial_path.open("x", encoding="utf-8") as output:
            json.dump(metadata, output, sort_keys=True)
            output.write("\n")

        # Publish both artifacts with no-overwrite filesystem operations.  A
        # prior existence check alone is racy: another deploy could create the
        # destination between that check and publication.  Both partial files
        # live beside their final paths, so hard-link creation is atomic and
        # guaranteed to stay on one filesystem.
        os.link(partial_path, destination_path)
        published_destination = True
        os.link(metadata_partial_path, metadata_path)
        published_metadata = True

        partial_path.unlink()
        metadata_partial_path.unlink()
        return metadata
    except BaseException:
        if published_metadata:
            metadata_path.unlink(missing_ok=True)
        if published_destination:
            destination_path.unlink(missing_ok=True)
        partial_path.unlink(missing_ok=True)
        metadata_partial_path.unlink(missing_ok=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    arguments = parser.parse_args()
    try:
        metadata = backup(arguments.source, arguments.destination)
    except (OSError, RuntimeError, sqlite3.Error) as error:
        print(f"backup failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(metadata, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
