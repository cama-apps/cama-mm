"""
Runtime health and usage monitoring for the Discord bot.
"""

from __future__ import annotations

import os
import platform
import resource
import sqlite3
import subprocess
import sys
import threading
import time
from collections import Counter
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


def _utc_now() -> datetime:
    return datetime.now(UTC)


def _short_git_sha() -> str:
    env_sha = os.getenv("GIT_SHA", "").strip()
    if env_sha:
        return env_sha[:12]

    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short=12", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
            timeout=2,
        )
    except Exception:
        return "unknown"
    return result.stdout.strip() or "unknown"


def _format_bytes(value: int | None) -> str:
    if value is None:
        return "unavailable"
    size = float(value)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if size < 1024 or unit == "GiB":
            return f"{size:.1f} {unit}" if unit != "B" else f"{int(size)} B"
        size /= 1024
    return f"{size:.1f} GiB"


def _format_duration(seconds: int) -> str:
    days, rem = divmod(max(0, seconds), 86400)
    hours, rem = divmod(rem, 3600)
    minutes, secs = divmod(rem, 60)
    parts = []
    if days:
        parts.append(f"{days}d")
    if hours or parts:
        parts.append(f"{hours}h")
    if minutes or parts:
        parts.append(f"{minutes}m")
    parts.append(f"{secs}s")
    return " ".join(parts)


def _rss_bytes() -> int:
    usage = resource.getrusage(resource.RUSAGE_SELF)
    rss = int(usage.ru_maxrss)
    if sys.platform == "darwin":
        return rss
    return rss * 1024


def _open_fd_count() -> int | None:
    for path in ("/proc/self/fd", "/dev/fd"):
        try:
            return len(os.listdir(path))
        except OSError:
            continue
    return None


@dataclass(frozen=True)
class UsageSnapshot:
    command_total: int
    command_failures: int
    commands_by_name: dict[str, int]
    api_requests: dict[str, int]


@dataclass(frozen=True)
class HealthSnapshot:
    status: str
    reasons: list[str]
    uptime: str
    started_at: str
    git_sha: str
    python: str
    db_ok: bool
    db_latency_ms: float | None
    db_size: str
    memory_rss: str
    open_fds: int | None
    io_blocks_in: int
    io_blocks_out: int
    discord_latency_ms: float | None
    guild_count: int
    usage: UsageSnapshot
    extra: dict[str, Any] = field(default_factory=dict)


