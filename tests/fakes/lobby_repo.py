"""
In-memory fake for :class:`~repositories.interfaces.ILobbyRepository`.

Use ``FakeLobbyRepo`` anywhere a test needs an ``ILobbyRepository`` without
touching sqlite. State is held in a single dict keyed on
``(lobby_id, guild_id)``; ``None`` guild_ids are normalized to ``0`` to mirror
the real ``LobbyRepository`` behavior.

Adding a new method to ``ILobbyRepository`` only requires updating this one
fake — every test that rolls its own stub would otherwise have to update
individually.
"""

from __future__ import annotations

from repositories.interfaces import ILobbyRepository


def _normalize_guild_id(guild_id: int | None) -> int:
    return guild_id if guild_id is not None else 0


class FakeLobbyRepo(ILobbyRepository):
    """In-memory ``ILobbyRepository`` backed by a dict."""

    def __init__(self) -> None:
        # Keyed on (lobby_id, normalized_guild_id) -> stored state dict
        self._rows: dict[tuple[int, int], dict] = {}

    def save_lobby_state(
        self,
        lobby_id: int,
        players: list[int],
        status: str,
        created_by: int,
        created_at: str,
        message_id: int | None = None,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        conditional_players: list[int] | None = None,
        origin_channel_id: int | None = None,
        player_join_times: dict[int, float] | None = None,
        guild_id: int | None = None,
    ) -> None:
        normalized = _normalize_guild_id(guild_id)
        self._rows[(lobby_id, normalized)] = {
            "lobby_id": lobby_id,
            "guild_id": normalized,
            "players": list(players),
            "conditional_players": list(conditional_players or []),
            "player_join_times": dict(player_join_times or {}),
            "status": status,
            "created_by": created_by,
            "created_at": created_at,
            "message_id": message_id,
            "channel_id": channel_id,
            "thread_id": thread_id,
            "embed_message_id": embed_message_id,
            "origin_channel_id": origin_channel_id,
        }

    def load_lobby_state(
        self, lobby_id: int, guild_id: int | None = None
    ) -> dict | None:
        normalized = _normalize_guild_id(guild_id)
        row = self._rows.get((lobby_id, normalized))
        if row is None:
            return None
        # Return a shallow copy so tests mutating the result don't affect storage.
        return dict(row)

    def load_all_lobby_states(self) -> list[dict]:
        return [
            {"lobby_id": lobby_id, "guild_id": gid}
            for (lobby_id, gid) in self._rows.keys()
        ]

    def clear_lobby_state(self, lobby_id: int, guild_id: int | None = None) -> None:
        normalized = _normalize_guild_id(guild_id)
        self._rows.pop((lobby_id, normalized), None)
