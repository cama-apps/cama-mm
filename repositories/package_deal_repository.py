"""
Repository for package deal data access.
"""

import logging
import time
from dataclasses import dataclass

from repositories.base_repository import BaseRepository
from repositories.interfaces import IPackageDealRepository

logger = logging.getLogger("cama_bot.repositories.package_deal")


@dataclass
class PackageDeal:
    """Represents an active package deal."""
    id: int
    guild_id: int
    buyer_discord_id: int
    partner_discord_id: int
    games_remaining: int
    cost_paid: int
    created_at: int
    updated_at: int


@dataclass
class PackageDealPurchase:
    """An immutable record of a single package-deal purchase.

    Unlike ``PackageDeal`` (which is mutated/deleted as games are consumed),
    this is append-only and survives consumption, so year-in-review stats can
    count every deal a player bought during a period.
    """
    guild_id: int
    buyer_discord_id: int
    partner_discord_id: int
    jc_spent: int
    games_committed: int
    created_at: int


class PackageDealRepository(BaseRepository, IPackageDealRepository):
    """Repository for package deal feature."""

    def create_or_extend_deal(
        self,
        guild_id: int | None,
        buyer_id: int,
        partner_id: int,
        games: int = 10,
        cost: int = 0,
    ) -> PackageDeal:
        """
        Create a new package deal or extend existing one.

        If a deal already exists for this pair, adds games to games_remaining.
        Returns the created/updated deal.

        Raises:
            ValueError: If buyer_id equals partner_id (cannot package deal with oneself)
        """
        if buyer_id == partner_id:
            raise ValueError("Cannot create package deal: buyer and partner cannot be the same player")

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())

        with self.connection() as conn:
            cursor = conn.cursor()

            # Use UPSERT for atomic create-or-extend operation
            # ON CONFLICT updates games_remaining by adding the new games
            cursor.execute(
                """
                INSERT INTO package_deals
                    (guild_id, buyer_discord_id, partner_discord_id, games_remaining, cost_paid, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(guild_id, buyer_discord_id, partner_discord_id)
                DO UPDATE SET
                    games_remaining = games_remaining + excluded.games_remaining,
                    cost_paid = cost_paid + excluded.cost_paid,
                    updated_at = excluded.updated_at
                """,
                (normalized_guild, buyer_id, partner_id, games, cost, now, now),
            )

            # Append an immutable purchase-log row in the SAME transaction so the
            # year-in-review stats survive consumption/deletion of the active deal.
            cursor.execute(
                """
                INSERT INTO package_deal_purchases
                    (guild_id, buyer_discord_id, partner_discord_id, jc_spent, games_committed, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (normalized_guild, buyer_id, partner_id, cost, games, now),
            )

            # Fetch the resulting row to return complete deal data
            cursor.execute(
                """
                SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                       games_remaining, cost_paid, created_at, updated_at
                FROM package_deals
                WHERE guild_id = ? AND buyer_discord_id = ? AND partner_discord_id = ?
                """,
                (normalized_guild, buyer_id, partner_id),
            )
            row = cursor.fetchone()

            if row is None:
                raise RuntimeError(
                    f"UPSERT succeeded but row not found for package deal "
                    f"({buyer_id} -> {partner_id}, guild={normalized_guild})"
                )

            return PackageDeal(
                id=row["id"],
                guild_id=row["guild_id"],
                buyer_discord_id=row["buyer_discord_id"],
                partner_discord_id=row["partner_discord_id"],
                games_remaining=row["games_remaining"],
                cost_paid=row["cost_paid"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
            )

    def get_active_deals_for_players(
        self,
        guild_id: int | None,
        player_ids: list[int],
    ) -> list[PackageDeal]:
        """
        Get all active deals where BOTH buyer and partner are in player_ids.

        This is used during shuffle to find deals relevant to the current match.
        Only returns deals with games_remaining > 0.
        """
        if not player_ids:
            return []

        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(player_ids))

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                       games_remaining, cost_paid, created_at, updated_at
                FROM package_deals
                WHERE guild_id = ?
                  AND games_remaining > 0
                  AND buyer_discord_id IN ({placeholders})
                  AND partner_discord_id IN ({placeholders})
                """,
                (normalized_guild, *player_ids, *player_ids),
            )
            rows = cursor.fetchall()

        return [
            PackageDeal(
                id=row["id"],
                guild_id=row["guild_id"],
                buyer_discord_id=row["buyer_discord_id"],
                partner_discord_id=row["partner_discord_id"],
                games_remaining=row["games_remaining"],
                cost_paid=row["cost_paid"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
            )
            for row in rows
        ]

    def get_deals_involving_player(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> list[PackageDeal]:
        """
        Get all active deals where player is buyer OR partner.

        Used for wrapped to show all deals involving a player.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                       games_remaining, cost_paid, created_at, updated_at
                FROM package_deals
                WHERE guild_id = ?
                  AND (buyer_discord_id = ? OR partner_discord_id = ?)
                  AND games_remaining > 0
                ORDER BY created_at DESC
                """,
                (normalized_guild, discord_id, discord_id),
            )
            rows = cursor.fetchall()

        return [
            PackageDeal(
                id=row["id"],
                guild_id=row["guild_id"],
                buyer_discord_id=row["buyer_discord_id"],
                partner_discord_id=row["partner_discord_id"],
                games_remaining=row["games_remaining"],
                cost_paid=row["cost_paid"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
            )
            for row in rows
        ]

    def get_purchases_involving_player(
        self,
        guild_id: int | None,
        discord_id: int,
        start_ts: int,
        end_ts: int,
    ) -> list[PackageDealPurchase]:
        """
        Get all package-deal purchases where the player is buyer OR partner,
        created within [start_ts, end_ts).

        Reads the immutable purchase log (not ``package_deals``), so consumed and
        deleted deals are still counted. Used for year-in-review stats.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT guild_id, buyer_discord_id, partner_discord_id,
                       jc_spent, games_committed, created_at
                FROM package_deal_purchases
                WHERE guild_id = ?
                  AND (buyer_discord_id = ? OR partner_discord_id = ?)
                  AND created_at >= ?
                  AND created_at < ?
                ORDER BY created_at DESC
                """,
                (normalized_guild, discord_id, discord_id, start_ts, end_ts),
            )
            rows = cursor.fetchall()

        return [
            PackageDealPurchase(
                guild_id=row["guild_id"],
                buyer_discord_id=row["buyer_discord_id"],
                partner_discord_id=row["partner_discord_id"],
                jc_spent=row["jc_spent"],
                games_committed=row["games_committed"],
                created_at=row["created_at"],
            )
            for row in rows
        ]

    def get_user_deals(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> list[PackageDeal]:
        """
        Get all active deals created by a user.

        Used for /shop deals.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                       games_remaining, cost_paid, created_at, updated_at
                FROM package_deals
                WHERE guild_id = ? AND buyer_discord_id = ? AND games_remaining > 0
                ORDER BY created_at DESC
                """,
                (normalized_guild, discord_id),
            )
            rows = cursor.fetchall()

        return [
            PackageDeal(
                id=row["id"],
                guild_id=row["guild_id"],
                buyer_discord_id=row["buyer_discord_id"],
                partner_discord_id=row["partner_discord_id"],
                games_remaining=row["games_remaining"],
                cost_paid=row["cost_paid"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
            )
            for row in rows
        ]

    def decrement_deals(
        self,
        guild_id: int | None,
        deal_ids: list[int],
    ) -> int:
        """
        Decrement games_remaining for the given deal IDs.

        Deals that reach 0 are kept but will be filtered out in future queries.
        Returns the number of deals decremented.
        """
        if not deal_ids:
            return 0

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())
        placeholders = ",".join("?" * len(deal_ids))

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                UPDATE package_deals
                SET games_remaining = games_remaining - 1, updated_at = ?
                WHERE guild_id = ? AND id IN ({placeholders}) AND games_remaining > 0
                """,
                (now, normalized_guild, *deal_ids),
            )
            return cursor.rowcount

    def delete_expired_deals(self, guild_id: int | None) -> int:
        """
        Delete deals with games_remaining = 0.

        This is optional cleanup - expired deals are ignored anyway.
        Returns the number of deals deleted.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                DELETE FROM package_deals
                WHERE guild_id = ? AND games_remaining <= 0
                """,
                (normalized_guild,),
            )
            return cursor.rowcount
