"""
Repository for managing player stake pool data in draft mode.
"""

from __future__ import annotations

from repositories.base_repository import BaseRepository
from repositories.interfaces import IStakeRepository


class StakeRepository(BaseRepository, IStakeRepository):
    """
    Handles CRUD operations for the player_stakes table.

    The player stake pool is a draft-only feature where players
    automatically stake a portion of a pool based on win probability.
    """

    @staticmethod
    def _normalize_guild_id(guild_id: int | None) -> int:
        return guild_id if guild_id is not None else 0

    def create_stakes(
        self,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        excluded_ids: list[int],
        stake_time: int,
    ) -> dict:
        """
        Create stake entries for all players in a draft.

        Args:
            guild_id: Guild ID for multi-guild support
            radiant_ids: Discord IDs of radiant team players
            dire_ids: Discord IDs of dire team players
            excluded_ids: Discord IDs of excluded players
            stake_time: Unix timestamp of stake creation

        Returns:
            Dict with creation summary: created count, team totals
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()

            created = 0

            # Create stakes for radiant team
            for discord_id in radiant_ids:
                cursor.execute(
                    """
                    INSERT INTO player_stakes
                    (guild_id, discord_id, team, is_excluded, stake_time)
                    VALUES (?, ?, 'radiant', 0, ?)
                    """,
                    (normalized_guild, discord_id, stake_time),
                )
                created += 1

            # Create stakes for dire team
            for discord_id in dire_ids:
                cursor.execute(
                    """
                    INSERT INTO player_stakes
                    (guild_id, discord_id, team, is_excluded, stake_time)
                    VALUES (?, ?, 'dire', 0, ?)
                    """,
                    (normalized_guild, discord_id, stake_time),
                )
                created += 1

            # Create stakes for excluded players (team='excluded')
            for discord_id in excluded_ids:
                cursor.execute(
                    """
                    INSERT INTO player_stakes
                    (guild_id, discord_id, team, is_excluded, stake_time)
                    VALUES (?, ?, 'excluded', 1, ?)
                    """,
                    (normalized_guild, discord_id, stake_time),
                )
                created += 1

        return {
            "created": created,
            "radiant_count": len(radiant_ids),
            "dire_count": len(dire_ids),
            "excluded_count": len(excluded_ids),
        }

    def settle_stakes_atomic(
        self,
        match_id: int,
        guild_id: int | None,
        since_ts: int,
        winning_team: str,
        payout_per_participant: int,
        payout_per_excluded: int,
    ) -> dict:
        """
        Atomically settle stakes for a completed match.

        - Assigns match_id to all pending stakes
        - Pays participating winners their parimutuel auto-stake share
        - Pays excluded players their minted odds-based payout
        - Updates player balances

        Args:
            match_id: The recorded match ID
            guild_id: Guild ID
            since_ts: Stake timestamp threshold
            winning_team: 'radiant' or 'dire'
            payout_per_participant: Parimutuel payout for winning team players
            payout_per_excluded: Minted payout for excluded players

        Returns:
            Dict with settlement summary
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        winners: list[dict] = []
        losers: list[dict] = []

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("BEGIN IMMEDIATE")

            # Get all pending stakes for this match
            cursor.execute(
                """
                SELECT stake_id, discord_id, team, is_excluded
                FROM player_stakes
                WHERE guild_id = ? AND match_id IS NULL AND stake_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            stakes = cursor.fetchall()

            if not stakes:
                return {"winners": [], "losers": [], "total_payout": 0}

            balance_deltas: dict[int, int] = {}
            payout_updates: list[tuple[int, int]] = []  # (payout, stake_id)

            for row in stakes:
                stake = dict(row)
                entry = {
                    "stake_id": stake["stake_id"],
                    "discord_id": stake["discord_id"],
                    "team": stake["team"],
                    "is_excluded": stake["is_excluded"],
                }

                # Determine payout based on player type
                if stake["is_excluded"] == 1:
                    # Excluded players get minted payout based on odds
                    payout = payout_per_excluded
                    balance_deltas[stake["discord_id"]] = (
                        balance_deltas.get(stake["discord_id"], 0) + payout
                    )
                    payout_updates.append((payout, stake["stake_id"]))
                    entry["payout"] = payout
                    winners.append(entry)
                elif stake["team"] == winning_team:
                    # Participating winners get parimutuel auto-stake payout
                    payout = payout_per_participant
                    balance_deltas[stake["discord_id"]] = (
                        balance_deltas.get(stake["discord_id"], 0) + payout
                    )
                    payout_updates.append((payout, stake["stake_id"]))
                    entry["payout"] = payout
                    winners.append(entry)
                else:
                    # Losing team gets nothing from auto-stake
                    losers.append(entry)

            # Update player balances
            if balance_deltas:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    [(delta, discord_id) for discord_id, delta in balance_deltas.items()],
                )

            # Store payout values
            if payout_updates:
                cursor.executemany(
                    "UPDATE player_stakes SET payout = ? WHERE stake_id = ?",
                    payout_updates,
                )

            # Assign match_id to all stakes
            cursor.execute(
                """
                UPDATE player_stakes
                SET match_id = ?
                WHERE guild_id = ? AND match_id IS NULL AND stake_time >= ?
                """,
                (match_id, normalized_guild, since_ts),
            )

        total_payout = sum(w.get("payout", 0) for w in winners)
        return {
            "winners": winners,
            "losers": losers,
            "total_payout": total_payout,
            "winning_team": winning_team,
        }

    def get_pending_stakes(
        self, guild_id: int | None, since_ts: int
    ) -> list[dict]:
        """
        Get all pending (unsettled) stakes for a guild.

        Args:
            guild_id: Guild ID
            since_ts: Stake timestamp threshold

        Returns:
            List of stake dicts
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT stake_id, guild_id, match_id, discord_id, team,
                       is_excluded, payout, stake_time, created_at
                FROM player_stakes
                WHERE guild_id = ? AND match_id IS NULL AND stake_time >= ?
                ORDER BY stake_time ASC
                """,
                (normalized_guild, since_ts),
            )
            return [dict(row) for row in cursor.fetchall()]

    def delete_stakes(
        self, guild_id: int | None, since_ts: int
    ) -> int:
        """
        Delete pending stakes (for draft abort/restart).

        No balance changes needed since stakes don't debit players.

        Args:
            guild_id: Guild ID
            since_ts: Stake timestamp threshold

        Returns:
            Number of stakes deleted
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                DELETE FROM player_stakes
                WHERE guild_id = ? AND match_id IS NULL AND stake_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            return cursor.rowcount

    def assign_match_id(
        self, guild_id: int | None, match_id: int, since_ts: int
    ) -> int:
        """
        Assign match ID to pending stakes without settling.

        Args:
            guild_id: Guild ID
            match_id: Match ID to assign
            since_ts: Stake timestamp threshold

        Returns:
            Number of stakes updated
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE player_stakes
                SET match_id = ?
                WHERE guild_id = ? AND match_id IS NULL AND stake_time >= ?
                """,
                (match_id, normalized_guild, since_ts),
            )
            return cursor.rowcount

    def get_player_stake_history(
        self, discord_id: int, limit: int = 50
    ) -> list[dict]:
        """
        Get stake history for a player.

        Args:
            discord_id: Player's Discord ID
            limit: Maximum number of records to return

        Returns:
            List of stake dicts with match info
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT ps.stake_id, ps.guild_id, ps.match_id, ps.team,
                       ps.is_excluded, ps.payout, ps.stake_time,
                       m.winning_team
                FROM player_stakes ps
                LEFT JOIN matches m ON ps.match_id = m.match_id
                WHERE ps.discord_id = ? AND ps.match_id IS NOT NULL
                ORDER BY ps.stake_time DESC
                LIMIT ?
                """,
                (discord_id, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_player_stake_stats(self, discord_id: int) -> dict:
        """
        Get aggregate stake statistics for a player.

        Returns:
            Dict with total stakes, wins, total payout, etc.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    COUNT(*) as total_stakes,
                    SUM(CASE WHEN payout > 0 THEN 1 ELSE 0 END) as winning_stakes,
                    SUM(COALESCE(payout, 0)) as total_payout,
                    SUM(CASE WHEN is_excluded = 1 THEN 1 ELSE 0 END) as excluded_stakes
                FROM player_stakes
                WHERE discord_id = ? AND match_id IS NOT NULL
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            if row:
                return dict(row)
            return {
                "total_stakes": 0,
                "winning_stakes": 0,
                "total_payout": 0,
                "excluded_stakes": 0,
            }
