"""
Repository for daily MTG mana land assignments.
"""

from repositories.base_repository import BaseRepository
from repositories.interfaces import IManaRepository

_BANKRUPT_BUFF_COLUMNS = {
    "insurance": "bankrupt_insurance_used",
    "reroll": "bankrupt_reroll_used",
}


def _buff_column(buff: str) -> str:
    if buff not in _BANKRUPT_BUFF_COLUMNS:
        raise ValueError(f"Unknown bankrupt buff: {buff!r}")
    return _BANKRUPT_BUFF_COLUMNS[buff]


class ManaRepository(BaseRepository, IManaRepository):
    """Stores and retrieves daily mana assignments (one row per player per guild)."""

    def get_mana(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Return {current_land, assigned_date} for the player, or None if never assigned."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT current_land, assigned_date FROM player_mana WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def set_mana(self, discord_id: int, guild_id: int | None, land: str, assigned_date: str) -> None:
        """Upsert today's mana for the player (replaces any previous value)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                """
                INSERT INTO player_mana (discord_id, guild_id, current_land, assigned_date, updated_at)
                VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    current_land  = excluded.current_land,
                    assigned_date = excluded.assigned_date,
                    updated_at    = CURRENT_TIMESTAMP
                """,
                (discord_id, gid, land, assigned_date),
            )

    def claim_mana_atomic(
        self, discord_id: int, guild_id: int | None, land: str, assigned_date: str
    ) -> bool:
        """Claim today's mana only if not already assigned for ``assigned_date``.

        Runs under BEGIN IMMEDIATE so two concurrent /mana calls can't both
        pass a pre-check and each roll a different land — the second caller
        observes the committed row and returns False.

        Resets the per-day bankruptcy buff flags so a new mana day grants fresh
        insurance / re-roll allowances.

        Returns True if the claim was applied, False if the player already
        has mana assigned for ``assigned_date``.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT assigned_date FROM player_mana WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            row = cursor.fetchone()
            if row and row["assigned_date"] == assigned_date:
                return False
            cursor.execute(
                """
                INSERT INTO player_mana (
                    discord_id, guild_id, current_land, assigned_date,
                    bankrupt_insurance_used, bankrupt_reroll_used, updated_at
                )
                VALUES (?, ?, ?, ?, 0, 0, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    current_land            = excluded.current_land,
                    assigned_date           = excluded.assigned_date,
                    bankrupt_insurance_used = 0,
                    bankrupt_reroll_used    = 0,
                    updated_at              = CURRENT_TIMESTAMP
                """,
                (discord_id, gid, land, assigned_date),
            )
            return True

    def is_bankrupt_buff_used(
        self, discord_id: int, guild_id: int | None, buff: str
    ) -> bool:
        column = _buff_column(buff)
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT {column} FROM player_mana WHERE discord_id = ? AND guild_id = ?",  # noqa: S608
                (discord_id, gid),
            )
            row = cursor.fetchone()
            return bool(row and row[column])

    def claim_bankrupt_buff_atomic(
        self, discord_id: int, guild_id: int | None, buff: str
    ) -> bool:
        """Atomically set the buff flag if not already set today.

        Returns True if the flag flipped from 0 → 1 (claim succeeded), False if
        already used for the current mana day or no mana row exists.
        """
        column = _buff_column(buff)
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT {column} FROM player_mana WHERE discord_id = ? AND guild_id = ?",  # noqa: S608
                (discord_id, gid),
            )
            row = cursor.fetchone()
            if row is None or row[column]:
                return False
            cursor.execute(
                f"UPDATE player_mana SET {column} = 1, updated_at = CURRENT_TIMESTAMP "  # noqa: S608
                "WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            return True

    def get_all_mana(self, guild_id: int | None) -> list[dict]:
        """Return all mana rows for the guild, ordered by current_land then discord_id."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, current_land, assigned_date
                FROM player_mana
                WHERE guild_id = ?
                ORDER BY current_land, discord_id
                """,
                (gid,),
            )
            return [dict(row) for row in cursor.fetchall()]

