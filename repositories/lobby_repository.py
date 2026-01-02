"""
Repository for lobby persistence.
"""

import json
from typing import Dict, List, Optional

from repositories.base_repository import BaseRepository
from repositories.interfaces import ILobbyRepository


class LobbyRepository(BaseRepository, ILobbyRepository):
    """
    Handles lobby_state persistence.
    """

    def save_lobby_state(self, lobby_id: int, players: List[int], status: str, created_by: int, created_at: str) -> None:
        payload = json.dumps(players)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO lobby_state (lobby_id, players, status, created_by, created_at)
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(lobby_id) DO UPDATE SET
                    players = excluded.players,
                    status = excluded.status,
                    created_by = excluded.created_by,
                    created_at = excluded.created_at,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (lobby_id, payload, status, created_by, created_at),
            )

    def load_lobby_state(self, lobby_id: int) -> Optional[Dict]:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM lobby_state WHERE lobby_id = ?", (lobby_id,))
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "lobby_id": row["lobby_id"],
                "players": json.loads(row["players"]) if row["players"] else [],
                "status": row["status"],
                "created_by": row["created_by"],
                "created_at": row["created_at"],
            }

    def clear_lobby_state(self, lobby_id: int) -> None:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM lobby_state WHERE lobby_id = ?", (lobby_id,))

