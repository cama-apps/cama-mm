#!/usr/bin/env python3
"""Replay a production-shaped SQLite snapshot without touching its source.

The source is opened read-only and copied with the existing online-backup
helper.  Every migration, Rust preflight, repository smoke, and retained
Python read runs against the disposable copy.  The disposable copy also
exercises the durable Survey recovery path so a current production-shaped
database proves both repository and recovery writes.  A JSON report is
printed even when a step fails so this rehearsal is evidence rather than a
cutover claim.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import time
from collections.abc import Sequence
from pathlib import Path
from typing import Any
from urllib.parse import quote

REPORT_FORMAT_VERSION = 1
OUTPUT_LIMIT = 12_000
DEFAULT_TIMEOUT_SECONDS = 900
SMOKE_GUILD_ID = -8_888_888_888_888_888_888
DIG_SMOKE_PLAYER_ID = -8_888_888_888_888_888_870
DIG_SMOKE_ITEM_TYPE = "hard_hat"
DIG_SMOKE_ROUTE_STATE = '{"route_id":"shored_passage","status":"active"}'
SURVEY_SMOKE_GUILD_ID = 9_000_000_000_000_001
SURVEY_SMOKE_USER_ID = 9_000_000_000_000_002
SURVEY_SMOKE_TITLE = "Rust disposable Survey recovery smoke"
SURVEY_SMOKE_PROMPT = (
    "Can the Rust recovery worker deliver this clone-only sentinel?"
)
SURVEY_SMOKE_CHANNEL_ID = 8_000_000_000_000_001
SURVEY_SMOKE_MESSAGE_ID = 8_000_000_000_000_002
PYTHON_RETAINED_READ_FORMAT = "python-guild-config-dig-survey-repository-v2"
VOLATILE_TIME_COLUMN_SUFFIXES = ("_at", "_date", "_time", "_timestamp")
VOLATILE_TIME_COLUMN_NAMES = frozenset(
    {
        "created_at",
        "updated_at",
        "timestamp",
        "week_key",
        "prev_week_key",
        "last_wheel_spin",
        "last_double_or_nothing",
        "last_trivia_session",
    }
)
EXPECTED_SMOKE_DELTAS = {
    "dig_inventory": 1,
    "duel_challenges": 1,
    "economy_ledger_entries": 17,
    "economy_policy_state": 1,
    "guild_config": 1,
    "loan_state": 1,
    "low_priority_state": 1,
    "nonprofit_fund": 1,
    "package_deal_purchases": 1,
    "package_deals": 1,
    "pet_brawls": 1,
    "pet_supplies": 1,
    "pets": 3,
    "player_pairings": 3,
    "players": 7,
    "soft_avoids": 1,
    "tip_transactions": 1,
    "tunnels": 1,
    "wheel_spins": 1,
}
# Survey recovery is deliberately kept as a separate delta contract. The
# shared repository smoke above predates the Rust Survey READY-recovery path;
# folding these rows into EXPECTED_SMOKE_DELTAS would make it too easy to
# accidentally omit or double-count the provider-owned write.
EXPECTED_SURVEY_SMOKE_DELTAS = {
    "survey_answers": 0,
    "survey_questions": 1,
    "survey_recipients": 1,
    "surveys": 1,
}


def expected_retained_python_read() -> dict[str, Any]:
    """Return the normalized, non-time-dependent Python read contract.

    Auto-incremented IDs and timestamps are intentionally absent.  The
    subprocess read below validates their relationships through the retained
    repositories, while this contract records the stable fields that prove
    the Rust writes are visible to the production Python APIs.
    """

    return {
        "guild_config": {
            "guild_id": SMOKE_GUILD_ID,
            "league_id": 777,
            "auto_enrich_matches": 0,
            "ai_features_enabled": 1,
        },
        "dig": {
            "inventory": {
                "row_count": 1,
                "discord_id": DIG_SMOKE_PLAYER_ID,
                "guild_id": SMOKE_GUILD_ID,
                "item_type": DIG_SMOKE_ITEM_TYPE,
                "queued": True,
            },
            "tunnel": {
                "discord_id": DIG_SMOKE_PLAYER_ID,
                "guild_id": SMOKE_GUILD_ID,
                "depth": 100,
                "route_state": DIG_SMOKE_ROUTE_STATE,
            },
        },
        "survey": {
            "survey": {
                "guild_id": SURVEY_SMOKE_GUILD_ID,
                "title": SURVEY_SMOKE_TITLE,
                "status": "open",
                "target_type": "member",
                "registered_only": False,
            },
            "question": {
                "position": 1,
                "prompt": SURVEY_SMOKE_PROMPT,
                "question_type": "nps",
                "required": True,
            },
            "recipient": {
                "guild_id": SURVEY_SMOKE_GUILD_ID,
                "discord_id": SURVEY_SMOKE_USER_ID,
                "delivery_status": "sent",
                "attempt_count": 1,
                "dm_channel_id": SURVEY_SMOKE_CHANNEL_ID,
                "dm_message_id": SURVEY_SMOKE_MESSAGE_ID,
                "ui_channel_id": SURVEY_SMOKE_CHANNEL_ID,
                "ui_message_id": SURVEY_SMOKE_MESSAGE_ID,
                "current_question_is_first": True,
                "in_review": False,
                "submitted": False,
                "answer_count": 0,
            },
        },
    }


class StepFailure(RuntimeError):
    """A subprocess failed and already has a normalized report entry."""

    def __init__(self, step: dict[str, Any]) -> None:
        self.step = step
        super().__init__(
            f"{step['name']} failed with exit status {step.get('returncode')}"
        )


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _read_only_uri(path: Path) -> str:
    return f"file:{quote(str(path), safe='/')}?mode=ro"


def _quote_identifier(identifier: str) -> str:
    return f'"{identifier.replace(chr(34), chr(34) * 2)}"'


def _volatile_time_columns(columns: Sequence[str]) -> list[str]:
    return sorted(
        column
        for column in columns
        if column in VOLATILE_TIME_COLUMN_NAMES
        or column.endswith(VOLATILE_TIME_COLUMN_SUFFIXES)
    )


def _canonical_sentinel_digest(
    rows: Sequence[dict[str, object]], volatile_columns: Sequence[str]
) -> str:
    volatile = set(volatile_columns)
    canonical_rows = sorted(
        json.dumps(
            {
                key: "<volatile-time>"
                if key in volatile
                else _json_value(value)
                for key, value in row.items()
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        for row in rows
    )
    return hashlib.sha256("\n".join(canonical_rows).encode("utf-8")).hexdigest()


def _assert_volatile_digest_invariant() -> None:
    columns = ["id", "created_at", "semantic_value"]
    first = [{"id": 7, "created_at": "2026-08-13 12:00:00", "semantic_value": 42}]
    second = [{"id": 7, "created_at": "2026-08-13 12:00:01", "semantic_value": 42}]
    volatile = _volatile_time_columns(columns)
    if _canonical_sentinel_digest(first, volatile) != _canonical_sentinel_digest(
        second, volatile
    ):
        raise RuntimeError("volatile sentinel digest normalization self-check failed")


def inspect_database(path: Path) -> dict[str, Any]:
    """Return immutable-file and SQLite health facts without logical writes."""

    if not path.is_file():
        raise FileNotFoundError(f"database does not exist: {path}")

    connection = sqlite3.connect(_read_only_uri(path), uri=True, timeout=30)
    try:
        connection.execute("PRAGMA query_only=ON")
        quick_check = str(connection.execute("PRAGMA quick_check(1)").fetchone()[0])
        integrity_rows = [
            str(row[0]) for row in connection.execute("PRAGMA integrity_check")
        ]
        migration_count = int(
            connection.execute("SELECT COUNT(*) FROM schema_migrations").fetchone()[0]
        )
        journal_mode = str(connection.execute("PRAGMA journal_mode").fetchone()[0])
        user_version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        table_count = int(
            connection.execute(
                "SELECT COUNT(*) FROM sqlite_master "
                "WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
            ).fetchone()[0]
        )
    finally:
        connection.close()

    return {
        "path": str(path),
        "sha256": _sha256(path),
        "size_bytes": path.stat().st_size,
        "migration_count": migration_count,
        "quick_check": quick_check,
        "integrity_check": integrity_rows,
        "integrity_ok": integrity_rows == ["ok"],
        "journal_mode": journal_mode,
        "user_version": user_version,
        "table_count": table_count,
    }


def table_row_counts(path: Path) -> dict[str, int]:
    """Count every user table without reading any row contents."""

    connection = sqlite3.connect(_read_only_uri(path), uri=True, timeout=30)
    try:
        names = [
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type = 'table' AND name NOT LIKE 'sqlite_%' "
                "ORDER BY name"
            )
        ]
        return {
            name: int(
                connection.execute(
                    f"SELECT COUNT(*) FROM {_quote_identifier(name)}"
                ).fetchone()[0]
            )
            for name in names
        }
    finally:
        connection.close()


def sentinel_digests(path: Path) -> dict[str, dict[str, Any]]:
    """Hash only rows belonging to the reserved smoke guild.

    No production row values are placed in the report. Sorting canonical row
    JSON makes the digest independent of SQLite scan order while retaining
    auto-generated IDs as part of the disposable-copy evidence.
    """

    connection = sqlite3.connect(_read_only_uri(path), uri=True, timeout=30)
    connection.row_factory = sqlite3.Row
    try:
        result: dict[str, dict[str, Any]] = {}
        for table in sorted(EXPECTED_SMOKE_DELTAS):
            columns = [
                str(row[1])
                for row in connection.execute(
                    f"PRAGMA table_info({_quote_identifier(table)})"
                )
            ]
            if "guild_id" not in columns:
                raise RuntimeError(f"smoke table lacks guild_id filter: {table}")
            rows = connection.execute(
                f"SELECT * FROM {_quote_identifier(table)} WHERE guild_id = ?",
                (SMOKE_GUILD_ID,),
            ).fetchall()
            volatile_columns = _volatile_time_columns(columns)
            row_values = [
                {key: row[key] for key in row.keys()} for row in rows
            ]
            digest = _canonical_sentinel_digest(row_values, volatile_columns)
            result[table] = {
                "row_count": len(row_values),
                "sha256": digest,
                "ignored_volatile_columns": volatile_columns,
            }
        return result
    finally:
        connection.close()


def _json_value(value: object) -> object:
    if isinstance(value, bytes):
        return {"bytes_hex": value.hex()}
    return value


def _short_output(value: str) -> str:
    value = value.strip()
    if len(value) <= OUTPUT_LIMIT:
        return value
    return value[:OUTPUT_LIMIT] + "\n...[truncated]"


def _command_for_report(command: Sequence[object]) -> list[str]:
    return [str(part) for part in command]


def run_step(
    name: str,
    command: Sequence[object],
    *,
    cwd: Path,
    timeout_seconds: float,
    report: dict[str, Any],
) -> dict[str, Any]:
    """Run one external operation and append its bounded output to the report."""

    command_for_report = _command_for_report(command)
    started = time.monotonic()
    try:
        completed = subprocess.run(
            command_for_report,
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
        )
        step = {
            "name": name,
            "status": "passed" if completed.returncode == 0 else "failed",
            "command": command_for_report,
            "returncode": completed.returncode,
            "duration_seconds": round(time.monotonic() - started, 3),
            "stdout": _short_output(completed.stdout),
            "stderr": _short_output(completed.stderr),
        }
    except subprocess.TimeoutExpired as error:
        step = {
            "name": name,
            "status": "failed",
            "command": command_for_report,
            "returncode": None,
            "duration_seconds": round(time.monotonic() - started, 3),
            "stdout": _short_output(_as_text(error.stdout)),
            "stderr": _short_output(_as_text(error.stderr)),
            "error": f"timed out after {timeout_seconds:g} seconds",
        }
    except OSError as error:
        step = {
            "name": name,
            "status": "failed",
            "command": command_for_report,
            "returncode": None,
            "duration_seconds": round(time.monotonic() - started, 3),
            "stdout": "",
            "stderr": "",
            "error": str(error),
        }

    report["steps"].append(step)
    if step["status"] != "passed":
        raise StepFailure(step)
    return step


def _as_text(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return str(value)


def _parse_key_values(output: str) -> dict[str, str]:
    """Parse the stable space-separated key=value line from `cama-rust`."""

    values: dict[str, str] = {}
    for token in shlex.split(output.strip()):
        if "=" not in token:
            continue
        key, value = token.split("=", 1)
        values[key] = value
    return values


def _parse_retained_python_read(output: str) -> dict[str, Any]:
    """Parse and validate the normalized retained Python read."""

    try:
        value = json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"retained Python repository read was not JSON: {output}"
        ) from error
    if not isinstance(value, dict):
        raise RuntimeError(
            f"retained Python repository read was not an object: {value!r}"
        )
    expected = expected_retained_python_read()
    if value != expected:
        raise RuntimeError(
            "retained Python repository read mismatch: "
            f"expected {expected!r}, found {value!r}"
        )
    return value


def _python_executable(argument: str | None, root: Path) -> str:
    value = argument or os.environ.get("CAMA_PARITY_PYTHON")
    if value is None:
        return sys.executable
    executable = Path(value)
    if not executable.is_absolute():
        executable = root / executable
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise FileNotFoundError(f"Python executable is not runnable: {executable}")
    return str(executable)


def _write_report(path: Path, report: dict[str, Any]) -> None:
    path = path.expanduser().resolve(strict=False)
    if not path.parent.is_dir():
        raise FileNotFoundError(f"report directory does not exist: {path.parent}")
    partial = path.with_name(f".{path.name}.{os.getpid()}.partial")
    if partial.exists():
        raise FileExistsError(f"partial report already exists: {partial}")
    try:
        with partial.open("x", encoding="utf-8") as output:
            json.dump(report, output, indent=2, sort_keys=True)
            output.write("\n")
        # A same-directory hard link publishes atomically and fails with
        # EEXIST if another invocation won the destination race. `replace`
        # would silently overwrite that report.
        try:
            os.link(partial, path)
        except FileExistsError as error:
            raise FileExistsError(f"refusing to overwrite report: {path}") from error
    except BaseException:
        partial.unlink(missing_ok=True)
        raise
    else:
        partial.unlink(missing_ok=True)


def _source_unchanged(before: dict[str, Any], after: dict[str, Any]) -> bool:
    # The main SQLite file is the source of truth. SQLite may create/update a
    # read-only WAL shared-memory sidecar while coordinating a reader; those
    # lock bytes are not logical database mutations and are intentionally not
    # used for the immutability decision.
    return all(
        before.get(key) == after.get(key)
        for key in (
            "sha256",
            "size_bytes",
            "migration_count",
            "quick_check",
            "integrity_check",
            "integrity_ok",
        )
    )


def run(arguments: argparse.Namespace) -> tuple[int, dict[str, Any]]:
    root = Path(__file__).resolve().parents[1]
    source = arguments.source.expanduser().resolve(strict=True)
    report_path = (
        arguments.report.expanduser().resolve(strict=False)
        if arguments.report is not None
        else None
    )
    if report_path is not None and report_path in {
        source,
        Path(f"{source}-wal"),
        Path(f"{source}-shm"),
    }:
        raise ValueError("report path must not be the source database or its sidecar")

    report: dict[str, Any] = {
        "format_version": REPORT_FORMAT_VERSION,
        "status": "failed",
        "source": {"path": str(source), "before": None, "after": None},
        "disposable_copy": {
            "path": None,
            "retained": False,
            "before": None,
            "after_python_migration": None,
            "after_rust": None,
        },
        "steps": [],
        "python_post_rust_read": {
            "status": "not_run",
            "format": PYTHON_RETAINED_READ_FORMAT,
            "expected": expected_retained_python_read(),
            "value": None,
        },
        "python_bridge": {
            "status": "not_run",
            "reason": (
                "the retained 13-family bridge requires its own seeded fixture; "
                "the snapshot replay uses the smoke sentinel through the Python repository"
            ),
        },
        "survey_recovery": {
            "status": "not_run",
            "expected_table_deltas": EXPECTED_SURVEY_SMOKE_DELTAS,
        },
        "self_checks": {"volatile_digest": "not_run"},
        "errors": [],
    }

    source_before: dict[str, Any] | None = None
    try:
        _assert_volatile_digest_invariant()
        report["self_checks"]["volatile_digest"] = "passed"
        source_before = inspect_database(source)
        report["source"]["before"] = source_before

        python = _python_executable(arguments.python, root)
        cargo = arguments.cargo
        backup_helper = root / "scripts" / "sqlite_backup.py"
        retained_read_script = root / "scripts" / "retained_python_snapshot_read.py"
        runtime_manifest = root / "rust" / "Cargo.toml"
        if not backup_helper.is_file():
            raise FileNotFoundError(f"backup helper does not exist: {backup_helper}")
        if not runtime_manifest.is_file():
            raise FileNotFoundError(f"Rust manifest does not exist: {runtime_manifest}")
        if not retained_read_script.is_file():
            raise FileNotFoundError(
                f"retained Python read helper does not exist: {retained_read_script}"
            )

        with tempfile.TemporaryDirectory(prefix="cama-rust-snapshot-replay-") as work:
            work_path = Path(work)
            copy = work_path / "cama_shuffle.db"
            report["disposable_copy"]["path"] = str(copy)

            run_step(
                "online_backup",
                [python, str(backup_helper), str(source), str(copy)],
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            report["disposable_copy"]["before"] = inspect_database(copy)

            migration_code = (
                "import sys; from database import Database; "
                "database = Database(sys.argv[1]); database.close()"
            )
            run_step(
                "python_migration",
                [python, "-c", migration_code, str(copy)],
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            report["disposable_copy"]["after_python_migration"] = inspect_database(copy)

            db_check = run_step(
                "rust_db_check",
                [
                    cargo,
                    "run",
                    "--locked",
                    "--manifest-path",
                    str(runtime_manifest),
                    "-p",
                    "cama-runtime",
                    "--bin",
                    "cama-rust",
                    "--",
                    "db-check",
                    "--db-path",
                    str(copy),
                ],
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            db_check_values = _parse_key_values(db_check["stdout"])
            if db_check_values.get("schema_compatible") != "true":
                raise RuntimeError(
                    "Rust db-check did not report schema_compatible=true: "
                    f"{db_check['stdout']}"
                )
            report["rust_db_check"] = db_check_values

            before_counts = table_row_counts(copy)
            smoke = run_step(
                "rust_repository_smoke",
                [
                    cargo,
                    "run",
                    "--locked",
                    "--manifest-path",
                    str(runtime_manifest),
                    "-p",
                    "cama-db",
                    "--example",
                    "guild_config_snapshot_smoke",
                    "--",
                    str(copy),
                    "--disposable-copy",
                ],
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            report["disposable_copy"]["after_rust"] = inspect_database(copy)
            smoke_marker_found = "shared_repository_roundtrip=ok" in smoke["stdout"]
            if not smoke_marker_found:
                raise RuntimeError(
                    "Rust repository smoke exited successfully without its "
                    "round-trip marker"
                )
            after_counts = table_row_counts(copy)
            table_deltas = {
                table: {
                    "before": before_counts.get(table, 0),
                    "after": after_counts.get(table, 0),
                    "delta": after_counts.get(table, 0) - before_counts.get(table, 0),
                }
                for table in sorted(set(before_counts) | set(after_counts))
                if after_counts.get(table, 0) != before_counts.get(table, 0)
            }
            actual_deltas = {
                table: details["delta"] for table, details in table_deltas.items()
            }
            report["repository_smoke"] = {
                "status": "failed",
                "roundtrip_marker": smoke_marker_found,
                "sentinel_filter": {"column": "guild_id", "value": SMOKE_GUILD_ID},
                "expected_table_deltas": EXPECTED_SMOKE_DELTAS,
                "table_deltas": table_deltas,
            }
            if actual_deltas != EXPECTED_SMOKE_DELTAS:
                raise RuntimeError(
                    "Rust repository smoke table delta mismatch: "
                    f"expected {EXPECTED_SMOKE_DELTAS!r}, found {actual_deltas!r}"
                )
            sentinel_rows = sentinel_digests(copy)
            if any(details["row_count"] == 0 for details in sentinel_rows.values()):
                raise RuntimeError("Rust repository smoke produced an empty sentinel table")
            report["repository_smoke"].update(
                {"status": "passed", "sentinel_digests": sentinel_rows}
            )

            survey_before_counts = {
                table: after_counts.get(table, 0)
                for table in EXPECTED_SURVEY_SMOKE_DELTAS
            }
            survey_smoke = run_step(
                "rust_survey_recovery_smoke",
                [
                    cargo,
                    "run",
                    "--locked",
                    "--manifest-path",
                    str(runtime_manifest),
                    "-p",
                    "cama-runtime",
                    "--example",
                    "survey_snapshot_smoke",
                    "--",
                    str(copy),
                    "--disposable-copy",
                ],
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            survey_after_counts = table_row_counts(copy)
            survey_table_deltas = {
                table: {
                    "before": survey_before_counts[table],
                    "after": survey_after_counts.get(table, 0),
                    "delta": survey_after_counts.get(table, 0)
                    - survey_before_counts[table],
                }
                for table in sorted(EXPECTED_SURVEY_SMOKE_DELTAS)
            }
            actual_survey_deltas = {
                table: details["delta"]
                for table, details in survey_table_deltas.items()
                if details["delta"] != 0
            }
            survey_marker_found = "survey_snapshot_smoke=ok" in survey_smoke["stdout"]
            report["survey_recovery"] = {
                "status": "failed",
                "recovery_marker": survey_marker_found,
                "expected_table_deltas": EXPECTED_SURVEY_SMOKE_DELTAS,
                "table_deltas": survey_table_deltas,
            }
            expected_nonzero_survey_deltas = {
                table: delta
                for table, delta in EXPECTED_SURVEY_SMOKE_DELTAS.items()
                if delta != 0
            }
            if not survey_marker_found:
                raise RuntimeError(
                    "Rust Survey recovery smoke exited successfully without its "
                    "recovery marker"
                )
            if actual_survey_deltas != expected_nonzero_survey_deltas:
                raise RuntimeError(
                    "Rust Survey recovery table delta mismatch: "
                    f"expected {expected_nonzero_survey_deltas!r}, "
                    f"found {actual_survey_deltas!r}"
                )
            report["survey_recovery"]["status"] = "passed"
            report["disposable_copy"]["after_rust"] = inspect_database(copy)

            # The retained bridge is a serial 13-family fixture gate and
            # expects its own seeded IDs. A real development snapshot does
            # not necessarily contain those rows (and auto-increment IDs can
            # differ), so this replay reads the Rust smoke sentinels through
            # the actual retained Python repository/domain APIs.
            try:
                python_read = run_step(
                    "python_post_rust_retained_repository_read",
                    [python, str(retained_read_script), str(copy)],
                    cwd=root,
                    timeout_seconds=arguments.timeout_seconds,
                    report=report,
                )
            except StepFailure as failure:
                step = failure.step
                detail = str(
                    step.get("error")
                    or step.get("stderr")
                    or step.get("stdout")
                    or failure
                )
                report["python_post_rust_read"].update(
                    {"status": "failed", "error": detail}
                )
                raise
            try:
                python_value = _parse_retained_python_read(python_read["stdout"])
            except RuntimeError as error:
                report["python_post_rust_read"].update(
                    {"status": "failed", "error": str(error)}
                )
                raise
            report["python_post_rust_read"] = {
                "status": "passed",
                "format": PYTHON_RETAINED_READ_FORMAT,
                "expected": expected_retained_python_read(),
                "value": python_value,
            }

            report["status"] = "passed"
    except (OSError, RuntimeError, sqlite3.Error, StepFailure, ValueError) as error:
        report["errors"].append(str(error))
    finally:
        try:
            source_after = inspect_database(source)
            report["source"]["after"] = source_after
            if source_before is not None:
                report["source"]["unchanged"] = _source_unchanged(
                    source_before, source_after
                )
                if not report["source"]["unchanged"]:
                    report["errors"].append(
                        "source database changed while replay was running"
                    )
                    report["status"] = "failed"
        except (OSError, sqlite3.Error) as error:
            report["source"]["after_error"] = str(error)
            report["errors"].append(f"could not verify source after replay: {error}")
            report["status"] = "failed"

    if report_path is not None:
        try:
            report["report_path"] = str(report_path)
            _write_report(report_path, report)
        except (OSError, ValueError) as error:
            report["errors"].append(f"could not write report: {error}")
            report["status"] = "failed"
    return (0 if report["status"] == "passed" else 1), report


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Replay a SQLite snapshot on an isolated online-backup copy; "
            "the source is never opened writable."
        )
    )
    parser.add_argument("source", type=Path, help="source SQLite database (read-only)")
    parser.add_argument(
        "--python",
        help="retained Python executable (default: CAMA_PARITY_PYTHON or this interpreter)",
    )
    parser.add_argument("--cargo", default="cargo", help="Cargo executable")
    parser.add_argument(
        "--report",
        type=Path,
        help="write the JSON report to a new path as well as stdout",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=DEFAULT_TIMEOUT_SECONDS,
        help=f"per-step subprocess timeout (default: {DEFAULT_TIMEOUT_SECONDS:g})",
    )
    arguments = parser.parse_args()
    if arguments.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be positive")
    try:
        status, report = run(arguments)
    except (OSError, RuntimeError, sqlite3.Error, ValueError) as error:
        report = {
            "format_version": REPORT_FORMAT_VERSION,
            "status": "failed",
            "errors": [str(error)],
        }
        status = 1
    print(json.dumps(report, indent=2, sort_keys=True))
    return status


if __name__ == "__main__":
    raise SystemExit(main())
