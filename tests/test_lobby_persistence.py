import os
import tempfile

from domain.models.lobby import LobbyManager
from repositories.lobby_repository import LobbyRepository


def test_lobby_manager_persists_and_recovers_state():
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        lobby_repo = LobbyRepository(db_path)
        manager1 = LobbyManager(lobby_repo)

        lobby = manager1.get_or_create_lobby(creator_id=42)
        lobby.add_player(111)
        lobby.add_player(222)
        manager1._persist_lobby()

        # New instance should load persisted state
        manager2 = LobbyManager(lobby_repo)
        loaded = manager2.get_lobby()
        assert loaded is not None
        assert loaded.players == {111, 222}
        assert loaded.created_by == 42
    finally:
        try:
            os.unlink(db_path)
        except OSError:
            pass
