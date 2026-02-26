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
