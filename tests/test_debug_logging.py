"""
Tests for debug_logging.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from utils.debug_logging import debug_log


def test_debug_log_no_env_does_nothing(monkeypatch):
    monkeypatch.delenv("DEBUG_LOG_PATH", raising=False)

    # Should not raise even without a configured path.
    debug_log("H0", "loc", "msg", {"a": 1})


def test_debug_log_writes_jsonl(monkeypatch, tmp_path: Path):
    path = tmp_path / "debug.jsonl"
    monkeypatch.setenv("DEBUG_LOG_PATH", str(path))

    debug_log(
        "H1",
        "module.py:func",
        "hello",
        {"k": "v"},
        run_id="run-123",
        session_id="session-abc",
    )

    content = path.read_text(encoding="utf-8").strip()
    payload: dict[str, Any] = json.loads(content)

    assert payload["hypothesisId"] == "H1"
    assert payload["location"] == "module.py:func"
    assert payload["message"] == "hello"
    assert payload["data"] == {"k": "v"}
    assert payload["runId"] == "run-123"
    assert payload["sessionId"] == "session-abc"


def test_debug_log_swallows_exceptions(monkeypatch, tmp_path: Path):
    path = tmp_path / "debug.jsonl"
    monkeypatch.setenv("DEBUG_LOG_PATH", str(path))

    def _raise(*_args, **_kwargs):
        raise OSError("nope")

    monkeypatch.setattr("builtins.open", _raise)

    # Should not raise even if file write fails.
    debug_log("H2", "loc", "msg")
