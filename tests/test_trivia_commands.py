"""Tests for trivia cooldown and economy integration."""

import time

import pytest

from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def player_service(repo_db_path):
    repo = PlayerRepository(repo_db_path)
    return PlayerService(repo)


@pytest.fixture
def registered_player(player_service):
    """Register a test player and return their discord_id."""
    discord_id = 100001
    player_service.register_player(
        discord_id=discord_id,
        discord_username="trivia_tester",
        guild_id=TEST_GUILD_ID,
        steam_id=12345,
        mmr_override=3000,
    )
    return discord_id


class TestTriviaCooldown:
    def test_first_session_succeeds(self, player_service, registered_player):
        now = int(time.time())
        claimed = player_service.try_claim_trivia_session(
            registered_player, TEST_GUILD_ID, now, 21600
        )
        assert claimed is True

    def test_second_session_blocked_by_cooldown(self, player_service, registered_player):
        now = int(time.time())
        # First claim succeeds
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        # Second claim within cooldown fails
        assert not player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now + 100, 21600)

    def test_session_available_after_cooldown(self, player_service, registered_player):
        now = int(time.time())
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        # After cooldown expires
        future = now + 21601
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, future, 21600)

    def test_get_last_trivia_session_none_initially(self, player_service, registered_player):
        result = player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID)
        assert result is None

    def test_get_last_trivia_session_after_claim(self, player_service, registered_player):
        now = int(time.time())
        player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        result = player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID)
        assert result == now


class TestTriviaEconomy:
    def test_balance_increases_on_correct_answer(self, player_service, registered_player):
        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        player_service.adjust_balance(registered_player, TEST_GUILD_ID, 1)
        after = player_service.get_balance(registered_player, TEST_GUILD_ID)
        assert after == initial + 1

    def test_multiple_correct_answers(self, player_service, registered_player):
        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        for _ in range(5):
            player_service.adjust_balance(registered_player, TEST_GUILD_ID, 1)
        after = player_service.get_balance(registered_player, TEST_GUILD_ID)
        assert after == initial + 5


class TestTriviaCooldownGuildIsolation:
    def test_cooldown_per_guild(self, player_service, registered_player):
        """Trivia cooldown in one guild shouldn't affect another."""
        guild_a = TEST_GUILD_ID
        guild_b = 99999

        # Register in second guild
        player_service.register_player(
            discord_id=registered_player,
            discord_username="trivia_tester",
            guild_id=guild_b,
            steam_id=12345,
            mmr_override=3000,
        )

        now = int(time.time())
        # Claim in guild A
        assert player_service.try_claim_trivia_session(registered_player, guild_a, now, 21600)
        # Should still be available in guild B
        assert player_service.try_claim_trivia_session(registered_player, guild_b, now, 21600)


class TestTriviaResetCooldown:
    def test_reset_clears_cooldown(self, player_service, registered_player):
        now = int(time.time())
        # Claim session (sets cooldown)
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        # Blocked within cooldown
        assert not player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now + 100, 21600)
        # Reset cooldown
        assert player_service.reset_trivia_cooldown(registered_player, TEST_GUILD_ID)
        # Should be able to claim again
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now + 100, 21600)

    def test_reset_returns_false_for_missing_player(self, player_service):
        assert not player_service.reset_trivia_cooldown(999999, TEST_GUILD_ID)

    def test_get_last_trivia_session_none_after_reset(self, player_service, registered_player):
        now = int(time.time())
        player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        assert player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID) == now
        player_service.reset_trivia_cooldown(registered_player, TEST_GUILD_ID)
        assert player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID) is None


class TestTriviaSessionRecording:
    def test_record_and_leaderboard(self, player_service, registered_player):
        now = int(time.time())
        # Record a few sessions
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=5, jc_earned=5)

        # Register a second player and record
        discord_id_2 = 100002
        player_service.register_player(
            discord_id=discord_id_2,
            discord_username="trivia_tester_2",
            guild_id=TEST_GUILD_ID,
            steam_id=12346,
            mmr_override=3000,
        )
        player_service.record_trivia_session(discord_id_2, TEST_GUILD_ID, streak=10, jc_earned=10)

        # Leaderboard should return player 2 first (higher streak)
        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert len(lb) == 2
        assert lb[0]["discord_id"] == discord_id_2
        assert lb[0]["best_streak"] == 10
        assert lb[1]["discord_id"] == registered_player
        assert lb[1]["best_streak"] == 5

    def test_leaderboard_uses_max_streak(self, player_service, registered_player):
        """If a player has multiple sessions, leaderboard shows their best."""
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=3, jc_earned=3)
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=8, jc_earned=8)
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=2, jc_earned=2)

        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert len(lb) == 1
        assert lb[0]["best_streak"] == 8

    def test_leaderboard_empty_when_no_sessions(self, player_service):
        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert lb == []

    def test_leaderboard_respects_time_window(self, player_service, registered_player):
        """Sessions older than the window should not appear."""
        repo = player_service.player_repo
        old_time = int(time.time()) - 8 * 86400  # 8 days ago
        repo.record_trivia_session(registered_player, TEST_GUILD_ID, streak=20, jc_earned=20, played_at=old_time)

        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID, days=7)
        assert lb == []

    def test_leaderboard_limit(self, player_service):
        """Leaderboard should respect the limit parameter."""
        for i in range(5):
            discord_id = 200000 + i
            player_service.register_player(
                discord_id=discord_id,
                discord_username=f"lb_test_{i}",
                guild_id=TEST_GUILD_ID,
                steam_id=50000 + i,
                mmr_override=3000,
            )
            player_service.record_trivia_session(discord_id, TEST_GUILD_ID, streak=i + 1, jc_earned=i + 1)

        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID, limit=3)
        assert len(lb) == 3
        # Top 3 by streak should be players with streaks 5, 4, 3
        assert lb[0]["best_streak"] == 5
        assert lb[1]["best_streak"] == 4
        assert lb[2]["best_streak"] == 3
