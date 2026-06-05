from __future__ import annotations

from datetime import UTC, datetime, timedelta
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from services.monitoring_service import (
    MonitoringService,
    UsageMonitor,
    format_health_snapshot,
    set_global_usage_monitor,
)


def test_usage_monitor_tracks_commands_and_api_requests():
    monitor = UsageMonitor()

    monitor.record_command("admin health")
    monitor.record_command("admin health")
    monitor.record_command("lobby")
    monitor.record_command_failure()
    monitor.record_api_request("opendota")
    monitor.record_api_request("opendota")
    monitor.record_api_request("valve")

    snapshot = monitor.snapshot()

    assert snapshot.command_total == 3
    assert snapshot.command_failures == 1
    assert snapshot.commands_by_name["admin health"] == 2
    assert snapshot.api_requests == {"opendota": 2, "valve": 1}


def test_monitoring_snapshot_reports_ok_db_and_runtime_fields(repo_db_path):
    started_at = datetime.now(UTC) - timedelta(seconds=3661)
    monitor = UsageMonitor()
    monitor.record_command("admin health")
    service = MonitoringService(
        repo_db_path,
        usage_monitor=monitor,
        started_at=started_at,
        git_sha="abc123",
    )
    bot = MagicMock()
    bot.latency = 0.123
    bot.guilds = [object(), object()]

    snapshot = service.snapshot(bot)

    assert snapshot.status == "ok"
    assert snapshot.db_ok is True
    assert snapshot.db_latency_ms is not None
    assert snapshot.git_sha == "abc123"
    assert snapshot.python
    assert snapshot.guild_count == 2
    assert snapshot.discord_latency_ms == 123
    assert snapshot.usage.command_total == 1
    assert "1h" in snapshot.uptime


def test_monitoring_service_prefers_git_sha_env(monkeypatch):
    monkeypatch.setenv("GIT_SHA", "abcdef1234567890")

    with patch("services.monitoring_service.subprocess.run") as run:
        service = MonitoringService(":memory:")

    assert service.git_sha == "abcdef123456"
    run.assert_not_called()


def test_monitoring_service_uses_git_command_when_env_missing(monkeypatch):
    monkeypatch.delenv("GIT_SHA", raising=False)
    completed = SimpleNamespace(stdout="123456789abc\n")

    with patch("services.monitoring_service.subprocess.run", return_value=completed) as run:
        service = MonitoringService(":memory:")

    assert service.git_sha == "123456789abc"
    run.assert_called_once()


def test_monitoring_snapshot_degrades_on_db_failure(tmp_path):
    db_dir = tmp_path / "missing-dir"
    service = MonitoringService(str(db_dir / "missing.db"), git_sha="abc123")

    snapshot = service.snapshot()

    assert snapshot.status == "degraded"
    assert snapshot.db_ok is False
    assert snapshot.reasons


def test_format_health_snapshot_includes_requested_fields(repo_db_path):
    monitor = UsageMonitor()
    monitor.record_command("admin health")
    monitor.record_api_request("opendota")
    service = MonitoringService(repo_db_path, usage_monitor=monitor, git_sha="abc123")

    text = format_health_snapshot(service.snapshot())

    assert "**Status:** OK" in text
    assert "**Uptime:**" in text
    assert "**Started:**" in text
    assert "**Git SHA:** `abc123`" in text
    assert "**Python:**" in text
    assert "**DB:** ok" in text
    assert "**Commands:** 1 total" in text
    assert "**API requests:** opendota: 1" in text


def test_opendota_counter_increments_on_actual_request_attempt(monkeypatch):
    from opendota_integration import OpenDotaAPI

    monitor = UsageMonitor()
    set_global_usage_monitor(monitor)
    api = OpenDotaAPI(api_key=None)
    monkeypatch.setattr(OpenDotaAPI._rate_limiter, "acquire", lambda timeout: True)
    response = MagicMock()
    response.status_code = 200
    with patch.object(api.session, "get", return_value=response):
        api.make_request("https://api.opendota.com/api/test")

    assert monitor.snapshot().api_requests["opendota"] == 1
