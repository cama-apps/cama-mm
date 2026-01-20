"""
Service for managing the spectator betting pool.

Spectators (non-participants) can bet on match outcomes. Payouts are:
- 90% to winning bettors (parimutuel)
- 10% to winning team players (split evenly among 5)
"""

from __future__ import annotations

import logging
from dataclasses import dataclass

import config
from repositories.interfaces import IPlayerRepository, ISpectatorBetRepository

logger = logging.getLogger("cama_bot.spectator_pool")


@dataclass
class SpectatorPoolConfig:
    """Configuration for the spectator betting pool."""

    enabled: bool = True
    player_cut: float = 0.10  # 10% to winning players


class SpectatorPoolService:
    """
    Manages the spectator betting pool for match outcomes.

    Spectators (non-match-participants) can bet on teams. Payouts use
    parimutuel odds with a portion going to winning team players.
    """

    def __init__(
        self,
        spectator_bet_repo: ISpectatorBetRepository,
        player_repo: IPlayerRepository,
        pool_config: SpectatorPoolConfig | None = None,
    ):
        self.spectator_bet_repo = spectator_bet_repo
        self.player_repo = player_repo
        self.config = pool_config or SpectatorPoolConfig(
            enabled=True,
            player_cut=config.SPECTATOR_POOL_PLAYER_CUT,
        )

    def place_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        pending_state: dict,
    ) -> dict:
        """
        Place a spectator bet on a team.

        Args:
            guild_id: Guild ID
            discord_id: Bettor's Discord ID
            team: 'radiant' or 'dire'
            amount: Amount to bet
            pending_state: Current pending match state

        Returns:
            Dict with bet confirmation or error
        """
        if not self.config.enabled:
            return {"success": False, "error": "Spectator pool is disabled"}

        # Validate team
        team = team.lower()
        if team not in {"radiant", "dire"}:
            return {"success": False, "error": f"Invalid team: {team}"}

        # Check if betting window is still open
        shuffle_ts = pending_state.get("shuffle_timestamp")
        if not shuffle_ts:
            return {"success": False, "error": "No active match to bet on"}

        # Check if user already has a bet
        existing_bet = self.spectator_bet_repo.get_player_pending_bet(
            guild_id, discord_id, shuffle_ts
        )
        if existing_bet:
            return {
                "success": False,
                "error": f"You already have a bet on {existing_bet['team']}",
            }

        # Check balance
        balance = self.player_repo.get_balance(discord_id)
        if balance < amount:
            return {
                "success": False,
                "error": f"Insufficient balance: have {balance} JC, need {amount} JC",
            }

        try:
            import time
            bet_time = int(time.time())
            bet_id = self.spectator_bet_repo.create_bet(
                guild_id=guild_id,
                discord_id=discord_id,
                team=team,
                amount=amount,
                bet_time=bet_time,
            )

            # Get updated pool totals
            pool_totals = self.spectator_bet_repo.get_pool_totals(guild_id, shuffle_ts)

            return {
                "success": True,
                "bet_id": bet_id,
                "team": team,
                "amount": amount,
                "pool_totals": pool_totals,
            }
        except ValueError as e:
            return {"success": False, "error": str(e)}

    def settle_bets(
        self,
        match_id: int,
        guild_id: int | None,
        winning_team: str,
        winning_player_ids: list[int],
        pending_state: dict,
    ) -> dict:
        """
        Settle spectator bets for a completed match.

        Args:
            match_id: The recorded match ID
            guild_id: Guild ID
            winning_team: 'radiant' or 'dire'
            winning_player_ids: Discord IDs of winning team players
            pending_state: Match state with shuffle timestamp

        Returns:
            Dict with settlement summary
        """
        if not self.config.enabled:
            return {"enabled": False, "total_payout": 0, "player_bonus": 0}

        shuffle_ts = pending_state.get("shuffle_timestamp")
        if not shuffle_ts:
            return {"enabled": False, "error": "No shuffle timestamp"}

        # Get pool totals
        pool_totals = self.spectator_bet_repo.get_pool_totals(guild_id, shuffle_ts)
        total_pool = pool_totals["total"]

        if total_pool == 0:
            return {
                "enabled": True,
                "total_pool": 0,
                "total_payout": 0,
                "player_bonus": 0,
                "player_bonus_each": 0,
                "winners": [],
                "losers": [],
            }

        # Calculate payout portions
        bettor_share = total_pool * (1 - self.config.player_cut)  # 90%
        player_bonus = int(total_pool * self.config.player_cut)  # 10%

        winning_side_total = pool_totals[winning_team]

        # Calculate bettor payout multiplier
        if winning_side_total > 0:
            # Multiplier for bettor share only
            payout_multiplier = bettor_share / winning_side_total
        else:
            # No bets on winning side - all money goes to players
            payout_multiplier = 0
            player_bonus = total_pool  # All goes to players

        # Settle bets with calculated multiplier
        settlement = self.spectator_bet_repo.settle_bets_atomic(
            match_id=match_id,
            guild_id=guild_id,
            since_ts=shuffle_ts,
            winning_team=winning_team,
            payout_multiplier=payout_multiplier,
        )

        # Distribute player bonus to winning team
        player_bonus_each = 0
        if player_bonus > 0 and winning_player_ids:
            player_bonus_each = player_bonus // len(winning_player_ids)
            for player_id in winning_player_ids:
                self.player_repo.add_balance(player_id, player_bonus_each)

        return {
            "enabled": True,
            "total_pool": total_pool,
            "bettor_share": int(bettor_share),
            "player_bonus": player_bonus,
            "player_bonus_each": player_bonus_each,
            "winning_team": winning_team,
            "payout_multiplier": payout_multiplier,
            "winners": settlement["winners"],
            "losers": settlement["losers"],
            "total_payout": settlement["total_payout"],
        }

    def refund_bets(
        self,
        guild_id: int | None,
        pending_state: dict,
    ) -> dict:
        """
        Refund all pending spectator bets (for abort/restart).

        Args:
            guild_id: Guild ID
            pending_state: Match state with shuffle timestamp

        Returns:
            Dict with refund summary
        """
        if not self.config.enabled:
            return {"enabled": False, "refunded": 0}

        shuffle_ts = pending_state.get("shuffle_timestamp")
        if not shuffle_ts:
            return {"enabled": False, "error": "No shuffle timestamp"}

        result = self.spectator_bet_repo.refund_bets_atomic(guild_id, shuffle_ts)
        return {
            "enabled": True,
            "refunded": result["refunded"],
            "total_amount": result["total_amount"],
        }

    def get_pool_info(
        self,
        guild_id: int | None,
        pending_state: dict,
    ) -> dict:
        """
        Get current spectator pool information for display.

        Args:
            guild_id: Guild ID
            pending_state: Match state with shuffle timestamp

        Returns:
            Dict with pool info for embed display
        """
        if not self.config.enabled:
            return {"enabled": False}

        shuffle_ts = pending_state.get("shuffle_timestamp")
        if not shuffle_ts:
            return {"enabled": False}

        pool_totals = self.spectator_bet_repo.get_pool_totals(guild_id, shuffle_ts)

        total = pool_totals["total"]
        radiant = pool_totals["radiant"]
        dire = pool_totals["dire"]

        # Calculate current odds (accounting for player cut)
        bettor_share = 1 - self.config.player_cut  # 0.90

        if total > 0 and radiant > 0:
            radiant_multiplier = (total * bettor_share) / radiant
        else:
            radiant_multiplier = 0

        if total > 0 and dire > 0:
            dire_multiplier = (total * bettor_share) / dire
        else:
            dire_multiplier = 0

        return {
            "enabled": True,
            "radiant_total": radiant,
            "dire_total": dire,
            "total_pool": total,
            "radiant_multiplier": round(radiant_multiplier, 2),
            "dire_multiplier": round(dire_multiplier, 2),
            "player_cut_pct": int(self.config.player_cut * 100),
        }

    def get_player_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        pending_state: dict,
    ) -> dict | None:
        """
        Get a player's current spectator bet.

        Args:
            guild_id: Guild ID
            discord_id: Player's Discord ID
            pending_state: Match state

        Returns:
            Bet dict or None
        """
        shuffle_ts = pending_state.get("shuffle_timestamp")
        if not shuffle_ts:
            return None

        return self.spectator_bet_repo.get_player_pending_bet(
            guild_id, discord_id, shuffle_ts
        )

    def get_player_stats(self, discord_id: int) -> dict:
        """
        Get spectator betting statistics for a player.

        Args:
            discord_id: Player's Discord ID

        Returns:
            Dict with betting stats
        """
        return self.spectator_bet_repo.get_player_bet_stats(discord_id)

    def format_pool_display(self, pool_info: dict) -> str:
        """
        Format pool info for Discord embed display.

        Args:
            pool_info: Output from get_pool_info()

        Returns:
            Formatted string for embed
        """
        if not pool_info.get("enabled"):
            return "Spectator pool disabled"

        total = pool_info.get("total_pool", 0)
        if total == 0:
            return (
                "No bets yet\n"
                f"Winners get {100 - pool_info.get('player_cut_pct', 10)}%, "
                f"players get {pool_info.get('player_cut_pct', 10)}%"
            )

        radiant = pool_info.get("radiant_total", 0)
        dire = pool_info.get("dire_total", 0)
        radiant_mult = pool_info.get("radiant_multiplier", 0)
        dire_mult = pool_info.get("dire_multiplier", 0)

        lines = [
            f"Radiant: {radiant} JC ({radiant_mult:.2f}x)",
            f"Dire: {dire} JC ({dire_mult:.2f}x)",
            f"Total: {total} JC",
        ]
        return "\n".join(lines)
