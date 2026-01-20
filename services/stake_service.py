"""
Service for managing player stake pool in draft mode.

The Player Stake Pool provides:
1. Glicko-weighted auto-liquidity (50 JC total, distributed based on odds)
2. Optional player bets on their own team (real JC)
3. Parimutuel payouts from combined pool (auto + player bets)
4. Excluded players get minted payouts based on Glicko odds (separate from pool)

Key features:
- Auto-liquidity is distributed to create fair starting odds
- Players can add real JC to increase their team's payout odds
- Excluded players always get 5 JC × (1/win_prob) minted out of thin air
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field

import config
from repositories.interfaces import IPlayerPoolBetRepository, IPlayerRepository, IStakeRepository

logger = logging.getLogger("cama_bot.services.stake")


@dataclass
class StakePoolConfig:
    """Configuration for the player stake pool."""

    pool_size: int = 50  # Total auto-liquidity pool (5 JC per drafted player)
    stake_per_player: int = 5  # Auto-liquidity per drafted player
    excluded_payout: int = 5  # Base payout for excluded players
    enabled: bool = True
    win_prob_min: float = 0.10  # Clamp to prevent extreme odds
    win_prob_max: float = 0.90


@dataclass
class PoolState:
    """State of the player stake pool."""

    radiant_auto: float = 0  # Auto-liquidity on radiant side
    dire_auto: float = 0  # Auto-liquidity on dire side
    radiant_bets: int = 0  # Player bets on radiant
    dire_bets: int = 0  # Player bets on dire
    radiant_win_prob: float = 0.5

    @property
    def radiant_total(self) -> float:
        return self.radiant_auto + self.radiant_bets

    @property
    def dire_total(self) -> float:
        return self.dire_auto + self.dire_bets

    @property
    def total_pool(self) -> float:
        return self.radiant_total + self.dire_total

    def get_multiplier(self, team: str) -> float:
        """Get parimutuel multiplier for a team."""
        team_total = self.radiant_total if team == "radiant" else self.dire_total
        if team_total <= 0:
            return 0
        return self.total_pool / team_total


class StakeService:
    """
    Manages the player stake pool for draft mode.

    The stake pool is a dual-pool system that:
    1. Creates Glicko-weighted auto-liquidity when a draft completes
    2. Allows players to optionally bet on their own team
    3. Uses parimutuel payouts from combined pool
    4. Pays excluded players separately (minted, not from pool)
    """

    def __init__(
        self,
        stake_repo: IStakeRepository,
        player_repo: IPlayerRepository,
        player_pool_bet_repo: IPlayerPoolBetRepository | None = None,
        pool_config: StakePoolConfig | None = None,
    ):
        """
        Initialize the stake service.

        Args:
            stake_repo: Repository for stake tracking (drafted players)
            player_repo: Repository for player data
            player_pool_bet_repo: Repository for player pool bets
            pool_config: Optional stake pool configuration
        """
        self.stake_repo = stake_repo
        self.player_repo = player_repo
        self.player_pool_bet_repo = player_pool_bet_repo
        self.config = pool_config or StakePoolConfig(
            pool_size=config.PLAYER_STAKE_POOL_SIZE,
            stake_per_player=config.PLAYER_STAKE_PER_PLAYER,
            enabled=config.PLAYER_STAKE_ENABLED,
            win_prob_min=config.STAKE_WIN_PROB_MIN,
            win_prob_max=config.STAKE_WIN_PROB_MAX,
        )

    def calculate_auto_liquidity(self, radiant_win_prob: float) -> tuple[float, float]:
        """
        Calculate Glicko-weighted auto-liquidity distribution.

        More auto-liquidity goes to the underdog side to create fair starting odds.

        Formula:
            radiant_auto = total_auto × dire_win_prob
            dire_auto = total_auto × radiant_win_prob

        Example (Radiant 55% / Dire 45%):
            radiant_auto = 50 × 0.45 = 22.5 JC
            dire_auto = 50 × 0.55 = 27.5 JC
            Starting odds: Radiant bettor gets 50/22.5 = 2.22x

        Args:
            radiant_win_prob: Probability radiant wins (0.0-1.0)

        Returns:
            Tuple of (radiant_auto, dire_auto)
        """
        # Clamp probability
        clamped_prob = max(
            self.config.win_prob_min,
            min(self.config.win_prob_max, radiant_win_prob)
        )

        total_auto = self.config.pool_size
        dire_win_prob = 1.0 - clamped_prob

        # More liquidity on the underdog side
        radiant_auto = total_auto * dire_win_prob
        dire_auto = total_auto * clamped_prob

        return radiant_auto, dire_auto

    def create_stakes_for_draft(
        self,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        excluded_ids: list[int],
        radiant_win_prob: float,
        stake_time: int,
    ) -> dict:
        """
        Create stake entries when a draft completes.

        This creates tracking entries for all participants and calculates
        the initial pool state with auto-liquidity.

        Args:
            guild_id: Guild ID
            radiant_ids: Discord IDs of radiant team (5 players)
            dire_ids: Discord IDs of dire team (5 players)
            excluded_ids: Discord IDs of excluded players (0-4)
            radiant_win_prob: Probability radiant wins (0.0-1.0)
            stake_time: Unix timestamp for stake creation

        Returns:
            Dict with creation info including pool state
        """
        if not self.config.enabled:
            return {"enabled": False, "created": 0}

        # Create stake entries for tracking
        result = self.stake_repo.create_stakes(
            guild_id=guild_id,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=excluded_ids,
            stake_time=stake_time,
        )

        # Calculate auto-liquidity distribution
        radiant_auto, dire_auto = self.calculate_auto_liquidity(radiant_win_prob)

        # Calculate initial multipliers (no player bets yet)
        initial_radiant_mult = self.config.pool_size / radiant_auto if radiant_auto > 0 else 0
        initial_dire_mult = self.config.pool_size / dire_auto if dire_auto > 0 else 0

        # Calculate excluded player payout (minted based on odds)
        excluded_payout_radiant = self.calculate_excluded_payout(radiant_win_prob, "radiant")
        excluded_payout_dire = self.calculate_excluded_payout(radiant_win_prob, "dire")

        result.update({
            "enabled": True,
            "radiant_win_prob": radiant_win_prob,
            "dire_win_prob": 1.0 - radiant_win_prob,
            "radiant_auto": radiant_auto,
            "dire_auto": dire_auto,
            "pool_size": self.config.pool_size,
            "stake_per_player": self.config.stake_per_player,
            "initial_radiant_multiplier": round(initial_radiant_mult, 2),
            "initial_dire_multiplier": round(initial_dire_mult, 2),
            "excluded_payout_if_radiant_wins": excluded_payout_radiant,
            "excluded_payout_if_dire_wins": excluded_payout_dire,
        })

        logger.info(
            f"Created stakes for draft: guild={guild_id}, "
            f"radiant={len(radiant_ids)}, dire={len(dire_ids)}, "
            f"excluded={len(excluded_ids)}, radiant_win_prob={radiant_win_prob:.2f}, "
            f"auto_liquidity: radiant={radiant_auto:.1f}, dire={dire_auto:.1f}"
        )

        return result

    def calculate_excluded_payout(self, radiant_win_prob: float, winning_team: str) -> int:
        """
        Calculate payout for excluded players (minted out of thin air).

        Formula: excluded_payout = stake_per_player / win_probability

        Example (Radiant 55% wins):
            payout = 5 / 0.55 = ~9 JC

        Example (Dire 45% wins):
            payout = 5 / 0.45 = ~11 JC

        Args:
            radiant_win_prob: Probability radiant wins (0.0-1.0)
            winning_team: 'radiant' or 'dire'

        Returns:
            Integer payout amount per excluded player
        """
        clamped_prob = max(
            self.config.win_prob_min,
            min(self.config.win_prob_max, radiant_win_prob)
        )

        win_prob = clamped_prob if winning_team == "radiant" else (1.0 - clamped_prob)
        payout = self.config.stake_per_player / win_prob

        return int(round(payout))

    def place_player_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        pending_state: dict,
    ) -> dict:
        """
        Place a player bet on their own team (real JC).

        Players can only bet on the team they're on. This adds to the
        pool and shifts the parimutuel odds.

        Args:
            guild_id: Guild ID
            discord_id: Player's Discord ID
            team: 'radiant' or 'dire' (must match player's team)
            amount: Amount to bet
            pending_state: Current pending match state

        Returns:
            Dict with bet confirmation or error
        """
        if not self.config.enabled:
            return {"success": False, "error": "Player pool is disabled"}

        if self.player_pool_bet_repo is None:
            return {"success": False, "error": "Player pool bets not configured"}

        # Validate team
        team = team.lower()
        if team not in {"radiant", "dire"}:
            return {"success": False, "error": f"Invalid team: {team}"}

        shuffle_ts = pending_state.get("shuffle_timestamp")
        if not shuffle_ts:
            return {"success": False, "error": "No active match to bet on"}

        # Check if user already has a bet
        existing_bet = self.player_pool_bet_repo.get_player_pending_bet(
            guild_id, discord_id, shuffle_ts
        )
        if existing_bet:
            return {
                "success": False,
                "error": f"You already have a bet of {existing_bet['amount']} JC on {existing_bet['team']}",
            }

        try:
            bet_time = int(time.time())
            result = self.player_pool_bet_repo.create_bet_atomic(
                guild_id=guild_id,
                discord_id=discord_id,
                team=team,
                amount=amount,
                bet_time=bet_time,
            )

            # Get updated pool state
            pool_state = self.get_pool_state(guild_id, pending_state)

            return {
                "success": True,
                "bet_id": result["bet_id"],
                "team": team,
                "amount": amount,
                "new_balance": result["new_balance"],
                "new_multiplier": pool_state.get_multiplier(team),
            }
        except ValueError as e:
            return {"success": False, "error": str(e)}

    def get_pool_state(self, guild_id: int | None, pending_state: dict) -> PoolState:
        """
        Get the current state of the player stake pool.

        Args:
            guild_id: Guild ID
            pending_state: Current pending match state

        Returns:
            PoolState with auto-liquidity and player bets
        """
        radiant_win_prob = pending_state.get("stake_radiant_win_prob", 0.5)
        radiant_auto, dire_auto = self.calculate_auto_liquidity(radiant_win_prob)

        radiant_bets = 0
        dire_bets = 0

        shuffle_ts = pending_state.get("shuffle_timestamp")
        if shuffle_ts and self.player_pool_bet_repo:
            pool_totals = self.player_pool_bet_repo.get_pool_totals(guild_id, shuffle_ts)
            radiant_bets = pool_totals.get("radiant", 0)
            dire_bets = pool_totals.get("dire", 0)

        return PoolState(
            radiant_auto=radiant_auto,
            dire_auto=dire_auto,
            radiant_bets=radiant_bets,
            dire_bets=dire_bets,
            radiant_win_prob=radiant_win_prob,
        )

    def settle_stakes(
        self,
        match_id: int,
        guild_id: int | None,
        winning_team: str,
        pending_state: dict,
    ) -> dict:
        """
        Settle stakes when a match is recorded.

        1. Calculate parimutuel payout for player bets from combined pool
        2. Pay excluded players with minted JC based on Glicko odds

        Args:
            match_id: Recorded match ID
            guild_id: Guild ID
            winning_team: 'radiant' or 'dire'
            pending_state: Pending match state with stake info

        Returns:
            Dict with settlement summary
        """
        if not self.config.enabled:
            return {"enabled": False, "settled": 0}

        radiant_win_prob = pending_state.get("stake_radiant_win_prob")
        if radiant_win_prob is None:
            logger.warning(
                f"No stake_radiant_win_prob in pending state for guild {guild_id}"
            )
            return {"enabled": True, "settled": 0, "error": "no_probability"}

        stake_time = pending_state.get("shuffle_timestamp")
        if stake_time is None:
            logger.warning(f"No shuffle_timestamp in pending state for guild {guild_id}")
            return {"enabled": True, "settled": 0, "error": "no_timestamp"}

        # Get pool state
        pool_state = self.get_pool_state(guild_id, pending_state)

        # Settle player pool bets (parimutuel from combined pool)
        player_bet_result = {"winners": [], "losers": [], "total_payout": 0}
        if self.player_pool_bet_repo:
            player_bet_result = self.player_pool_bet_repo.settle_bets_atomic(
                match_id=match_id,
                guild_id=guild_id,
                since_ts=stake_time,
                winning_team=winning_team,
                radiant_total=pool_state.radiant_total,
                dire_total=pool_state.dire_total,
            )

        # Calculate odds-based payout for all drafted players (participants + excluded)
        # Formula: stake_per_player / win_probability
        # Underdogs get more reward for winning against the odds
        odds_based_payout = self.calculate_excluded_payout(radiant_win_prob, winning_team)
        payout_per_participant = odds_based_payout
        payout_per_excluded = odds_based_payout

        # Get multiplier for display/logging (only relevant when player bets exist)
        multiplier = pool_state.get_multiplier(winning_team)

        # Settle stakes with differentiated payouts
        stake_result = self.stake_repo.settle_stakes_atomic(
            match_id=match_id,
            guild_id=guild_id,
            since_ts=stake_time,
            winning_team=winning_team,
            payout_per_participant=payout_per_participant,
            payout_per_excluded=payout_per_excluded,
        )

        # Separate participant and excluded winners for reporting
        participant_winners = [
            w for w in stake_result.get("winners", [])
            if w.get("is_excluded") != 1
        ]
        excluded_winners = [
            w for w in stake_result.get("winners", [])
            if w.get("is_excluded") == 1
        ]
        participant_total = len(participant_winners) * payout_per_participant
        excluded_total = len(excluded_winners) * payout_per_excluded

        result = {
            "enabled": True,
            "winning_team": winning_team,
            "pool_state": {
                "radiant_auto": pool_state.radiant_auto,
                "dire_auto": pool_state.dire_auto,
                "radiant_bets": pool_state.radiant_bets,
                "dire_bets": pool_state.dire_bets,
                "total_pool": pool_state.total_pool,
                "multiplier": multiplier,
            },
            "participants": {
                "payout_per_player": payout_per_participant,
                "winners": participant_winners,
                "total_payout": participant_total,
            },
            "player_bets": {
                "winners": player_bet_result.get("winners", []),
                "losers": player_bet_result.get("losers", []),
                "total_payout": player_bet_result.get("total_payout", 0),
                "multiplier": multiplier,
            },
            "excluded": {
                "payout_per_player": payout_per_excluded,
                "winners": excluded_winners,
                "total_payout": excluded_total,
            },
            "total_minted": participant_total + excluded_total,
        }

        logger.info(
            f"Settled stakes: match={match_id}, guild={guild_id}, "
            f"winning_team={winning_team}, multiplier={multiplier:.2f}, "
            f"participant_payout={participant_total}, "
            f"player_bets_payout={player_bet_result.get('total_payout', 0)}, "
            f"excluded_payout={excluded_total}"
        )

        return result

    def clear_stakes(
        self,
        guild_id: int | None,
        pending_state: dict,
    ) -> dict:
        """
        Clear pending stakes and refund player bets (for draft abort/restart).

        Args:
            guild_id: Guild ID
            pending_state: Pending match state with timestamp

        Returns:
            Dict with deletion/refund counts
        """
        if not self.config.enabled:
            return {"enabled": False, "deleted": 0}

        stake_time = pending_state.get("shuffle_timestamp")
        if stake_time is None:
            return {"enabled": True, "deleted": 0, "error": "no_timestamp"}

        # Delete stake entries
        deleted = self.stake_repo.delete_stakes(guild_id, stake_time)

        # Refund player pool bets
        refunded = 0
        refund_amount = 0
        if self.player_pool_bet_repo:
            refund_result = self.player_pool_bet_repo.refund_bets_atomic(
                guild_id, stake_time
            )
            refunded = refund_result.get("refunded", 0)
            refund_amount = refund_result.get("total_amount", 0)

        logger.info(
            f"Cleared stakes: guild={guild_id}, deleted={deleted}, "
            f"refunded_bets={refunded}, refund_amount={refund_amount}"
        )

        return {
            "enabled": True,
            "deleted": deleted,
            "refunded_bets": refunded,
            "refund_amount": refund_amount,
        }

    def get_pending_stakes(
        self,
        guild_id: int | None,
        pending_state: dict,
    ) -> list[dict]:
        """
        Get pending stakes for a guild.

        Args:
            guild_id: Guild ID
            pending_state: Pending match state with timestamp

        Returns:
            List of pending stake dicts
        """
        if not self.config.enabled:
            return []

        stake_time = pending_state.get("shuffle_timestamp")
        if stake_time is None:
            return []

        return self.stake_repo.get_pending_stakes(guild_id, stake_time)

    def get_player_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        pending_state: dict,
    ) -> dict | None:
        """
        Get a player's current pool bet.

        Args:
            guild_id: Guild ID
            discord_id: Player's Discord ID
            pending_state: Match state

        Returns:
            Bet dict or None
        """
        if not self.player_pool_bet_repo:
            return None

        shuffle_ts = pending_state.get("shuffle_timestamp")
        if not shuffle_ts:
            return None

        return self.player_pool_bet_repo.get_player_pending_bet(
            guild_id, discord_id, shuffle_ts
        )

    def get_player_stats(self, discord_id: int) -> dict:
        """
        Get stake statistics for a player.

        Args:
            discord_id: Player's Discord ID

        Returns:
            Dict with aggregate stake stats
        """
        return self.stake_repo.get_player_stake_stats(discord_id)

    def is_enabled(self) -> bool:
        """Check if stake pool is enabled."""
        return self.config.enabled

    def get_pool_size(self) -> int:
        """Get the total auto-liquidity pool size."""
        return self.config.pool_size

    def get_stake_per_player(self) -> int:
        """Get auto-liquidity amount per player (for display)."""
        return self.config.stake_per_player

    def format_stake_pool_info(
        self,
        radiant_win_prob: float,
        excluded_count: int = 0,
        pool_state: PoolState | None = None,
    ) -> dict:
        """
        Format stake pool information for embed display.

        Args:
            radiant_win_prob: Probability radiant wins
            excluded_count: Number of excluded players
            pool_state: Optional current pool state with player bets

        Returns:
            Dict with formatted display info
        """
        if pool_state is None:
            radiant_auto, dire_auto = self.calculate_auto_liquidity(radiant_win_prob)
            pool_state = PoolState(
                radiant_auto=radiant_auto,
                dire_auto=dire_auto,
                radiant_bets=0,
                dire_bets=0,
                radiant_win_prob=radiant_win_prob,
            )

        dire_win_prob = 1.0 - radiant_win_prob

        # Determine favored team
        if radiant_win_prob > 0.52:
            favored = "radiant"
        elif dire_win_prob > 0.52:
            favored = "dire"
        else:
            favored = "even"

        # Current multipliers from pool state
        radiant_mult = pool_state.get_multiplier("radiant")
        dire_mult = pool_state.get_multiplier("dire")

        # Excluded player payouts (minted based on odds)
        excluded_payout_radiant = self.calculate_excluded_payout(radiant_win_prob, "radiant")
        excluded_payout_dire = self.calculate_excluded_payout(radiant_win_prob, "dire")

        return {
            "pool_size": self.config.pool_size,
            "stake_per_player": self.config.stake_per_player,
            "radiant_win_prob": radiant_win_prob,
            "dire_win_prob": dire_win_prob,
            "radiant_auto": pool_state.radiant_auto,
            "dire_auto": pool_state.dire_auto,
            "radiant_bets": pool_state.radiant_bets,
            "dire_bets": pool_state.dire_bets,
            "radiant_total": pool_state.radiant_total,
            "dire_total": pool_state.dire_total,
            "radiant_multiplier": round(radiant_mult, 2),
            "dire_multiplier": round(dire_mult, 2),
            "excluded_count": excluded_count,
            "excluded_payout_if_radiant_wins": excluded_payout_radiant,
            "excluded_payout_if_dire_wins": excluded_payout_dire,
            "favored": favored,
        }
