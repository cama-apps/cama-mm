"""
Tests for the Double or Nothing shop item.
"""

import time

import pytest

from config import DOUBLE_OR_NOTHING_COOLDOWN_SECONDS, SHOP_DOUBLE_OR_NOTHING_COST


TEST_GUILD_ID = 12345


class TestDoubleOrNothingRepository:
    """Tests for Double or Nothing repository methods."""

    def test_get_last_double_or_nothing_returns_none_for_new_player(self, player_repository):
        """New player has no previous Double or Nothing spin."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        result = player_repository.get_last_double_or_nothing(12345, TEST_GUILD_ID)
        assert result is None

    def test_log_double_or_nothing_updates_cooldown(self, player_repository):
        """Logging a spin updates the cooldown timestamp."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        now = int(time.time())
        player_repository.log_double_or_nothing(
            discord_id=12345,
            guild_id=TEST_GUILD_ID,
            cost=25,
            balance_before=75,
            balance_after=150,
            won=True,
            spin_time=now,
        )

        result = player_repository.get_last_double_or_nothing(12345, TEST_GUILD_ID)
        assert result == now

    def test_log_double_or_nothing_records_history(self, player_repository):
        """Spin history is recorded correctly."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        now = int(time.time())
        player_repository.log_double_or_nothing(
            discord_id=12345,
            guild_id=TEST_GUILD_ID,
            cost=25,
            balance_before=75,
            balance_after=150,
            won=True,
            spin_time=now,
        )

        history = player_repository.get_double_or_nothing_history(12345, TEST_GUILD_ID)
        assert len(history) == 1
        assert history[0]["cost"] == 25
        assert history[0]["balance_before"] == 75
        assert history[0]["balance_after"] == 150
        assert history[0]["won"] is True
        assert history[0]["spin_time"] == now

    def test_log_double_or_nothing_loss(self, player_repository):
        """Loss is recorded correctly."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        now = int(time.time())
        player_repository.log_double_or_nothing(
            discord_id=12345,
            guild_id=TEST_GUILD_ID,
            cost=25,
            balance_before=75,
            balance_after=0,
            won=False,
            spin_time=now,
        )

        history = player_repository.get_double_or_nothing_history(12345, TEST_GUILD_ID)
        assert len(history) == 1
        assert history[0]["balance_after"] == 0
        assert history[0]["won"] is False

    def test_multiple_spins_tracked(self, player_repository):
        """Multiple spins are tracked in order."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        now = int(time.time())

        # First spin - win
        player_repository.log_double_or_nothing(
            discord_id=12345,
            guild_id=TEST_GUILD_ID,
            cost=25,
            balance_before=75,
            balance_after=150,
            won=True,
            spin_time=now,
        )

        # Second spin - loss (after cooldown in reality, but testing history)
        later = now + DOUBLE_OR_NOTHING_COOLDOWN_SECONDS + 1
        player_repository.log_double_or_nothing(
            discord_id=12345,
            guild_id=TEST_GUILD_ID,
            cost=25,
            balance_before=125,
            balance_after=0,
            won=False,
            spin_time=later,
        )

        history = player_repository.get_double_or_nothing_history(12345, TEST_GUILD_ID)
        assert len(history) == 2
        assert history[0]["spin_time"] == now
        assert history[0]["won"] is True
        assert history[1]["spin_time"] == later
        assert history[1]["won"] is False

        # Last spin timestamp should be the later one
        last = player_repository.get_last_double_or_nothing(12345, TEST_GUILD_ID)
        assert last == later

    def test_get_history_empty_for_new_player(self, player_repository):
        """New player has empty spin history."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        history = player_repository.get_double_or_nothing_history(12345, TEST_GUILD_ID)
        assert history == []


class TestDoubleOrNothingCooldown:
    """Tests for Double or Nothing cooldown mechanics."""

    def test_cooldown_not_passed(self, player_repository):
        """Player on cooldown cannot play again."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        now = int(time.time())
        player_repository.log_double_or_nothing(
            discord_id=12345,
            guild_id=TEST_GUILD_ID,
            cost=25,
            balance_before=75,
            balance_after=150,
            won=True,
            spin_time=now,
        )

        # Check if on cooldown (this would be done in command handler)
        last_spin = player_repository.get_last_double_or_nothing(12345, TEST_GUILD_ID)
        elapsed = now - last_spin
        assert elapsed < DOUBLE_OR_NOTHING_COOLDOWN_SECONDS

    def test_cooldown_passed(self, player_repository):
        """Player can play after cooldown expires."""
        player_repository.add(discord_id=12345, discord_username="TestPlayer", guild_id=TEST_GUILD_ID)

        # Simulate a spin from 31 days ago
        old_spin_time = int(time.time()) - DOUBLE_OR_NOTHING_COOLDOWN_SECONDS - 86400
        player_repository.log_double_or_nothing(
            discord_id=12345,
            guild_id=TEST_GUILD_ID,
            cost=25,
            balance_before=75,
            balance_after=150,
            won=True,
            spin_time=old_spin_time,
        )

        # Check if cooldown has passed
        now = int(time.time())
        last_spin = player_repository.get_last_double_or_nothing(12345, TEST_GUILD_ID)
        elapsed = now - last_spin
        assert elapsed >= DOUBLE_OR_NOTHING_COOLDOWN_SECONDS


class TestDoubleOrNothingConfig:
    """Tests for Double or Nothing configuration."""

    def test_cost_default(self):
        """Cost has expected default value."""
        assert SHOP_DOUBLE_OR_NOTHING_COST == 50

    def test_cooldown_default(self):
        """Cooldown has expected default value (30 days)."""
        assert DOUBLE_OR_NOTHING_COOLDOWN_SECONDS == 2592000  # 30 days