class UsageMonitor:
    """Thread-safe in-memory counters for since-startup usage."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._command_total = 0
        self._command_failures = 0
        self._commands_by_name: Counter[str] = Counter()
        self._api_requests: Counter[str] = Counter()

    def record_command(self, name: str | None) -> None:
        command_name = name or "unknown"
        with self._lock:
            self._command_total += 1
            self._commands_by_name[command_name] += 1

    def record_command_failure(self) -> None:
        with self._lock:
            self._command_failures += 1

    def record_api_request(self, provider: str) -> None:
        with self._lock:
            self._api_requests[provider] += 1

    def snapshot(self) -> UsageSnapshot:
        with self._lock:
            return UsageSnapshot(
                command_total=self._command_total,
                command_failures=self._command_failures,
                commands_by_name=dict(self._commands_by_name.most_common(8)),
                api_requests=dict(self._api_requests),
            )


_GLOBAL_USAGE_MONITOR: UsageMonitor | None = None


def set_global_usage_monitor(monitor: UsageMonitor) -> None:
    global _GLOBAL_USAGE_MONITOR
    _GLOBAL_USAGE_MONITOR = monitor


def get_global_usage_monitor() -> UsageMonitor | None:
    return _GLOBAL_USAGE_MONITOR


class MonitoringService:
    """Builds health snapshots from runtime state and lightweight probes."""

    def __init__(
        self,
        db_path: str,
        *,
        usage_monitor: UsageMonitor | None = None,
        started_at: datetime | None = None,
        git_sha: str | None = None,
    ) -> None:
        self.db_path = db_path
        self.usage_monitor = usage_monitor or UsageMonitor()
        self.started_at = started_at or _utc_now()
        self.git_sha = git_sha or _short_git_sha()

    def snapshot(self, bot: Any | None = None) -> HealthSnapshot:
        now = _utc_now()
        uptime_seconds = int((now - self.started_at).total_seconds())
        reasons: list[str] = []

        db_ok, db_latency_ms, db_reason = self._probe_db()
        if not db_ok and db_reason:
            reasons.append(db_reason)

        latency = getattr(bot, "latency", None) if bot is not None else None
        discord_latency_ms = None
        if isinstance(latency, int | float) and latency >= 0:
            discord_latency_ms = latency * 1000

        usage = resource.getrusage(resource.RUSAGE_SELF)
        status = "ok" if not reasons else "degraded"

        return HealthSnapshot(
            status=status,
            reasons=reasons,
            uptime=_format_duration(uptime_seconds),
            started_at=self.started_at.isoformat(timespec="seconds"),
            git_sha=self.git_sha,
            python=platform.python_version(),
            db_ok=db_ok,
            db_latency_ms=db_latency_ms,
            db_size=_format_bytes(self._db_size_bytes()),
            memory_rss=_format_bytes(_rss_bytes()),
            open_fds=_open_fd_count(),
            io_blocks_in=int(usage.ru_inblock),
            io_blocks_out=int(usage.ru_oublock),
            discord_latency_ms=discord_latency_ms,
            guild_count=len(getattr(bot, "guilds", []) or []) if bot is not None else 0,
            usage=self.usage_monitor.snapshot(),
        )

    def _probe_db(self) -> tuple[bool, float | None, str | None]:
        started = time.perf_counter()
        try:
            with sqlite3.connect(self.db_path) as conn:
                conn.execute("SELECT 1").fetchone()
        except Exception as exc:
            return False, None, f"DB probe failed: {exc}"
        return True, (time.perf_counter() - started) * 1000, None

    def _db_size_bytes(self) -> int | None:
        if self.db_path.startswith("file:") or self.db_path == ":memory:":
            return None
        try:
            return Path(self.db_path).stat().st_size
        except OSError:
            return None


def format_health_snapshot(snapshot: HealthSnapshot) -> str:
    """Format a compact Discord-safe health response."""
    status_line = "OK" if snapshot.status == "ok" else "DEGRADED"
    db_latency = (
        f"{snapshot.db_latency_ms:.1f} ms" if snapshot.db_latency_ms is not None else "failed"
    )
    discord_latency = (
        f"{snapshot.discord_latency_ms:.0f} ms"
        if snapshot.discord_latency_ms is not None
        else "unavailable"
    )
    fds = str(snapshot.open_fds) if snapshot.open_fds is not None else "unavailable"
    api_requests = snapshot.usage.api_requests or {}
    api_line = ", ".join(
        f"{name}: {count}" for name, count in sorted(api_requests.items())
    ) or "none"
    command_line = ", ".join(
        f"{name}: {count}" for name, count in snapshot.usage.commands_by_name.items()
    ) or "none"
    reasons = "\n".join(f"- {reason}" for reason in snapshot.reasons) or "- none"

    return (
        f"**Status:** {status_line}\n"
        f"**Uptime:** {snapshot.uptime}\n"
        f"**Started:** {snapshot.started_at}\n"
        f"**Git SHA:** `{snapshot.git_sha}`\n"
        f"**Python:** {snapshot.python}\n"
        f"**Discord latency:** {discord_latency}\n"
        f"**Guilds:** {snapshot.guild_count}\n"
        f"**DB:** {'ok' if snapshot.db_ok else 'failed'} ({db_latency}), size {snapshot.db_size}\n"
        f"**Memory RSS:** {snapshot.memory_rss}\n"
        f"**Open FDs:** {fds}\n"
        f"**IO blocks:** in {snapshot.io_blocks_in}, out {snapshot.io_blocks_out}\n"
        f"**Commands:** {snapshot.usage.command_total} total, "
        f"{snapshot.usage.command_failures} failed\n"
        f"**Top commands:** {command_line}\n"
        f"**API requests:** {api_line}\n"
        f"**Degraded reasons:**\n{reasons}"
    )
