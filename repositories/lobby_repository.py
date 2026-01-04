"""
Repository for lobby persistence.
"""

import json

from repositories.base_repository import BaseRepository
from repositories.interfaces import ILobbyRepository


class LobbyRepository(BaseRepository, ILobbyRepository):
    """
    Handles lobby_state persistence.
    """

    def save_lobby_state(
        self,
        lobby_id: int,
        players: list[int],
        status: str,
        created_by: int,
        created_at: str,
        message_id: int | None = None,
        channel_id: int | None = None,
    ) -> None:
        payload = json.dumps(players)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO lobby_state (lobby_id, players, status, created_by, created_at, message_id, channel_id)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(lobby_id) DO UPDATE SET
                    players = excluded.players,
                    status = excluded.status,
                    created_by = excluded.created_by,
                    created_at = excluded.created_at,
                    message_id = excluded.message_id,
                    channel_id = excluded.channel_id,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (lobby_id, payload, status, created_by, created_at, message_id, channel_id),
            )

    def load_lobby_state(self, lobby_id: int) -> dict | None:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM lobby_state WHERE lobby_id = ?", (lobby_id,))
            row = cursor.fetchone()
            if not row:
                return None
            row_dict = dict(row)
            return {
                "lobby_id": row_dict["lobby_id"],
                "players": json.loads(row_dict["players"]) if row_dict.get("players") else [],
                "status": row_dict["status"],
                "created_by": row_dict["created_by"],
                "created_at": row_dict["created_at"],
                "message_id": row_dict.get("message_id"),
                "channel_id": row_dict.get("channel_id"),
            }

    def clear_lobby_state(self, lobby_id: int) -> None:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM lobby_state WHERE lobby_id = ?", (lobby_id,))
