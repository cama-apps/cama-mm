"""Fast-fail Python inventory checks used before the expensive Rust CI lanes."""

from __future__ import annotations

import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = ROOT / "rust" / "parity" / "baseline.txt"
MAPPINGS = ROOT / "rust" / "parity" / "tests.tsv"


def fnv1a64_lines(lines: list[str]) -> int:
    value = 0xCBF29CE484222325
    for line in lines:
        for byte in f"{line}\n".encode():
            value ^= byte
            value = value * 0x100000001B3 & 0xFFFFFFFFFFFFFFFF
    return value


def collect_python_tests() -> set[str]:
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "-n", "0", "--collect-only", "-q"],
        cwd=ROOT,
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"pytest collection failed with exit code {result.returncode}:\n{result.stderr}"
        )
    tests = {
        line
        for line in result.stdout.splitlines()
        if line.startswith("tests/") and "::" in line
    }
    if not tests:
        raise SystemExit("pytest collection returned no test node IDs")
    return tests


def read_baseline() -> tuple[int, int]:
    values = {}
    for raw_line in BASELINE.read_text().splitlines():
        line = raw_line.strip()
        if line and not line.startswith("#"):
            key, value = line.split("=", 1)
            values[key] = value
    return int(values["collected_cases"]), int(values["node_id_fnv1a64"], 16)


def read_mapping_ids() -> tuple[list[str], list[str]]:
    python_ids = []
    rust_ids = []
    for raw_line in MAPPINGS.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        try:
            python_id, rust_id = line.split("\t", 1)
        except ValueError as error:
            raise SystemExit(f"invalid parity mapping: {raw_line!r}") from error
        python_ids.append(python_id)
        rust_ids.append(rust_id)
    return python_ids, rust_ids


def require_unique(values: list[str], label: str) -> None:
    duplicates = sorted(value for value, count in Counter(values).items() if count > 1)
    if duplicates:
        raise SystemExit(f"duplicate {label}: {', '.join(duplicates)}")


def main() -> None:
    python_tests = collect_python_tests()
    expected_count, expected_fingerprint = read_baseline()
    actual = sorted(python_tests)
    actual_fingerprint = fnv1a64_lines(actual)
    if (len(actual), actual_fingerprint) != (expected_count, expected_fingerprint):
        raise SystemExit(
            "Python test inventory changed: "
            f"expected {expected_count} cases/{expected_fingerprint:016x}, "
            f"found {len(actual)} cases/{actual_fingerprint:016x}; "
            "update rust/parity/baseline.txt intentionally"
        )

    mapped_python, mapped_rust = read_mapping_ids()
    require_unique(mapped_python, "Python parity test IDs")
    require_unique(mapped_rust, "Rust parity test IDs")
    unknown = sorted(set(mapped_python) - python_tests)
    unmapped = sorted(python_tests - set(mapped_python))
    if unknown:
        raise SystemExit(f"parity mappings reference unknown Python tests: {', '.join(unknown)}")
    if unmapped:
        raise SystemExit(f"Python tests are missing parity mappings: {', '.join(unmapped)}")

    print(
        f"Python parity inventory: {len(actual)}/{len(actual)} cases mapped "
        f"({actual_fingerprint:016x})"
    )


if __name__ == "__main__":
    main()
