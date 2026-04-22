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
    """Record, read, expire, and overwrite echo rows (keyed by boss_id)."""

    def test_no_row_returns_none(self, dig_repo):
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak") is None

    def test_record_then_read(self, dig_repo):
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "grothak", 25,
            killer_discord_id=777, window_seconds=3600,
        )
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak")
        assert row is not None
        assert row["killer_discord_id"] == 777
        assert row["depth"] == 25
        assert row["weakened_until"] > int(time.time())

    def test_expired_row_returns_none(self, dig_repo, monkeypatch):
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "crystalia", 50,
            killer_discord_id=111, window_seconds=60,
        )
        real_time = time.time()
        monkeypatch.setattr(time, "time", lambda: real_time + 3600)
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "crystalia") is None

    def test_overwrite_restarts_window(self, dig_repo):
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "magmus_rex", 75,
            killer_discord_id=111, window_seconds=60,
        )
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "magmus_rex", 75,
            killer_discord_id=222, window_seconds=3600,
        )
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, "magmus_rex")
        assert row["killer_discord_id"] == 222

    def test_boss_id_isolation(self, dig_repo):
        """Killing Pudge at tier 25 does NOT weaken Grothak at tier 25."""
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "pudge", 25,
            killer_discord_id=1, window_seconds=3600,
        )
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "crystalia", 50,
            killer_discord_id=2, window_seconds=3600,
        )
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "pudge")["killer_discord_id"] == 1
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "crystalia")["killer_discord_id"] == 2
        # Same tier, different boss — no echo crosstalk.
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak") is None
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "ogre_magi") is None

    def test_guild_isolation(self, dig_repo):
        dig_repo.record_boss_echo(
            1000, "grothak", 25,
            killer_discord_id=1, window_seconds=3600,
        )
        dig_repo.record_boss_echo(
            2000, "grothak", 25,
            killer_discord_id=2, window_seconds=3600,
        )
        assert dig_repo.get_active_boss_echo(1000, "grothak")["killer_discord_id"] == 1
        assert dig_repo.get_active_boss_echo(2000, "grothak")["killer_discord_id"] == 2
        # None guild normalizes to 0; not a collision with either above.
        assert dig_repo.get_active_boss_echo(None, "grothak") is None
