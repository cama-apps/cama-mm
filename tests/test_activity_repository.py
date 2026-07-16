"""Tests for channel-activity tracking on PlayerRepository.

Covers the last_active_at bump path, the combined lottery-eligibility read, and
the lottery-selection query now counting channel presence alongside match play.
"""

import sqlite3
from datetime import UTC, datetime, timedelta

import pytest

from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

ACTIVITY_DAYS = 14


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


def _register(repo: PlayerRepository, discord_id: int, guild_id: int = TEST_GUILD_ID) -> None:
    repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
    )


def _read_last_active(repo: PlayerRepository, discord_id: int, guild_id: int = TEST_GUILD_ID):
    with sqlite3.connect(repo.db_path) as conn:
        row = conn.execute(
            "SELECT last_active_at FROM players WHERE discord_id = ? AND guild_id = ?",
            (discord_id, guild_id),
        ).fetchone()
    return row[0] if row else None


def _iso(days_ago: int) -> str:
    return (datetime.now(UTC) - timedelta(days=days_ago)).isoformat()


class TestBumpLastActiveMany:
    def test_updates_registered_only(self, player_repo):
        """Registered IDs get a timestamp; unknown IDs no-op without error."""
        _register(player_repo, 1)
        # 999 is not registered — it must be silently ignored.
        player_repo.bump_last_active_many([1, 999], TEST_GUILD_ID)

        assert _read_last_active(player_repo, 1) is not None
        assert _read_last_active(player_repo, 999) is None

    def test_is_monotonic(self, player_repo):
        """Bumping with an older timestamp never moves last_active_at backward."""
        _register(player_repo, 1)
        recent = _iso(0)
        older = _iso(5)

        player_repo.bump_last_active_many([1], TEST_GUILD_ID, timestamp=recent)
        player_repo.bump_last_active_many([1], TEST_GUILD_ID, timestamp=older)

        assert _read_last_active(player_repo, 1) == recent

    def test_empty_list_noop(self, player_repo):
        assert player_repo.bump_last_active_many([], TEST_GUILD_ID) == 0

    def test_guild_isolation(self, player_repo):
        """A bump in one guild does not activate the same user in another."""
        _register(player_repo, 1, TEST_GUILD_ID)
        _register(player_repo, 1, TEST_GUILD_ID_SECONDARY)

        player_repo.bump_last_active_many([1], TEST_GUILD_ID)

        assert player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)
        assert not player_repo.is_active_for_lottery(1, TEST_GUILD_ID_SECONDARY, ACTIVITY_DAYS)


class TestIsActiveForLottery:
    def test_counts_channel_activity(self, player_repo):
        """Recent channel presence alone makes a player active (no matches)."""
        _register(player_repo, 1)
        player_repo.bump_last_active_many([1], TEST_GUILD_ID)

        assert player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)

    def test_counts_match_activity(self, player_repo):
        """Recent match play alone makes a player active (last_active_at NULL)."""
        _register(player_repo, 1)
        player_repo.update_last_match_date(1, TEST_GUILD_ID, timestamp=_iso(1))

        assert _read_last_active(player_repo, 1) is None
        assert player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)

    def test_inactive_when_both_stale(self, player_repo):
        """Both signals older than the window -> inactive."""
        _register(player_repo, 1)
        player_repo.update_last_match_date(1, TEST_GUILD_ID, timestamp=_iso(30))
        player_repo.bump_last_active_many([1], TEST_GUILD_ID, timestamp=_iso(30))

        assert not player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)

    def test_inactive_when_no_signals(self, player_repo):
        """A freshly-registered player with no match/activity is inactive."""
        _register(player_repo, 1)
        assert not player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)


class TestLotteryGateCountsActivity:
    def test_channel_activity_appears_in_lottery(self, player_repo):
        """A player with no matches but recent presence is lottery-eligible."""
        _register(player_repo, 1)
        player_repo.bump_last_active_many([1], TEST_GUILD_ID)

        eligible = {row["discord_id"] for row in
                    player_repo.get_all_registered_players_for_lottery(TEST_GUILD_ID, ACTIVITY_DAYS)}
        assert 1 in eligible

    def test_match_activity_still_appears(self, player_repo):
        """Regression: match recency still qualifies with last_active_at NULL."""
        _register(player_repo, 2)
        player_repo.update_last_match_date(2, TEST_GUILD_ID, timestamp=_iso(1))

        eligible = {row["discord_id"] for row in
                    player_repo.get_all_registered_players_for_lottery(TEST_GUILD_ID, ACTIVITY_DAYS)}
        assert 2 in eligible

    def test_stale_player_excluded(self, player_repo):
        """A player with only stale signals is not lottery-eligible."""
        _register(player_repo, 3)
        player_repo.bump_last_active_many([3], TEST_GUILD_ID, timestamp=_iso(30))

        eligible = {row["discord_id"] for row in
                    player_repo.get_all_registered_players_for_lottery(TEST_GUILD_ID, ACTIVITY_DAYS)}
        assert 3 not in eligible
