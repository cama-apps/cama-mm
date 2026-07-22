"""Regression tests for keeping Dotabase ORM setup off the startup path."""

import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]


def _run_fresh_process(script: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(script)],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )


@pytest.mark.parametrize("module_name", ["services.trivia_data", "commands.dota_info"])
def test_import_does_not_initialize_dotabase_orm(module_name: str):
    completed = _run_fresh_process(
        f"""
        import importlib
        import sys

        importlib.import_module({module_name!r})

        unexpected = [
            name
            for name in sys.modules
            if name == "dotabase_integration"
            or name.startswith("sqlalchemy")
        ]
        assert not unexpected, unexpected
        """
    )

    assert completed.returncode == 0, completed.stderr


@pytest.mark.parametrize(
    ("module_name", "loader_name"),
    [
        ("services.trivia_data", "load_heroes"),
        ("commands.dota_info", "_get_all_heroes"),
    ],
)
def test_first_data_lookup_initializes_dotabase_orm(module_name: str, loader_name: str):
    completed = _run_fresh_process(
        f"""
        import importlib
        import sys

        module = importlib.import_module({module_name!r})
        rows = getattr(module, {loader_name!r})()

        assert len(rows) > 100
        assert "dotabase_integration" in sys.modules
        assert "sqlalchemy" in sys.modules
        """
    )

    assert completed.returncode == 0, completed.stderr
