"""
Tests for the /readycheck feature: join timestamps on Lobby model,
persistence through lobby repository, and the _is_playing_dota helper.
"""

import time
from datetime import datetime
from unittest.mock import MagicMock

import discord
import pytest

from domain.models.lobby import Lobby
from services.lobby_manager_service import LobbyManagerService
from tests.fakes.lobby_repo import FakeLobbyRepo

# ---------------------------------------------------------------------------
# Lobby model: join timestamp tracking
# ---------------------------------------------------------------------------


class TestLobbyJoinTimestamps:
    @pytest.mark.parametrize(
        ("method_name", "discord_id"),
        [("add_player", 100), ("add_conditional_player", 200)],
    )
    def test_add_player_sets_join_time(self, monkeypatch, method_name, discord_id):
        fixed_now = datetime(2026, 7, 14, 12, 34, 56, 123456)
        fake_datetime = MagicMock()
        fake_datetime.now.return_value = fixed_now
        monkeypatch.setattr("domain.models.lobby.datetime", fake_datetime)
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())

        getattr(lobby, method_name)(discord_id)

        assert lobby.player_join_times[discord_id] == fixed_now.timestamp()

    def test_switch_queue_preserves_original_join_time(self):
        """Switching from conditional to regular should keep the original timestamp."""
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        lobby.add_conditional_player(100)

        # Pin an explicit, deterministic original timestamp that is nowhere near
        # "now". If the switch wrongly re-stamps the join time with the current
        # clock, this sentinel value would be overwritten and the assertion fails.
        original_time = 1_000_000.0
        lobby.player_join_times[100] = original_time

        lobby.add_player(100)
        assert lobby.player_join_times[100] == original_time
        assert 100 in lobby.players
        assert 100 not in lobby.conditional_players

    def test_switch_regular_to_conditional_preserves_join_time(self):
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        lobby.add_player(100)

        # Explicit sentinel timestamp (see above): the switch must preserve it.
        original_time = 1_000_000.0
        lobby.player_join_times[100] = original_time

        lobby.add_conditional_player(100)
        assert lobby.player_join_times[100] == original_time
        assert 100 in lobby.conditional_players
        assert 100 not in lobby.players

    def test_remove_player_clears_join_time(self):
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        lobby.add_player(100)
        assert 100 in lobby.player_join_times

        lobby.remove_player(100)
        assert 100 not in lobby.player_join_times

    def test_remove_conditional_player_clears_join_time(self):
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        lobby.add_conditional_player(200)
        assert 200 in lobby.player_join_times

        lobby.remove_conditional_player(200)
        assert 200 not in lobby.player_join_times

    def test_remove_nonexistent_player_no_error(self):
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        assert lobby.remove_player(999) is False
        assert 999 not in lobby.player_join_times


# ---------------------------------------------------------------------------
# Lobby model: to_dict / from_dict roundtrip
# ---------------------------------------------------------------------------


class TestLobbyJoinTimeSerialization:
    def test_to_dict_includes_join_times(self):
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        lobby.add_player(100)
        lobby.add_conditional_player(200)

        data = lobby.to_dict()
        assert "player_join_times" in data
        # Keys should be strings in serialized form
        assert "100" in data["player_join_times"]
        assert "200" in data["player_join_times"]

    def test_roundtrip_preserves_join_times(self):
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        lobby.add_player(100)
        lobby.add_conditional_player(200)

        data = lobby.to_dict()
        restored = Lobby.from_dict(data)

        assert restored.player_join_times[100] == lobby.player_join_times[100]
        assert restored.player_join_times[200] == lobby.player_join_times[200]

    def test_from_dict_missing_join_times_defaults_empty(self):
        """Backward compatibility: old lobbies without player_join_times."""
        data = {
            "lobby_id": 1,
            "created_by": 0,
            "created_at": datetime.now().isoformat(),
            "players": [100, 200],
            "conditional_players": [300],
            "status": "open",
        }
        lobby = Lobby.from_dict(data)
        assert lobby.player_join_times == {}


# ---------------------------------------------------------------------------
# Lobby repository: persistence
# ---------------------------------------------------------------------------


