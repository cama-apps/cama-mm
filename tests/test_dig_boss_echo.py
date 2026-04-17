"""Tests for boss echo weakening (post-kill 24h softening window)."""

from __future__ import annotations

import time

import pytest

from repositories.dig_repository import DigRepository
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


class TestBossEchoRepository:
    """Record, read, expire, and overwrite echo rows."""

    def test_no_row_returns_none(self, dig_repo):
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, 25) is None

    def test_record_then_read(self, dig_repo):
        dig_repo.record_boss_echo(TEST_GUILD_ID, 25, killer_discord_id=777, window_seconds=3600)
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, 25)
        assert row is not None
        assert row["killer_discord_id"] == 777
        assert row["weakened_until"] > int(time.time())

    def test_expired_row_returns_none(self, dig_repo, monkeypatch):
        dig_repo.record_boss_echo(TEST_GUILD_ID, 50, killer_discord_id=111, window_seconds=60)
        # Jump clock 1 hour forward.
        real_time = time.time()
        monkeypatch.setattr(time, "time", lambda: real_time + 3600)
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, 50) is None

    def test_overwrite_restarts_window(self, dig_repo):
        dig_repo.record_boss_echo(TEST_GUILD_ID, 75, killer_discord_id=111, window_seconds=60)
        dig_repo.record_boss_echo(TEST_GUILD_ID, 75, killer_discord_id=222, window_seconds=3600)
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, 75)
        assert row["killer_discord_id"] == 222

    def test_depth_isolation(self, dig_repo):
        dig_repo.record_boss_echo(TEST_GUILD_ID, 25, killer_discord_id=1, window_seconds=3600)
        dig_repo.record_boss_echo(TEST_GUILD_ID, 50, killer_discord_id=2, window_seconds=3600)
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, 25)["killer_discord_id"] == 1
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, 50)["killer_discord_id"] == 2

    def test_guild_isolation(self, dig_repo):
        dig_repo.record_boss_echo(1000, 25, killer_discord_id=1, window_seconds=3600)
        dig_repo.record_boss_echo(2000, 25, killer_discord_id=2, window_seconds=3600)
        assert dig_repo.get_active_boss_echo(1000, 25)["killer_discord_id"] == 1
        assert dig_repo.get_active_boss_echo(2000, 25)["killer_discord_id"] == 2
        # None guild normalizes to 0; not a collision with either above.
        assert dig_repo.get_active_boss_echo(None, 25) is None
