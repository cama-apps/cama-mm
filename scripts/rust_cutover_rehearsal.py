#!/usr/bin/env python3
"""Rehearse a one-way Rust cutover on an explicit disposable SQLite copy.

The caller must provide both the production-shaped source and a separately
created disposable copy.  This runner never creates a copy, never opens the
source writable, and refuses to operate when the two paths resolve to the
same file.  Rust performs schema admission/migration and all post-admission
repository probes.  Python is used only as the orchestration interpreter;
there is deliberately no Python migration or Python readback step.

The JSON report is fail-closed evidence: it records source immutability,
Rust admission details, every Rust-only smoke command, and the resulting
disposable-copy deltas.  A caller can retain the copy for investigation, but
this command never treats it as the live database.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from pathlib import Path
from typing import Any

# When launched as `python scripts/rust_cutover_rehearsal.py`, Python places
# `scripts/` (rather than the repository root) on sys.path.  Add the root
# before importing the existing replay helpers as a module.
REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
if str(REPOSITORY_ROOT) not in sys.path:
    sys.path.insert(0, str(REPOSITORY_ROOT))

from scripts.production_snapshot_replay import (
    StepFailure,
    _parse_key_values,
    _source_unchanged,
    _write_report,
    inspect_database,
    run_step,
)

REPORT_FORMAT_VERSION = 1
DEFAULT_TIMEOUT_SECONDS = 900
CONFIRMATION = "--disposable-copy"
DIG_PET_GUILD_ID = -8_888_888_888_888_741
DIG_PET_PLAYER_ID = -8_888_888_888_888_739


def _same_file(left: Path, right: Path) -> bool:
    """Return whether two existing paths identify the same filesystem file."""

    try:
        return os.path.samefile(left, right)
    except FileNotFoundError:
        return left == right


def _validate_paths(source: Path, disposable_copy: Path, report: Path | None) -> None:
    """Reject ambiguous/live targets before opening anything writable."""

    if not source.is_file():
        raise FileNotFoundError(f"source database does not exist: {source}")
    if not disposable_copy.is_file():
        raise FileNotFoundError(
            f"disposable copy does not exist; create it with sqlite backup first: {disposable_copy}"
        )
    if _same_file(source, disposable_copy):
        raise ValueError("source and --disposable-copy must be different files")
    if disposable_copy in {
        Path(f"{source}-wal"),
        Path(f"{source}-shm"),
        Path(f"{source}-journal"),
    }:
        raise ValueError("--disposable-copy must not target a source SQLite sidecar")
    if report is not None and report in {
        source,
        disposable_copy,
        Path(f"{source}-wal"),
        Path(f"{source}-shm"),
        Path(f"{source}-journal"),
        Path(f"{disposable_copy}-wal"),
        Path(f"{disposable_copy}-shm"),
        Path(f"{disposable_copy}-journal"),
    }:
        raise ValueError("report path must not be either database or a SQLite sidecar")


def _run_rust_smoke(
    *,
    cargo: str,
    manifest: Path,
    package: str,
    example: str,
    database: Path,
    root: Path,
    timeout_seconds: float,
    report: dict[str, Any],
    step_name: str,
    extra_arguments: list[str] | None = None,
) -> dict[str, Any]:
    return run_step(
        step_name,
        [
            cargo,
            "run",
            "--locked",
            "--manifest-path",
            str(manifest),
            "-p",
            package,
            "--example",
            example,
            "--",
            str(database),
            *(extra_arguments or []),
            CONFIRMATION,
        ],
        cwd=root,
        timeout_seconds=timeout_seconds,
        report=report,
    )


def run(arguments: argparse.Namespace) -> tuple[int, dict[str, Any]]:
    root = REPOSITORY_ROOT
    source = arguments.source.expanduser().resolve(strict=False)
    disposable_copy = arguments.disposable_copy.expanduser().resolve(strict=False)
    report_path = (
        arguments.report.expanduser().resolve(strict=False)
        if arguments.report is not None
        else None
    )
    _validate_paths(source, disposable_copy, report_path)

    report: dict[str, Any] = {
        "format_version": REPORT_FORMAT_VERSION,
        "mode": "rust_only_one_way",
        "status": "failed",
        "python_post_write_migration": False,
        "python_post_rust_readback": False,
        "source": {"path": str(source), "before": None, "after": None, "unchanged": None},
        "disposable_copy": {
            "path": str(disposable_copy),
            "before": None,
            "rust_final_check": None,
        },
        "steps": [],
        "admission": {"status": "not_run"},
        "repository_smoke": {"status": "not_run"},
        "profile_info_reads": {"status": "not_run"},
        "dig_pet_reads": {"status": "not_run"},
        "referral_settlement": {"status": "not_run"},
        "survey_recovery": {"status": "not_run"},
        "errors": [],
    }
    source_before: dict[str, Any] | None = None

    try:
        source_before = inspect_database(source)
        report["source"]["before"] = source_before
        copy_before = inspect_database(disposable_copy)
        report["disposable_copy"]["before"] = copy_before
        runtime_manifest = root / "rust" / "Cargo.toml"
        if not runtime_manifest.is_file():
            raise FileNotFoundError(f"Rust manifest does not exist: {runtime_manifest}")

        # This is the production Rust binary's guarded schema-admission path.
        # Unlike `db-check`, it migrates only the explicitly distinct copy.
        admission = run_step(
            "rust_database_admission",
            [
                arguments.cargo,
                "run",
                "--locked",
                "--manifest-path",
                str(runtime_manifest),
                "-p",
                "cama-runtime",
                "--bin",
                "cama-rust",
                "--",
                "db-admit",
                "--db-path",
                str(disposable_copy),
                "--source-db",
                str(source),
                CONFIRMATION,
            ],
            cwd=root,
            timeout_seconds=arguments.timeout_seconds,
            report=report,
        )
        admission_values = _parse_key_values(admission["stdout"])
        if admission_values.get("schema_compatible") != "true":
            raise RuntimeError(
                f"Rust admission did not report schema_compatible=true: {admission['stdout']}"
            )
        report["admission"] = {"status": "passed", **admission_values}

        smoke = _run_rust_smoke(
            cargo=arguments.cargo,
            manifest=runtime_manifest,
            package="cama-db",
            example="guild_config_snapshot_smoke",
            database=disposable_copy,
            root=root,
            timeout_seconds=arguments.timeout_seconds,
            report=report,
            step_name="rust_repository_smoke",
        )
        if "shared_repository_roundtrip=ok" not in smoke["stdout"]:
            raise RuntimeError("Rust repository smoke omitted shared_repository_roundtrip=ok")
        report["repository_smoke"].update(
            {
                "status": "passed",
                "rust_self_check": True,
            }
        )

        dig_pet = _run_rust_smoke(
            cargo=arguments.cargo,
            manifest=runtime_manifest,
            package="cama-runtime",
            example="dig_pet_snapshot_smoke",
            database=disposable_copy,
            root=root,
            timeout_seconds=arguments.timeout_seconds,
            report=report,
            step_name="rust_dig_pet_read_smoke",
        )
        dig_pet_values = _parse_key_values(dig_pet["stdout"])
        dig_pet_marker = dig_pet_values.get("dig_pet_snapshot_smoke") == "ok"
        dig_pet_contract = (
            dig_pet_values.get("adjusted_jc") == "23"
            and dig_pet_values.get("worker_entries") == "2"
            and dig_pet_values.get("provider_entries") == "2"
            and dig_pet_values.get("guild_isolation") == "true"
            and dig_pet_values.get("read_only") == "true"
        )
        report["dig_pet_reads"].update(
            {
                "status": "passed" if dig_pet_marker and dig_pet_contract else "failed",
                "rust_self_check": dig_pet_marker,
                "contract": dig_pet_values,
            }
        )
        if not dig_pet_marker or not dig_pet_contract:
            raise RuntimeError(
                f"Rust Dig/pet smoke did not expose its exact read contract: {dig_pet['stdout']}"
            )

        profile = _run_rust_smoke(
            cargo=arguments.cargo,
            manifest=runtime_manifest,
            package="cama-runtime",
            example="profile_snapshot_smoke",
            database=disposable_copy,
            root=root,
            timeout_seconds=arguments.timeout_seconds,
            report=report,
            step_name="rust_profile_info_read_smoke",
            extra_arguments=[str(DIG_PET_PLAYER_ID), str(DIG_PET_GUILD_ID)],
        )
        profile_values = _parse_key_values(profile["stdout"])
        profile_marker = profile_values.get("profile_snapshot_smoke") == "ok"
        profile_contract = int(profile_values.get("profile_events", "0")) > 0
        report["profile_info_reads"].update(
            {
                "status": "passed" if profile_marker and profile_contract else "failed",
                "rust_self_check": profile_marker,
                "contract": profile_values,
            }
        )
        if not profile_marker or not profile_contract:
            raise RuntimeError(
                "Rust profile/Info smoke did not read the seeded production history: "
                f"{profile['stdout']}"
            )

        referral = _run_rust_smoke(
            cargo=arguments.cargo,
            manifest=runtime_manifest,
            package="cama-db",
            example="referral_snapshot_smoke",
            database=disposable_copy,
            root=root,
            timeout_seconds=arguments.timeout_seconds,
            report=report,
            step_name="rust_referral_settlement_smoke",
        )
        marker = "referral_snapshot_smoke=ok" in referral["stdout"]
        report["referral_settlement"].update(
            {
                "status": "passed" if marker else "failed",
                "rust_self_check": marker,
            }
        )
        if not marker:
            raise RuntimeError("Rust referral smoke omitted referral_snapshot_smoke=ok")

        survey = _run_rust_smoke(
            cargo=arguments.cargo,
            manifest=runtime_manifest,
            package="cama-runtime",
            example="survey_snapshot_smoke",
            database=disposable_copy,
            root=root,
            timeout_seconds=arguments.timeout_seconds,
            report=report,
            step_name="rust_survey_recovery_smoke",
        )
        marker = "survey_snapshot_smoke=ok" in survey["stdout"]
        report["survey_recovery"].update(
            {
                "status": "passed" if marker else "failed",
                "rust_self_check": marker,
            }
        )
        if not marker:
            raise RuntimeError("Rust Survey smoke omitted survey_snapshot_smoke=ok")

        final_check = run_step(
            "rust_final_database_check",
            [
                arguments.cargo,
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
                str(disposable_copy),
            ],
            cwd=root,
            timeout_seconds=arguments.timeout_seconds,
            report=report,
        )
        final_values = _parse_key_values(final_check["stdout"])
        if (
            final_values.get("schema_compatible") != "true"
            or final_values.get("quick_check") != "ok"
        ):
            raise RuntimeError(
                f"Rust final database check was not healthy: {final_check['stdout']}"
            )
        report["disposable_copy"]["rust_final_check"] = {
            "status": "passed",
            **final_values,
        }
        report["status"] = "passed"
    except (OSError, RuntimeError, sqlite3.Error, StepFailure, ValueError) as error:
        report["errors"].append(str(error))
    finally:
        try:
            source_after = inspect_database(source)
            report["source"]["after"] = source_after
            if source_before is not None:
                report["source"]["unchanged"] = _source_unchanged(source_before, source_after)
                if not report["source"]["unchanged"]:
                    report["errors"].append("source database changed while rehearsal was running")
                    report["status"] = "failed"
        except (OSError, sqlite3.Error) as error:
            report["source"]["after_error"] = str(error)
            report["source"]["unchanged"] = False
            report["errors"].append(f"could not verify source after rehearsal: {error}")
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
            "Run Rust admission and Rust-only repository/recovery smokes on "
            "an explicitly disposable SQLite copy."
        )
    )
    parser.add_argument("source", type=Path, help="production-shaped source database (read-only)")
    parser.add_argument(
        "--disposable-copy",
        type=Path,
        required=True,
        help="pre-created writable SQLite copy; this command never creates it",
    )
    parser.add_argument("--cargo", default="cargo", help="Cargo executable")
    parser.add_argument("--report", type=Path, help="write JSON evidence to a new path")
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
            "mode": "rust_only_one_way",
            "status": "failed",
            "errors": [str(error)],
        }
        status = 1
    print(json.dumps(report, indent=2, sort_keys=True))
    return status


if __name__ == "__main__":
    raise SystemExit(main())
