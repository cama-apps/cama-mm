"""
Unit tests for lobby management.
"""

from datetime import datetime
from unittest.mock import Mock

import pytest

from domain.models.lobby import Lobby
from repositories.lobby_repository import LobbyRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from tests.fakes.lobby_repo import FakeLobbyRepo


class TestLobby:
    """Test Lobby class functionality."""

    def test_lobby_creation(self):
        """Test creating a lobby."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        assert lobby.lobby_id == 1
        assert lobby.created_by == 12345
        assert lobby.status == "open"
        assert len(lobby.players) == 0

    def test_legacy_conditional_players_are_ignored(self):
        """Persisted Frogling entries no longer participate in the lobby."""
        lobby = Lobby.from_dict(
            {
                "lobby_id": 1,
                "created_by": 12345,
                "created_at": datetime.now().isoformat(),
                "players": list(range(1, 10)),
                "conditional_players": [99],
                "player_join_times": {"1": 100.0, "99": 200.0},
            }
        )

        assert lobby.players == set(range(1, 10))
        assert lobby.conditional_players == set()
        assert lobby.get_total_count() == 9
        assert lobby.player_join_times == {1: 100.0}

    def test_add_and_remove_player(self):
        """Add/remove player lifecycle: success, duplicate add, closed lobby,
        and removing a player who isn't in the lobby.

        Merged from test_add_player, test_add_player_duplicate,
        test_add_player_closed_lobby, test_remove_player, and
        test_remove_player_not_in_lobby.
        """
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())

        # Removing a player who isn't in the lobby fails.
        assert lobby.remove_player(1001) is False

        # Adding succeeds once; the duplicate add is rejected.
        assert lobby.add_player(1001) is True
        assert 1001 in lobby.players
        assert lobby.get_player_count() == 1
        assert lobby.add_player(1001) is False
        assert lobby.get_player_count() == 1

        # Removing succeeds.
        assert lobby.remove_player(1001) is True
        assert 1001 not in lobby.players
        assert lobby.get_player_count() == 0

        # A closed lobby rejects joins.
        lobby.status = "closed"
        assert lobby.add_player(1002) is False
        assert 1002 not in lobby.players

    def test_is_ready(self):
        """Test checking if lobby is ready."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        # Add 10 players
        for i in range(10):
            lobby.add_player(1000 + i)
        assert lobby.is_ready() is True
        assert lobby.is_ready(min_players=12) is False

    def test_can_create_teams(self):
        """can_create_teams: True with full role diversity, False when all
        players share one role (merged from test_can_create_teams_insufficient_roles).
        """
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        # Add 10 players with roles
        player_roles = {}
        for i, role in enumerate(["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]):
            player_id = 1000 + i
            lobby.add_player(player_id)
            player_roles[player_id] = [role]

        assert lobby.can_create_teams(player_roles) is True

        # All-carry role map cannot form balanced teams.
        all_carry_roles = {player_id: ["1"] for player_id in player_roles}
        assert lobby.can_create_teams(all_carry_roles) is False


class TestLobbyManager:
    """Test LobbyManager class functionality."""

    def test_get_or_create_and_get_lobby_lifecycle(self):
        """get_or_create/get lifecycle: none exists → None, create, retrieve,
        closed lobbies not returned.

        Merged from test_get_or_create_lobby, test_get_lobby_none,
        test_get_lobby_exists, and test_get_lobby_closed.
        """
        manager = LobbyManager(FakeLobbyRepo())
        assert manager.get_lobby() is None

        created = manager.get_or_create_lobby(creator_id=12345)
        assert created is not None
        assert created.created_by == 12345
        assert manager.get_lobby() is not None

        created.status = "closed"
        assert manager.get_lobby() is None

    def test_join_and_leave_lobby(self):
        """Join/leave through the manager: join ok, leave ok, leave when not
        in lobby fails, join when full rejected.

        Merged from test_join_lobby, test_join_lobby_full, test_leave_lobby,
        and test_leave_lobby_not_in_lobby.
        """
        manager = LobbyManager(FakeLobbyRepo())
        assert manager.leave_lobby(1001) is False

        assert manager.join_lobby(1001) == "ok"
        assert 1001 in manager.get_lobby().players
        assert manager.leave_lobby(1001) is True
        assert 1001 not in manager.get_lobby().players

        # Fill lobby to 12 players; the 13th join is rejected.
        for i in range(12):
            manager.join_lobby(2000 + i)
        assert manager.join_lobby(9999) == "full"
        lobby = manager.get_lobby()
        assert 9999 not in lobby.players
        assert lobby.get_player_count() == 12

    def test_reset_lobby(self):
        """Resetting the lobby clears the lobby, message_id, and channel_id
        (merged from test_reset_lobby_clears_channel_id)."""
        manager = LobbyManager(FakeLobbyRepo())
        manager.get_or_create_lobby()
        manager.join_lobby(1001)
        manager.set_lobby_message(message_id=12345, channel_id=67890)

        manager.reset_lobby()

        assert manager.get_lobby(guild_id=0) is None
        assert manager.get_lobby_message_id(guild_id=0) is None
        assert manager.get_lobby_channel_id(guild_id=0) is None


class TestLobbyPersistence:
    """Test lobby state persistence across bot restarts."""

    def test_fake_load_all_returns_full_shallow_copies(self):
        repo = FakeLobbyRepo()
        repo.save_lobby_state(
            lobby_id=1,
            guild_id=42,
            players=[101, 102],
            status="open",
            created_by=1001,
            created_at="2026-07-22T12:00:00+00:00",
            message_id=2001,
            channel_id=2002,
            thread_id=2003,
            embed_message_id=2004,
            origin_channel_id=2005,
            player_join_times={101: 100.5, 102: 200.5},
        )

        rows = repo.load_all_lobby_states()

        assert rows == [repo.load_lobby_state(1, guild_id=42)]
        rows[0]["status"] = "closed"
        assert repo.load_lobby_state(1, guild_id=42)["status"] == "open"

    def test_startup_bulk_rehydrates_all_guild_metadata_without_point_reads(self):
        repo = FakeLobbyRepo()
        states = {
            42: {
                "players": [101, 102],
                "created_by": 1001,
                "created_at": "2026-07-22T12:00:00+00:00",
                "message_id": 2001,
                "channel_id": 2002,
                "thread_id": 2003,
                "embed_message_id": 2004,
                "origin_channel_id": 2005,
                "player_join_times": {101: 100.5, 102: 200.5},
            },
            84: {
                "players": [201, 202, 203],
                "created_by": 3001,
                "created_at": "2026-07-22T13:00:00+00:00",
                "message_id": 4001,
                "channel_id": 4002,
                "thread_id": 4003,
                "embed_message_id": 4004,
                "origin_channel_id": 4005,
                "player_join_times": {201: 300.5, 202: 400.5, 203: 500.5},
            },
        }
        for guild_id, state in states.items():
            repo.save_lobby_state(
                lobby_id=1,
                guild_id=guild_id,
                players=state["players"],
                status="open",
                created_by=state["created_by"],
                created_at=state["created_at"],
                message_id=state["message_id"],
                channel_id=state["channel_id"],
                thread_id=state["thread_id"],
                embed_message_id=state["embed_message_id"],
                origin_channel_id=state["origin_channel_id"],
                player_join_times=state["player_join_times"],
            )

        load_all = Mock(wraps=repo.load_all_lobby_states)
        point_load = Mock(
            side_effect=AssertionError("startup must hydrate from the bulk rows")
        )
        repo.load_all_lobby_states = load_all
        repo.load_lobby_state = point_load

        manager = LobbyManager(repo)

        load_all.assert_called_once_with()
        point_load.assert_not_called()
        for guild_id, state in states.items():
            lobby = manager.get_lobby(guild_id=guild_id)
            assert lobby is not None
            assert lobby.lobby_id == 1
            assert lobby.guild_id == guild_id
            assert lobby.players == set(state["players"])
            assert lobby.status == "open"
            assert lobby.created_by == state["created_by"]
            assert lobby.created_at == datetime.fromisoformat(state["created_at"])
            assert lobby.player_join_times == state["player_join_times"]
            assert manager.get_lobby_message_id(guild_id) == state["message_id"]
            assert manager.get_lobby_channel_id(guild_id) == state["channel_id"]
            assert manager.get_lobby_thread_id(guild_id) == state["thread_id"]
            assert (
                manager.get_lobby_embed_message_id(guild_id)
                == state["embed_message_id"]
            )
            assert (
                manager.get_origin_channel_id(guild_id)
                == state["origin_channel_id"]
            )

    def test_startup_persists_sanitized_legacy_conditional_state(self, repo_db_path):
        guild_id = 42
        repo = LobbyRepository(repo_db_path)
        repo.save_lobby_state(
            lobby_id=1,
            guild_id=guild_id,
            players=[1],
            conditional_players=[99],
            status="open",
            created_by=1,
            created_at=datetime.now().isoformat(),
            message_id=100,
            channel_id=200,
            player_join_times={1: 1000.0, 99: 2000.0},
        )

        manager = LobbyManager(LobbyRepository(repo_db_path))

        restored = manager.get_lobby(guild_id=guild_id)
        stored = repo.load_lobby_state(1, guild_id=guild_id)
        assert restored is not None
        assert restored.conditional_players == set()
        assert stored is not None
        assert stored["conditional_players"] == []
        assert stored["player_join_times"] == {1: 1000.0}
        assert stored["message_id"] == 100
        assert stored["channel_id"] == 200

    def test_restart_scenario_preserves_lobby_and_message_state(self, tmp_path):
        """Full restart scenario for lobby persistence.

        Merged from: test_message_and_channel_ids_persist_across_restart,
        test_players_persist_across_restart, test_can_join_lobby_after_restart,
        test_can_leave_lobby_after_restart, test_lobby_creator_persists_across_restart,
        test_lobby_status_persists_across_restart,
        test_set_lobby_message_persists_immediately,
        test_join_persists_message_id, test_leave_persists_message_id, and
        test_multiple_restarts_preserve_state — all slices of the same
        create → mutate → restart → verify flow.
        """
        db_path = str(tmp_path / "lobby_restart.db")

        # Session 1: create lobby, add players, set message ids. No explicit
        # save call — set_lobby_message must persist immediately.
        manager1 = LobbyManager(LobbyRepository(db_path))
        manager1.get_or_create_lobby(creator_id=99999)
        manager1.join_lobby(1001)
        manager1.join_lobby(1002)
        manager1.set_lobby_message(message_id=111222333, channel_id=444555666)
        assert manager1.get_lobby_message_id(guild_id=0) == 111222333
        assert manager1.get_lobby_channel_id(guild_id=0) == 444555666

        # Session 2 (restart): creator, status, players and message ids restored.
        manager2 = LobbyManager(LobbyRepository(db_path))
        lobby = manager2.get_lobby()
        assert lobby is not None
        assert lobby.created_by == 99999
        assert lobby.status == "open"
        assert lobby.players == {1001, 1002}
        assert manager2.get_lobby_message_id(guild_id=0) == 111222333
        assert manager2.get_lobby_channel_id(guild_id=0) == 444555666

        # Join and leave still work after the restart (both persist).
        assert manager2.join_lobby(1003) == "ok"
        assert manager2.leave_lobby(1001) is True

        # Session 3 (second restart): the post-restart join/leave stuck, and
        # message ids survived the join/leave persists.
        manager3 = LobbyManager(LobbyRepository(db_path))
        lobby3 = manager3.get_lobby()
        assert lobby3 is not None
        assert lobby3.players == {1002, 1003}
        assert manager3.get_lobby_message_id(guild_id=0) == 111222333
        assert manager3.get_lobby_channel_id(guild_id=0) == 444555666

    def test_closed_lobby_not_restored_after_restart(self, tmp_path):
        """Test that a closed lobby doesn't restore message IDs."""
        db_path = str(tmp_path / "lobby_closed.db")

        manager1 = LobbyManager(LobbyRepository(db_path))
        manager1.get_or_create_lobby(creator_id=12345)
        manager1.set_lobby_message(message_id=111, channel_id=222)
        manager1.reset_lobby()  # Close the lobby

        # Simulate restart
        manager2 = LobbyManager(LobbyRepository(db_path))

        # Lobby should not exist
        assert manager2.get_lobby() is None
        # Message IDs should be None since lobby was reset
        assert manager2.get_lobby_message_id(guild_id=0) is None
        assert manager2.get_lobby_channel_id(guild_id=0) is None

    def test_message_id_edge_cases_persist(self, tmp_path):
        """Message-id persistence edge cases across restarts.

        Merged from: test_message_id_without_channel_id,
        test_empty_lobby_still_has_message_id, and
        test_update_message_id_persists.
        """
        db_path = str(tmp_path / "lobby_message_edges.db")

        # message_id set with channel_id=None; a player joins then leaves so
        # the lobby ends up empty.
        manager1 = LobbyManager(LobbyRepository(db_path))
        manager1.get_or_create_lobby(creator_id=12345)
        manager1.set_lobby_message(message_id=111, channel_id=None)
        manager1.join_lobby(1001)
        manager1.leave_lobby(1001)

        # Restart: message_id restored, channel_id None, empty lobby usable.
        manager2 = LobbyManager(LobbyRepository(db_path))
        assert manager2.get_lobby_message_id(guild_id=0) == 111
        assert manager2.get_lobby_channel_id(guild_id=0) is None
        lobby = manager2.get_lobby()
        assert lobby is not None
        assert lobby.get_player_count() == 0

        # Update to a new message (e.g., lobby command run again); the new
        # values win after another restart.
        manager2.set_lobby_message(message_id=333, channel_id=444)
        manager3 = LobbyManager(LobbyRepository(db_path))
        assert manager3.get_lobby_message_id(guild_id=0) == 333
        assert manager3.get_lobby_channel_id(guild_id=0) == 444


class TestLobbyMultiGuildIsolation:
    """Regression tests: lobbies must be isolated across Discord guilds."""

    def test_lobby_state_is_per_guild(self, repo_db_path):
        """Two guilds get two independent Lobby instances with distinct players."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        manager = LobbyManager(LobbyRepository(repo_db_path))

        lobby_a = manager.get_or_create_lobby(
            creator_id=1001, guild_id=TEST_GUILD_ID
        )
        lobby_b = manager.get_or_create_lobby(
            creator_id=2001, guild_id=TEST_GUILD_ID_SECONDARY
        )

        # Distinct objects, tagged with their own guild_id
        assert lobby_a is not lobby_b
        assert lobby_a.guild_id == TEST_GUILD_ID
        assert lobby_b.guild_id == TEST_GUILD_ID_SECONDARY

        assert manager.join_lobby(111, guild_id=TEST_GUILD_ID) == "ok"
        assert manager.join_lobby(222, guild_id=TEST_GUILD_ID) == "ok"
        assert manager.join_lobby(333, guild_id=TEST_GUILD_ID_SECONDARY) == "ok"
        assert manager.join_lobby(444, guild_id=TEST_GUILD_ID_SECONDARY) == "ok"

        a = manager.get_lobby(guild_id=TEST_GUILD_ID)
        b = manager.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert a is not None and b is not None

        # Guild A only sees 111/222; Guild B only sees 333/444
        assert a.players == {111, 222}
        assert b.players == {333, 444}
        # And no cross-leakage
        assert 333 not in a.players and 444 not in a.players
        assert 111 not in b.players and 222 not in b.players

    def test_reset_one_guild_leaves_other_alone(self, repo_db_path):
        """Resetting guild A's lobby must not touch guild B's state."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        manager = LobbyManager(LobbyRepository(repo_db_path))

        manager.get_or_create_lobby(creator_id=1, guild_id=TEST_GUILD_ID)
        manager.get_or_create_lobby(creator_id=2, guild_id=TEST_GUILD_ID_SECONDARY)
        manager.join_lobby(111, guild_id=TEST_GUILD_ID)
        manager.join_lobby(222, guild_id=TEST_GUILD_ID_SECONDARY)
        manager.set_lobby_message(
            message_id=9001, channel_id=9002, guild_id=TEST_GUILD_ID
        )
        manager.set_lobby_message(
            message_id=9101, channel_id=9102, guild_id=TEST_GUILD_ID_SECONDARY
        )

        manager.reset_lobby(guild_id=TEST_GUILD_ID)

        assert manager.get_lobby(guild_id=TEST_GUILD_ID) is None
        assert manager.get_lobby_message_id(guild_id=TEST_GUILD_ID) is None
        assert manager.get_lobby_channel_id(guild_id=TEST_GUILD_ID) is None

        b = manager.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert b is not None
        assert b.players == {222}
        assert manager.get_lobby_message_id(guild_id=TEST_GUILD_ID_SECONDARY) == 9101
        assert manager.get_lobby_channel_id(guild_id=TEST_GUILD_ID_SECONDARY) == 9102

    def test_multi_guild_lobbies_survive_restart(self, repo_db_path):
        """Both guilds' lobbies persist across a manager restart and stay distinct."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        repo = LobbyRepository(repo_db_path)
        manager1 = LobbyManager(repo)

        manager1.get_or_create_lobby(creator_id=1001, guild_id=TEST_GUILD_ID)
        manager1.get_or_create_lobby(creator_id=2001, guild_id=TEST_GUILD_ID_SECONDARY)
        manager1.join_lobby(111, guild_id=TEST_GUILD_ID)
        manager1.join_lobby(222, guild_id=TEST_GUILD_ID)
        manager1.join_lobby(333, guild_id=TEST_GUILD_ID_SECONDARY)

        # Fresh manager over the same persistent repo
        manager2 = LobbyManager(repo)

        a = manager2.get_lobby(guild_id=TEST_GUILD_ID)
        b = manager2.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert a is not None and b is not None
        assert a.players == {111, 222}
        assert b.players == {333}
        assert a.guild_id == TEST_GUILD_ID
        assert b.guild_id == TEST_GUILD_ID_SECONDARY

    def test_leave_is_scoped_to_guild(self, repo_db_path):
        """Leaving in guild A must not remove a player from guild B's lobby."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        manager = LobbyManager(LobbyRepository(repo_db_path))
        # Same discord id is in both guilds' lobbies
        manager.join_lobby(555, guild_id=TEST_GUILD_ID)
        manager.join_lobby(555, guild_id=TEST_GUILD_ID_SECONDARY)

        assert manager.leave_lobby(555, guild_id=TEST_GUILD_ID) is True
        a = manager.get_lobby(guild_id=TEST_GUILD_ID)
        b = manager.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert a is not None and 555 not in a.players
        assert b is not None and 555 in b.players


class TestStaleReadycheckPrune:
    """Verify that stale readycheck pruning removes players from LobbyManagerService state."""

    def test_leave_lobby_removes_player_from_in_memory_state(self):
        """Calling leave_lobby removes the player from the in-memory lobby.

        This is the primitive relied on by the prune path; confirming it works
        end-to-end through the manager catches regressions before they hit the
        command layer.
        """
        manager = LobbyManager(FakeLobbyRepo())
        guild_id = 0

        # Seed a lobby with 4 players
        for pid in [101, 102, 103, 104]:
            manager.join_lobby(pid, guild_id=guild_id)

        lobby = manager.get_lobby(guild_id=guild_id)
        assert lobby is not None
        assert {101, 102, 103, 104} == lobby.players

        # Prune two of them (simulating the stale readycheck removal loop)
        pruned = [102, 104]
        for pid in pruned:
            manager.leave_lobby(pid, guild_id=guild_id)

        lobby_after = manager.get_lobby(guild_id=guild_id)
        assert lobby_after is not None
        assert lobby_after.players == {101, 103}, "pruned players must be absent"
        assert 102 not in lobby_after.players
        assert 104 not in lobby_after.players

    def test_conditional_lobby_join_is_deprecated(self):
        manager = LobbyManager(FakeLobbyRepo())
        guild_id = 0

        manager.join_lobby(1, guild_id=guild_id)
        result = manager.join_lobby_conditional(2, guild_id=guild_id)

        lobby = manager.get_lobby(guild_id=guild_id)
        assert result == "deprecated"
        assert lobby is not None
        assert lobby.players == {1}
        assert lobby.conditional_players == set()

    def test_prune_does_not_affect_other_guild(self):
        """Removing a player from guild A does not touch guild B's lobby."""
        manager = LobbyManager(FakeLobbyRepo())

        manager.join_lobby(10, guild_id=100)
        manager.join_lobby(10, guild_id=200)

        manager.leave_lobby(10, guild_id=100)

        lobby_a = manager.get_lobby(guild_id=100)
        lobby_b = manager.get_lobby(guild_id=200)
        assert lobby_a is None or 10 not in lobby_a.players
        assert lobby_b is not None and 10 in lobby_b.players


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
