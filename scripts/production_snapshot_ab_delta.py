#!/usr/bin/env python3
"""Compare bounded Python/Rust writes on independent SQLite snapshot copies.

This is a representative A/B evidence runner, not a cutover claim.  It
creates two online-backup copies of one current production-shaped database,
lets the retained Python repositories finish their write subprocess, then
touches only the Rust copy with the existing repository/recovery smokes.  The
comparison is limited to schema-aware projections for guild configuration,
Dig inventory/route state, and Survey delivery.  Timestamps, generated IDs,
and SQLite file bytes are deliberately not used as semantic equality.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import tempfile
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

# Running this file by path puts ``scripts/`` on sys.path, not the project
# root.  Add the root before importing the existing replay helpers.
PROJECT_ROOT = str(Path(__file__).resolve().parents[1])
if PROJECT_ROOT not in sys.path:
    sys.path.insert(0, PROJECT_ROOT)

from scripts.production_snapshot_replay import (
    DEFAULT_TIMEOUT_SECONDS,
    _python_executable,
    _read_only_uri,
    _source_unchanged,
    _write_report,
    inspect_database,
    run_step,
)

REPORT_FORMAT_VERSION = 1
SMOKE_GUILD_ID = -8_888_888_888_888_888_888
DIG_PLAYER_ID = -8_888_888_888_888_888_870
SURVEY_GUILD_ID = 9_000_000_000_000_001
SURVEY_USER_ID = 9_000_000_000_000_002
SURVEY_TITLE = "Rust disposable Survey recovery smoke"
SURVEY_PROMPT = "Can the Rust recovery worker deliver this clone-only sentinel?"
SURVEY_CHANNEL_ID = 8_000_000_000_000_001
SURVEY_MESSAGE_ID = 8_000_000_000_000_002

# Only these projections are the contract for this bounded A/B slice.  The
# source snapshot may contain unrelated rows, and the Rust repository smoke
# deliberately covers more families than this harness compares.
SCOPE_COLUMNS: dict[str, tuple[str, ...]] = {
    "guild_config": (
        "guild_id",
        "league_id",
        "auto_enrich_matches",
        "ai_features_enabled",
    ),
    "dig_inventory": ("discord_id", "guild_id", "item_type", "queued"),
    "tunnels": ("discord_id", "guild_id", "depth", "route_state"),
    "surveys": (
        "guild_id",
        "title",
        "status",
        "target_type",
        "registered_only",
    ),
    "survey_questions": ("position", "prompt", "question_type", "required"),
    "survey_recipients": (
        "guild_id",
        "discord_id",
        "delivery_status",
        "attempt_count",
        "dm_channel_id",
        "dm_message_id",
        "ui_channel_id",
        "ui_message_id",
        "current_question_id",
        "in_review",
        "submitted_at",
        "receipt_checked_attempt",
    ),
    "survey_answers": (
        "guild_id",
        "recipient_id",
        "numeric_value",
        "text_value",
        "skipped",
    ),
}


def _scope_filter(table: str) -> tuple[str, tuple[object, ...]]:
    if table == "guild_config":
        return "guild_id = ?", (SMOKE_GUILD_ID,)
    if table == "dig_inventory":
        return "discord_id = ? AND guild_id = ?", (DIG_PLAYER_ID, SMOKE_GUILD_ID)
    if table == "tunnels":
        return "discord_id = ? AND guild_id = ?", (DIG_PLAYER_ID, SMOKE_GUILD_ID)
    if table == "surveys":
        return "guild_id = ? AND title = ?", (SURVEY_GUILD_ID, SURVEY_TITLE)
    survey_subquery = (
        "survey_id IN (SELECT survey_id FROM surveys "
        "WHERE guild_id = ? AND title = ?)"
    )
    if table == "survey_questions":
        return f"guild_id = ? AND {survey_subquery}", (
            SURVEY_GUILD_ID,
            SURVEY_GUILD_ID,
            SURVEY_TITLE,
        )
    if table == "survey_recipients":
        return f"guild_id = ? AND discord_id = ? AND {survey_subquery}", (
            SURVEY_GUILD_ID,
            SURVEY_USER_ID,
            SURVEY_GUILD_ID,
            SURVEY_TITLE,
        )
    if table == "survey_answers":
        return f"guild_id = ? AND {survey_subquery}", (
            SURVEY_GUILD_ID,
            SURVEY_GUILD_ID,
            SURVEY_TITLE,
        )
    raise KeyError(f"unknown snapshot A/B scope table: {table}")


def _canonical_value(table: str, column: str, value: object) -> object:
    """Normalize unstable storage details while retaining semantic fields."""

    if column == "route_state" and isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return value
    if table == "survey_questions" and column == "position":
        return int(value) if value is not None else None
    if column in {"current_question_id", "question_id"}:
        return "<question>" if value is not None else None
    return value


def _project_rows(table: str, rows: Sequence[Mapping[str, object]]) -> list[dict[str, object]]:
    columns = SCOPE_COLUMNS[table]
    projected = [
        {
            column: _canonical_value(table, column, row.get(column))
            for column in columns
        }
        for row in rows
    ]
    return sorted(
        projected,
        key=lambda row: json.dumps(row, sort_keys=True, separators=(",", ":")),
    )


def scope_snapshot(path: Path) -> dict[str, dict[str, object]]:
    """Read only reserved rows through explicit schema-aware projections."""

    connection = sqlite3.connect(_read_only_uri(path), uri=True, timeout=30)
    connection.row_factory = sqlite3.Row
    try:
        snapshot: dict[str, dict[str, object]] = {}
        for table, columns in SCOPE_COLUMNS.items():
            where, parameters = _scope_filter(table)
            rows = connection.execute(
                f"SELECT {', '.join(columns)} FROM \"{table}\" WHERE {where}",
                parameters,
            ).fetchall()
            projected = _project_rows(table, [dict(row) for row in rows])
            snapshot[table] = {
                "row_count": len(projected),
                "rows": projected,
            }
        return snapshot
    finally:
        connection.close()


def scope_delta(
    before: Mapping[str, Mapping[str, object]],
    after: Mapping[str, Mapping[str, object]],
) -> dict[str, dict[str, object]]:
    """Describe the selected table deltas without comparing whole files."""

    delta: dict[str, dict[str, object]] = {}
    for table in SCOPE_COLUMNS:
        before_rows = list(before[table]["rows"])
        after_rows = list(after[table]["rows"])
        delta[table] = {
            "before_count": len(before_rows),
            "after_count": len(after_rows),
            "delta": len(after_rows) - len(before_rows),
            "before_rows": before_rows,
            "after_rows": after_rows,
        }
    return delta


def expected_scope_after() -> dict[str, dict[str, object]]:
    """Stable semantic result expected from both production write paths."""

    return {
        "guild_config": {
            "row_count": 1,
            "rows": [
                {
                    "guild_id": SMOKE_GUILD_ID,
                    "league_id": 777,
                    "auto_enrich_matches": 0,
                    "ai_features_enabled": 1,
                }
            ],
        },
        "dig_inventory": {
            "row_count": 1,
            "rows": [
                {
                    "discord_id": DIG_PLAYER_ID,
                    "guild_id": SMOKE_GUILD_ID,
                    "item_type": "hard_hat",
                    "queued": 1,
                }
            ],
        },
        "tunnels": {
            "row_count": 1,
            "rows": [
                {
                    "discord_id": DIG_PLAYER_ID,
                    "guild_id": SMOKE_GUILD_ID,
                    "depth": 100,
                    "route_state": {"route_id": "shored_passage", "status": "active"},
                }
            ],
        },
        "surveys": {
            "row_count": 1,
            "rows": [
                {
                    "guild_id": SURVEY_GUILD_ID,
                    "title": SURVEY_TITLE,
                    "status": "open",
                    "target_type": "member",
                    "registered_only": 0,
                }
            ],
        },
        "survey_questions": {
            "row_count": 1,
            "rows": [
                {
                    "position": 1,
                    "prompt": SURVEY_PROMPT,
                    "question_type": "nps",
                    "required": 1,
                }
            ],
        },
        "survey_recipients": {
            "row_count": 1,
            "rows": [
                {
                    "guild_id": SURVEY_GUILD_ID,
                    "discord_id": SURVEY_USER_ID,
                    "delivery_status": "sent",
                    "attempt_count": 1,
                    "dm_channel_id": SURVEY_CHANNEL_ID,
                    "dm_message_id": SURVEY_MESSAGE_ID,
                    "ui_channel_id": SURVEY_CHANNEL_ID,
                    "ui_message_id": SURVEY_MESSAGE_ID,
                    "current_question_id": "<question>",
                    "in_review": 0,
                    "submitted_at": None,
                    "receipt_checked_attempt": 1,
                }
            ],
        },
        "survey_answers": {"row_count": 0, "rows": []},
    }


def _copy_path(work_path: Path, name: str) -> Path:
    return work_path / f"{name}.db"


def _backup_command(python: str, root: Path, source: Path, destination: Path) -> list[str]:
    return [python, str(root / "scripts" / "sqlite_backup.py"), str(source), str(destination)]


def _migration_command(python: str, copy: Path) -> list[str]:
    code = "import sys; from database import Database; db = Database(sys.argv[1]); db.close()"
    return [python, "-c", code, str(copy)]


def _rust_db_check_command(cargo: str, root: Path, copy: Path) -> list[str]:
    return [
        cargo,
        "run",
        "--locked",
        "--manifest-path",
        str(root / "rust" / "Cargo.toml"),
        "-p",
        "cama-runtime",
        "--bin",
        "cama-rust",
        "--",
        "db-check",
        "--db-path",
        str(copy),
    ]


def _rust_repository_smoke_command(cargo: str, root: Path, copy: Path) -> list[str]:
    return [
        cargo,
        "run",
        "--locked",
        "--manifest-path",
        str(root / "rust" / "Cargo.toml"),
        "-p",
        "cama-db",
        "--example",
        "guild_config_snapshot_smoke",
        "--",
        str(copy),
        "--disposable-copy",
    ]


def _rust_survey_smoke_command(cargo: str, root: Path, copy: Path) -> list[str]:
    return [
        cargo,
        "run",
        "--locked",
        "--manifest-path",
        str(root / "rust" / "Cargo.toml"),
        "-p",
        "cama-runtime",
        "--example",
        "survey_snapshot_smoke",
        "--",
        str(copy),
        "--disposable-copy",
    ]


def _same_source(before: Mapping[str, object], after: Mapping[str, object]) -> bool:
    return _source_unchanged(dict(before), dict(after))


def run(arguments: argparse.Namespace) -> tuple[int, dict[str, Any]]:
    root = Path(__file__).resolve().parents[1]
    source = arguments.source.expanduser().resolve(strict=True)
    report_path = (
        arguments.report.expanduser().resolve(strict=False)
        if arguments.report is not None
        else None
    )
    report: dict[str, Any] = {
        "format_version": REPORT_FORMAT_VERSION,
        "status": "failed",
        "readiness_gate_closed": False,
        "readiness_evidence": (
            "bounded current-schema A/B representative passed; full gate remains "
            "open for old-schema coverage, all repository families, and retained "
            "Python rollback/readback transitions"
        ),
        "source": {"path": str(source), "before": None, "after": None},
        "copies": {
            "python": {"path": None, "before": None, "after": None},
            "rust": {"path": None, "before": None, "after": None},
        },
        "scope": {
            "tables": list(SCOPE_COLUMNS),
            "expected_after": expected_scope_after(),
            "python": None,
            "rust": None,
            "equal": False,
        },
        "steps": [],
        "errors": [],
    }

    source_before: dict[str, Any] | None = None
    try:
        source_before = inspect_database(source)
        report["source"]["before"] = source_before
        python = _python_executable(arguments.python, root)
        cargo = arguments.cargo
        backup_helper = root / "scripts" / "sqlite_backup.py"
        python_write_helper = root / "scripts" / "python_snapshot_ab_write.py"
        if not backup_helper.is_file():
            raise FileNotFoundError(f"backup helper does not exist: {backup_helper}")
        if not python_write_helper.is_file():
            raise FileNotFoundError(
                f"Python A/B write helper does not exist: {python_write_helper}"
            )

        with tempfile.TemporaryDirectory(prefix="cama-rust-snapshot-ab-") as work:
            work_path = Path(work)
            python_copy = _copy_path(work_path, "python")
            rust_copy = _copy_path(work_path, "rust")
            report["copies"]["python"]["path"] = str(python_copy)
            report["copies"]["rust"]["path"] = str(rust_copy)

            run_step(
                "online_backup_python_copy",
                _backup_command(python, root, source, python_copy),
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            run_step(
                "online_backup_rust_copy",
                _backup_command(python, root, source, rust_copy),
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            for side, copy in (("python", python_copy), ("rust", rust_copy)):
                run_step(
                    f"python_migration_{side}_copy",
                    _migration_command(python, copy),
                    cwd=root,
                    timeout_seconds=arguments.timeout_seconds,
                    report=report,
                )
                report["copies"][side]["before"] = inspect_database(copy)
                run_step(
                    f"rust_db_check_{side}_copy",
                    _rust_db_check_command(cargo, root, copy),
                    cwd=root,
                    timeout_seconds=arguments.timeout_seconds,
                    report=report,
                )

            python_before = scope_snapshot(python_copy)
            rust_before = scope_snapshot(rust_copy)
            if any(details["row_count"] for details in python_before.values()) or any(
                details["row_count"] for details in rust_before.values()
            ):
                raise RuntimeError(
                    "reserved A/B sentinel rows already exist in a disposable copy"
                )

            # This subprocess must complete before any Rust-side operation.
            run_step(
                "python_repository_ab_write",
                [python, str(python_write_helper), str(python_copy)],
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            python_after = scope_snapshot(python_copy)
            report["scope"]["python"] = {
                "before": python_before,
                "after": python_after,
                "delta": scope_delta(python_before, python_after),
            }

            run_step(
                "rust_repository_ab_write",
                _rust_repository_smoke_command(cargo, root, rust_copy),
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            run_step(
                "rust_survey_delivery_ab_write",
                _rust_survey_smoke_command(cargo, root, rust_copy),
                cwd=root,
                timeout_seconds=arguments.timeout_seconds,
                report=report,
            )
            rust_after = scope_snapshot(rust_copy)
            report["scope"]["rust"] = {
                "before": rust_before,
                "after": rust_after,
                "delta": scope_delta(rust_before, rust_after),
            }

            expected = expected_scope_after()
            python_matches = python_after == expected
            rust_matches = rust_after == expected
            report["scope"]["python_matches_expected"] = python_matches
            report["scope"]["rust_matches_expected"] = rust_matches
            report["scope"]["equal"] = python_after == rust_after
            if not python_matches or not rust_matches or not report["scope"]["equal"]:
                raise RuntimeError(
                    "Python/Rust normalized A/B scope mismatch; see report scope"
                )

            report["copies"]["python"]["after"] = inspect_database(python_copy)
            report["copies"]["rust"]["after"] = inspect_database(rust_copy)
            report["process_order"] = {
                "python_write_completed_before_rust_write": True,
                "rust_write_started_after_python_subprocess_exit": True,
            }
            report["status"] = "passed"
    except (OSError, RuntimeError, sqlite3.Error, ValueError) as error:
        report["errors"].append(str(error))
    finally:
        try:
            source_after = inspect_database(source)
            report["source"]["after"] = source_after
            if source_before is not None:
                report["source"]["unchanged"] = _same_source(source_before, source_after)
                if not report["source"]["unchanged"]:
                    report["errors"].append(
                        "source database changed while A/B replay was running"
                    )
                    report["status"] = "failed"
        except (OSError, sqlite3.Error) as error:
            report["source"]["after_error"] = str(error)
            report["errors"].append(f"could not verify source after A/B replay: {error}")
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
            "Compare bounded Python/Rust repository writes on independent "
            "online-backup copies; the source is never opened writable."
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
            "readiness_gate_closed": False,
            "errors": [str(error)],
        }
        status = 1
    print(json.dumps(report, indent=2, sort_keys=True))
    return status


if __name__ == "__main__":
    raise SystemExit(main())
