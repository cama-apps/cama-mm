"""
Repository for package deal data access.
"""

import logging
import time
from dataclasses import dataclass
from typing import Literal

from domain.package_deal_constants import PACKAGE_DEAL_MAX_GAMES
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
    games_added: int = 0


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


@dataclass(frozen=True)
class PackageDealPurchaseResult:
    """Outcome of an atomic paid package-deal purchase attempt."""

    success: bool
    reason: Literal["already_full", "insufficient_balance"] | None
    balance: int
    deal: PackageDeal | None
    cost: int
    active_deal_count: int


class PackageDealRepository(BaseRepository, IPackageDealRepository):
    """Repository for package deal feature."""

    @staticmethod
    def _calculate_top_up(existing, games: int) -> tuple[int, int]:
        current_games = (
            min(PACKAGE_DEAL_MAX_GAMES, max(0, existing["games_remaining"]))
            if existing
            else 0
        )
        games_added = min(games, PACKAGE_DEAL_MAX_GAMES - current_games)
        return games_added, current_games + games_added

    def _write_deal_top_up(
        self,
        cursor,
        *,
        normalized_guild: int,
        buyer_id: int,
        partner_id: int,
        games_remaining: int,
        games_added: int,
        cost: int,
        now: int,
    ) -> PackageDeal:
        cursor.execute(
            """
            INSERT INTO package_deals
                (guild_id, buyer_discord_id, partner_discord_id, games_remaining, cost_paid, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(guild_id, buyer_discord_id, partner_discord_id)
            DO UPDATE SET
                games_remaining = excluded.games_remaining,
                cost_paid = cost_paid + excluded.cost_paid,
                updated_at = excluded.updated_at
            """,
            (normalized_guild, buyer_id, partner_id, games_remaining, cost, now, now),
        )

        cursor.execute(
            """
            INSERT INTO package_deal_purchases
                (guild_id, buyer_discord_id, partner_discord_id, jc_spent, games_committed, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            """,
            (normalized_guild, buyer_id, partner_id, cost, games_added, now),
        )

        row = cursor.execute(
            """
            SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                   games_remaining, cost_paid, created_at, updated_at
            FROM package_deals
            WHERE guild_id = ? AND buyer_discord_id = ? AND partner_discord_id = ?
            """,
            (normalized_guild, buyer_id, partner_id),
        ).fetchone()
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
            games_added=games_added,
        )

    def create_or_extend_deal(
        self,
        guild_id: int | None,
        buyer_id: int,
        partner_id: int,
        games: int = 10,
        cost: int = 0,
    ) -> PackageDeal:
        """
        Create a new package deal or top up an existing one.

        Deals can never have more than 10 games remaining.
        Returns the created/updated deal.

        Raises:
            ValueError: If buyer_id equals partner_id (cannot package deal with oneself)
        """
        if buyer_id == partner_id:
            raise ValueError("Cannot create package deal: buyer and partner cannot be the same player")
        if games < 1:
            raise ValueError("Package deal duration must be at least 1 game")

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                       games_remaining, cost_paid, created_at, updated_at
                FROM package_deals
                WHERE guild_id = ? AND buyer_discord_id = ? AND partner_discord_id = ?
                """,
                (normalized_guild, buyer_id, partner_id),
            )
            existing = cursor.fetchone()
            games_added, games_remaining = self._calculate_top_up(existing, games)
            if games_added == 0 and existing:
                return PackageDeal(
                    **dict(existing),
                    games_added=0,
                )

            return self._write_deal_top_up(
                cursor,
                normalized_guild=normalized_guild,
                buyer_id=buyer_id,
                partner_id=partner_id,
                games_remaining=games_remaining,
                games_added=games_added,
                cost=cost,
                now=now,
            )

    def purchase_deal(
        self,
        guild_id: int | None,
        buyer_id: int,
        partner_id: int,
        *,
        games: int = 10,
        no_active_cost: int,
        active_cost: int,
    ) -> PackageDealPurchaseResult:
        """Atomically price, debit, and top up a paid package deal when allowed."""
        if buyer_id == partner_id:
            raise ValueError("Cannot create package deal: buyer and partner cannot be the same player")
        if games < 1:
            raise ValueError("Package deal duration must be at least 1 game")
        if no_active_cost < 0 or active_cost < 0:
            raise ValueError("Package deal costs cannot be negative")

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            player_row = cursor.execute(
                """
                SELECT COALESCE(jopacoin_balance, 0) AS balance
                FROM players
                WHERE discord_id = ? AND guild_id = ?
                """,
                (buyer_id, normalized_guild),
            ).fetchone()
            if player_row is None:
                raise RuntimeError(
                    f"Registered player row not found for package deal buyer {buyer_id} "
                    f"in guild {normalized_guild}"
                )
            balance = int(player_row["balance"])

            active_deal_count = int(
                cursor.execute(
                    """
                    SELECT COUNT(*)
                    FROM package_deals
                    WHERE guild_id = ? AND buyer_discord_id = ? AND games_remaining > 0
                    """,
                    (normalized_guild, buyer_id),
                ).fetchone()[0]
            )
            cost = no_active_cost if active_deal_count == 0 else active_cost

            existing = cursor.execute(
                """
                SELECT id, guild_id, buyer_discord_id, partner_discord_id,
                       games_remaining, cost_paid, created_at, updated_at
                FROM package_deals
                WHERE guild_id = ? AND buyer_discord_id = ? AND partner_discord_id = ?
                """,
                (normalized_guild, buyer_id, partner_id),
            ).fetchone()
            games_added, games_remaining = self._calculate_top_up(existing, games)
            if games_added == 0 and existing:
                return PackageDealPurchaseResult(
                    success=False,
                    reason="already_full",
                    balance=balance,
                    deal=PackageDeal(**dict(existing), games_added=0),
                    cost=cost,
                    active_deal_count=active_deal_count,
                )
            if balance < cost:
                return PackageDealPurchaseResult(
                    success=False,
                    reason="insufficient_balance",
                    balance=balance,
                    deal=None,
                    cost=cost,
                    active_deal_count=active_deal_count,
                )

            self._set_economy_ledger_context(
                cursor,
                source="package_deal",
                actor_id=buyer_id,
                related_type="player",
                related_id=partner_id,
                reason="package deal purchase",
                metadata={"cost": cost, "games": games, "games_added": games_added},
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                      AND COALESCE(jopacoin_balance, 0) >= ?
                    """,
                    (cost, buyer_id, normalized_guild, cost),
                )
                if cursor.rowcount == 0:
                    raise RuntimeError("Package deal debit failed after balance validation")
            finally:
                self._clear_economy_ledger_context(cursor)

            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = jopacoin_balance
                WHERE discord_id = ? AND guild_id = ?
                  AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                """,
                (buyer_id, normalized_guild),
            )

            deal = self._write_deal_top_up(
                cursor,
                normalized_guild=normalized_guild,
                buyer_id=buyer_id,
                partner_id=partner_id,
                games_remaining=games_remaining,
                games_added=games_added,
                cost=cost,
                now=now,
            )
            return PackageDealPurchaseResult(
                success=True,
                reason=None,
                balance=balance - cost,
                deal=deal,
                cost=cost,
                active_deal_count=active_deal_count,
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
