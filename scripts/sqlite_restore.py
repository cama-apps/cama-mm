#!/usr/bin/env python3
"""Restore a verified deployment backup while the runtime lock is held.

This helper is deliberately one-way: it never asks the retained Python app to
interpret a database touched by Rust.  The candidate runtime must be stopped,
then this process acquires the same filesystem lock, verifies the immutable
predeploy backup and metadata, atomically replaces the database, removes stale
SQLite sidecars, and verifies the published bytes before releasing the lock.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import sqlite3
import sys
from pathlib import Path
from typing import BinaryIO
from urllib.parse import quote


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _verify_sqlite(path: Path, expected_migrations: int) -> None:
    uri = f"file:{quote(str(path), safe='/')}?mode=ro&immutable=1"
    connection = sqlite3.connect(uri, uri=True, timeout=30)
    try:
        quick_check = connection.execute("PRAGMA quick_check").fetchone()
        if quick_check != ("ok",):
            raise RuntimeError(f"restore quick_check failed: {quick_check!r}")
        migrations = int(connection.execute("SELECT COUNT(*) FROM schema_migrations").fetchone()[0])
        if migrations != expected_migrations:
            raise RuntimeError(
                "restore migration count mismatch: "
                f"expected {expected_migrations}, found {migrations}"
            )
    finally:
        connection.close()


def _load_metadata(backup_path: Path, destination_path: Path) -> dict[str, object]:
    metadata_path = backup_path.with_suffix(backup_path.suffix + ".json")
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    if not isinstance(metadata, dict) or metadata.get("format_version") != 1:
        raise RuntimeError("unsupported or malformed backup metadata")
    if metadata.get("backup") != str(backup_path):
        raise RuntimeError("backup metadata path does not match the restore source")
    if metadata.get("source") != str(destination_path):
        raise RuntimeError("backup metadata source does not match the restore destination")
    if metadata.get("quick_check") != "ok":
        raise RuntimeError("backup metadata does not contain a successful quick_check")
    expected_size = metadata.get("size_bytes")
    expected_digest = metadata.get("sha256")
    expected_migrations = metadata.get("schema_migration_count")
    if not isinstance(expected_size, int) or expected_size <= 0:
        raise RuntimeError("backup metadata has an invalid size")
    if not isinstance(expected_digest, str) or len(expected_digest) != 64:
        raise RuntimeError("backup metadata has an invalid SHA-256 digest")
    if not isinstance(expected_migrations, int) or expected_migrations < 0:
        raise RuntimeError("backup metadata has an invalid migration count")
    if backup_path.stat().st_size != expected_size:
        raise RuntimeError("backup size does not match its metadata")
    if sha256(backup_path) != expected_digest:
        raise RuntimeError("backup SHA-256 does not match its metadata")
    _verify_sqlite(backup_path, expected_migrations)
    return metadata


def _copy_and_sync(source: Path, destination: Path) -> None:
    with source.open("rb") as input_file, destination.open("xb") as output_file:
        for chunk in iter(lambda: input_file.read(1024 * 1024), b""):
            output_file.write(chunk)
        output_file.flush()
        os.fsync(output_file.fileno())


def _acquire_runtime_lock(lock_file: BinaryIO) -> None:
    try:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        raise RuntimeError("runtime lock is still held; refusing database restore") from error


def restore(backup: Path, destination: Path) -> dict[str, object]:
    if backup.is_symlink() or destination.is_symlink():
        raise RuntimeError("backup and destination must not be symbolic links")
    backup_path = backup.resolve(strict=True)
    destination_path = destination.resolve(strict=False)
    if not destination_path.parent.is_dir():
        raise RuntimeError("restore destination parent does not exist")
    protected_namespace = {
        destination_path,
        Path(f"{destination_path}-wal").resolve(strict=False),
        Path(f"{destination_path}-shm").resolve(strict=False),
        Path(f"{destination_path}-journal").resolve(strict=False),
    }
    if backup_path in protected_namespace:
        raise RuntimeError("backup path overlaps the live SQLite namespace")

    partial_path = destination_path.with_name(
        f".{destination_path.name}.{os.getpid()}.restore.partial"
    )
    if partial_path.exists():
        raise FileExistsError(f"partial restore already exists: {partial_path}")

    lock_path = destination_path.parent / ".cama-runtime.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+b") as lock_file:
        _acquire_runtime_lock(lock_file)
        try:
            metadata = _load_metadata(backup_path, destination_path)
            expected_digest = str(metadata["sha256"])
            expected_migrations = int(metadata["schema_migration_count"])
            _copy_and_sync(backup_path, partial_path)
            if sha256(partial_path) != expected_digest:
                raise RuntimeError("copied restore candidate does not match backup SHA-256")
            _verify_sqlite(partial_path, expected_migrations)

            Path(f"{destination_path}-wal").unlink(missing_ok=True)
            Path(f"{destination_path}-shm").unlink(missing_ok=True)
            Path(f"{destination_path}-journal").unlink(missing_ok=True)
            os.replace(partial_path, destination_path)
            directory_fd = os.open(destination_path.parent, os.O_RDONLY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)

            if sha256(destination_path) != expected_digest:
                raise RuntimeError("published restore does not match backup SHA-256")
            _verify_sqlite(destination_path, expected_migrations)
        finally:
            partial_path.unlink(missing_ok=True)

    return {
        "format_version": 1,
        "restored_from": str(backup_path),
        "destination": str(destination_path),
        "size_bytes": destination_path.stat().st_size,
        "sha256": str(metadata["sha256"]),
        "schema_migration_count": int(metadata["schema_migration_count"]),
        "quick_check": "ok",
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("backup", type=Path)
    parser.add_argument("destination", type=Path)
    arguments = parser.parse_args()
    try:
        report = restore(arguments.backup, arguments.destination)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError, sqlite3.Error) as error:
        print(f"restore failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
