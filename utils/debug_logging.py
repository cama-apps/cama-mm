"""
Lightweight structured debug logging.

Enabled when DEBUG_LOG_PATH env var is set. Intended for temporary/diagnostic tracing
without polluting normal logs.
"""

from __future__ import annotations

import json
import os
import time
from typing import Any


def debug_log(
    hypothesis_id: str,
    location: str,
    message: str,
    data: dict[str, Any] | None = None,
    *,
    run_id: str = "run1",
    session_id: str = "debug-session",
) -> None:
    """
    Append a JSONL debug entry to DEBUG_LOG_PATH if configured.
    """
    path = os.getenv("DEBUG_LOG_PATH")
    if not path:
        return

    payload = {
        "sessionId": session_id,
        "runId": run_id,
        "hypothesisId": hypothesis_id,
        "timestamp": int(time.time() * 1000),
        "location": location,
        "message": message,
        "data": data or {},
    }

    try:
        with open(path, "a", encoding="utf-8") as f:
            f.write(json.dumps(payload) + "\n")
    except Exception:
        # Debug logging must never impact bot functionality
        return