class TestLobbyRepositoryJoinTimes:
    def test_save_and_load_join_times(self, lobby_repository):
        join_times = {100: 1700000000.0, 200: 1700000060.0}
        lobby_repository.save_lobby_state(
            lobby_id=1,
            players=[100, 200],
            status="open",
            created_by=0,
            created_at=datetime.now().isoformat(),
            player_join_times=join_times,
        )

        data = lobby_repository.load_lobby_state(1)
        assert data is not None
        assert data["player_join_times"] == {100: 1700000000.0, 200: 1700000060.0}

    def test_load_without_join_times_defaults_empty(self, lobby_repository):
        """If player_join_times column has default '{}', loading works."""
        lobby_repository.save_lobby_state(
            lobby_id=1,
            players=[100],
            status="open",
            created_by=0,
            created_at=datetime.now().isoformat(),
        )

        data = lobby_repository.load_lobby_state(1)
        assert data is not None
        assert data["player_join_times"] == {}

    def test_full_lobby_roundtrip_with_join_times(self, lobby_repository):
        """Full roundtrip: Lobby -> save -> load -> Lobby."""
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        lobby.add_player(100)
        lobby.add_player(200)
        lobby.add_conditional_player(300)

        lobby_repository.save_lobby_state(
            lobby_id=lobby.lobby_id,
            players=list(lobby.players),
            status=lobby.status,
            created_by=lobby.created_by,
            created_at=lobby.created_at.isoformat(),
            conditional_players=list(lobby.conditional_players),
            player_join_times=lobby.player_join_times,
        )

        data = lobby_repository.load_lobby_state(1)
        restored = Lobby.from_dict(data)

        assert restored.player_join_times[100] == lobby.player_join_times[100]
        assert restored.player_join_times[200] == lobby.player_join_times[200]
        assert restored.player_join_times[300] == lobby.player_join_times[300]


# ---------------------------------------------------------------------------
# _is_playing_dota helper
# ---------------------------------------------------------------------------


class TestIsPlayingDota:
    def test_playing_dota_game(self):
        from commands.lobby import _is_playing_dota

        member = MagicMock(spec=discord.Member)
        game = discord.Game(name="Dota 2")
        member.activities = [game]
        assert _is_playing_dota(member) is True

    def test_playing_dota_case_insensitive(self):
        from commands.lobby import _is_playing_dota

        member = MagicMock(spec=discord.Member)
        game = discord.Game(name="dota 2")
        member.activities = [game]
        assert _is_playing_dota(member) is True

    def test_not_playing_dota(self):
        from commands.lobby import _is_playing_dota

        member = MagicMock(spec=discord.Member)
        game = discord.Game(name="Counter-Strike 2")
        member.activities = [game]
        assert _is_playing_dota(member) is False

    def test_no_activities(self):
        from commands.lobby import _is_playing_dota

        member = MagicMock(spec=discord.Member)
        member.activities = []
        assert _is_playing_dota(member) is False

    def test_activity_with_dota_substring(self):
        from commands.lobby import _is_playing_dota

        member = MagicMock(spec=discord.Member)
        activity = MagicMock(spec=discord.Activity)
        activity.name = "Playing Dota 2 with friends"
        member.activities = [activity]
        assert _is_playing_dota(member) is True


# ---------------------------------------------------------------------------
# Readycheck precondition: lobby size
# ---------------------------------------------------------------------------


class TestReadycheckPreconditions:
    def test_lobby_not_ready_under_10(self):
        """get_total_count is below 10 for a partial roster (full-game threshold)."""
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        for i in range(9):
            lobby.add_player(i)
        assert lobby.get_total_count() < 10

    def test_lobby_ready_at_10(self):
        """get_total_count reaches the 10-player full-game threshold."""
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        for i in range(10):
            lobby.add_player(i)
        assert lobby.get_total_count() >= 10

    def test_lobby_mixed_regular_and_conditional(self):
        """Combined count of regular + conditional should count toward 10."""
        lobby = Lobby(lobby_id=1, created_by=0, created_at=datetime.now())
        for i in range(7):
            lobby.add_player(i)
        for i in range(7, 10):
            lobby.add_conditional_player(i)
        assert lobby.get_total_count() == 10


# ---------------------------------------------------------------------------
# Readycheck message post-time (drives the 30-min staleness check)
# ---------------------------------------------------------------------------


class TestReadycheckCreatedAt:
    """set_readycheck_state records when the message was posted; the next
    /readycheck compares against it to decide whether to repost fresh."""

    def test_set_readycheck_state_records_created_at(self):
        mgr = LobbyManagerService(FakeLobbyRepo())
        mgr.get_or_create_lobby(creator_id=1)
        mgr.set_readycheck_state(111, 222, {1, 2}, {}, created_at=1000.0)
        assert mgr.get_readycheck_created_at(guild_id=0) == 1000.0

    def test_set_readycheck_state_defaults_created_at_to_now(self):
        mgr = LobbyManagerService(FakeLobbyRepo())
        mgr.get_or_create_lobby(creator_id=1)
        before = time.time()
        mgr.set_readycheck_state(111, 222, {1}, {})
        after = time.time()
        ts = mgr.get_readycheck_created_at(guild_id=0)
        assert ts is not None and before <= ts <= after

    def test_get_readycheck_created_at_defaults_none(self):
        mgr = LobbyManagerService(FakeLobbyRepo())
        assert mgr.get_readycheck_created_at(guild_id=0) is None

    def test_reset_lobby_clears_created_at(self):
        mgr = LobbyManagerService(FakeLobbyRepo())
        mgr.get_or_create_lobby(creator_id=1)
        mgr.set_readycheck_state(111, 222, {1}, {}, created_at=1000.0)
        mgr.reset_lobby()
        assert mgr.get_readycheck_created_at(guild_id=0) is None
