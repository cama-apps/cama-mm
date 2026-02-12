"""
Tests for next-match queue functionality in LobbyManagerService.
"""

import os
import tempfile
import time

from database import Database
from services.lobby_manager_service import LobbyManagerService as LobbyManager


def _cleanup_db_file(db_path: str) -> None:
    """Close sqlite handles and remove temp db with retries for Windows."""
    try:
        import sqlite3
        sqlite3.connect(db_path).close()
    except Exception:
        pass
    time.sleep(0.1)
    try:
        os.unlink(db_path)
    except PermissionError:
        time.sleep(0.2)
        try:
            os.unlink(db_path)
        except Exception:
            pass


class TestNextMatchQueue:
    """Test next-match queue operations on LobbyManagerService."""

    def test_join_next_lobby_creates_separate_queue(self):
        """Joining next lobby does not affect main lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=1)
        manager.join_lobby(101)
        manager.join_next_lobby(201)

        assert 101 in manager.get_lobby().players
        assert 201 not in manager.get_lobby().players
        assert 201 in manager.get_next_lobby().players
        assert 101 not in manager.get_next_lobby().players

    def test_join_next_lobby_auto_creates(self):
        """join_next_lobby creates next_lobby if none exists."""
        manager = LobbyManager(Database(db_path=":memory:"))
        assert manager.get_next_lobby() is None

        result = manager.join_next_lobby(201)
        assert result is True
        assert manager.get_next_lobby() is not None
        assert 201 in manager.get_next_lobby().players

    def test_join_next_lobby_duplicate(self):
        """Cannot join next lobby twice."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.join_next_lobby(201)
        result = manager.join_next_lobby(201)
        assert result is False

    def test_join_next_lobby_full(self):
        """Cannot join next lobby when full."""
        manager = LobbyManager(Database(db_path=":memory:"))
        for i in range(12):
            manager.join_next_lobby(200 + i)
        result = manager.join_next_lobby(299)
        assert result is False

    def test_leave_next_lobby(self):
        """Players can leave next_lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.join_next_lobby(201)
        manager.join_next_lobby(202)

        result = manager.leave_next_lobby(201)
        assert result is True
        assert 201 not in manager.get_next_lobby().players
        assert 202 in manager.get_next_lobby().players

    def test_leave_next_lobby_not_in_queue(self):
        """Leaving when not in next queue returns False."""
        manager = LobbyManager(Database(db_path=":memory:"))
        result = manager.leave_next_lobby(201)
        assert result is False

    def test_promote_next_lobby(self):
        """Promotion moves next_lobby players to main lobby slot."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.join_next_lobby(201)
        manager.join_next_lobby(202)

        promoted = manager.promote_next_lobby()
        assert promoted is True

        lobby = manager.get_lobby()
        assert lobby is not None
        assert {201, 202} == lobby.players
        assert lobby.lobby_id == manager.DEFAULT_LOBBY_ID
        assert manager.get_next_lobby() is None

    def test_promote_next_lobby_empty_queue_no_op(self):
        """Promotion with empty next_lobby returns False."""
        manager = LobbyManager(Database(db_path=":memory:"))
        promoted = manager.promote_next_lobby()
        assert promoted is False

    def test_promote_does_not_stomp_existing_main(self):
        """Promotion is a no-op if main lobby exists."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=1)
        manager.join_lobby(101)
        manager.join_next_lobby(201)

        promoted = manager.promote_next_lobby()
        assert promoted is False
        # Main lobby unchanged
        assert 101 in manager.get_lobby().players
        assert 201 not in manager.get_lobby().players
        # Next lobby still has player
        assert 201 in manager.get_next_lobby().players

    def test_reset_lobby_auto_promotes(self):
        """reset_lobby() auto-promotes next_lobby if it has players."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=1)
        manager.join_lobby(101)
        manager.join_next_lobby(201)
        manager.join_next_lobby(202)

        manager.reset_lobby()

        lobby = manager.get_lobby()
        assert lobby is not None
        assert {201, 202} == lobby.players
        assert 101 not in lobby.players
        assert manager.get_next_lobby() is None

    def test_reset_lobby_no_next_leaves_lobby_none(self):
        """reset_lobby() with empty next_lobby leaves lobby=None."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=1)
        manager.join_lobby(101)

        manager.reset_lobby()
        assert manager.get_lobby() is None

    def test_get_or_create_lobby_promotes_if_needed(self):
        """get_or_create_lobby() promotes next_lobby when main is None."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.join_next_lobby(201)
        # Ensure main lobby is None
        assert manager.lobby is None

        lobby = manager.get_or_create_lobby(creator_id=99)
        assert 201 in lobby.players


class TestNextMatchQueuePersistence:
    """Test next-match queue persistence across bot restarts."""

    def test_next_lobby_persists_across_restart(self):
        """next_lobby players survive bot restart."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.join_next_lobby(201)
            manager1.join_next_lobby(202)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            nl = manager2.get_next_lobby()
            assert nl is not None
            assert {201, 202} == nl.players
        finally:
            _cleanup_db_file(db_path)

    def test_promotion_clears_next_lobby_from_db(self):
        """After promotion, next_lobby is cleared from DB."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.join_next_lobby(201)
            manager1.promote_next_lobby()

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # Main lobby should have the promoted player
            lobby = manager2.get_lobby()
            assert lobby is not None
            assert 201 in lobby.players
            # Next lobby should be empty
            assert manager2.get_next_lobby() is None
        finally:
            _cleanup_db_file(db_path)

    def test_reset_with_promotion_persists(self):
        """reset_lobby with auto-promotion persists correctly."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=1)
            manager1.join_lobby(101)
            manager1.join_next_lobby(201)
            manager1.reset_lobby()

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            lobby = manager2.get_lobby()
            assert lobby is not None
            assert 201 in lobby.players
            assert 101 not in lobby.players
            assert manager2.get_next_lobby() is None
        finally:
            _cleanup_db_file(db_path)
