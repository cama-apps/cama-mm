"""
Service for the daily MTG mana land system.

Each player may claim exactly one mana land per day (reset at 4 AM PST).
The land is randomly selected from five options weighted by player attributes.
"""

import logging
import random
from datetime import datetime, timedelta
from typing import TYPE_CHECKING

logger = logging.getLogger("cama_bot.services.mana")

if TYPE_CHECKING:
    from repositories.mana_repository import ManaRepository
    from repositories.player_repository import PlayerRepository
    from repositories.tip_repository import TipRepository
    from services.gambling_stats_service import GamblingStatsService
    from services.bankruptcy_service import BankruptcyService

# Reset boundary: 4 AM Pacific
RESET_HOUR = 4
RESET_TZ = "America/Los_Angeles"

LAND_COLORS: dict[str, str] = {
    "Island": "Blue",
    "Mountain": "Red",
    "Forest": "Green",
    "Plains": "White",
    "Swamp": "Black",
}

LAND_ORDER = ("Island", "Mountain", "Forest", "Plains", "Swamp")

LAND_EMOJIS: dict[str, str] = {
    "Island": "🏝️",
    "Mountain": "⛰️",
    "Forest": "🌲",
    "Plains": "🌾",
    "Swamp": "🌿",
}


def get_today_pst() -> str:
    """Return today's date string 'YYYY-MM-DD' in PST using the 4 AM reset boundary.

    If the current LA time is before 4 AM, 'today' is still the previous day's date.
    """
    from zoneinfo import ZoneInfo

    la_tz = ZoneInfo(RESET_TZ)
    now_la = datetime.now(la_tz)

    if now_la.hour < RESET_HOUR:
        effective = now_la - timedelta(days=1)
    else:
        effective = now_la

    return effective.strftime("%Y-%m-%d")


class ManaService:
    """Handles daily mana assignment and weight calculation."""

    def __init__(
        self,
        mana_repo: "ManaRepository",
        player_repo: "PlayerRepository",
        gambling_stats_service: "GamblingStatsService",
        bankruptcy_service: "BankruptcyService",
        tip_repo: "TipRepository",
    ):
        self.mana_repo = mana_repo
        self.player_repo = player_repo
        self.gambling_stats_service = gambling_stats_service
        self.bankruptcy_service = bankruptcy_service
        self.tip_repo = tip_repo

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def has_assigned_today(self, discord_id: int, guild_id: int | None) -> bool:
        """Return True if the player already claimed their mana today."""
        row = self.mana_repo.get_mana(discord_id, guild_id)
        if row is None:
            return False
        return row["assigned_date"] == get_today_pst()

    def get_current_mana(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Return {land, color, assigned_date} or None if never assigned."""
        row = self.mana_repo.get_mana(discord_id, guild_id)
        if row is None:
            return None
        land = row["current_land"]
        return {
            "land": land,
            "color": LAND_COLORS.get(land, "Unknown"),
            "emoji": LAND_EMOJIS.get(land, "❓"),
            "assigned_date": row["assigned_date"],
        }

    def assign_all_daily_mana(
        self, guild_id: int | None, *, ash_fan_ids: set[int] | None = None
    ) -> None:
        """Assign today's mana to every registered player who hasn't been assigned yet.

        Args:
            guild_id: Guild to process.
            ash_fan_ids: Discord IDs that have an "ash" role (checked by the command layer).
        """
        gid = self.player_repo.normalize_guild_id(guild_id)
        players = self.player_repo.get_all(gid)
        ash_fan_ids = ash_fan_ids or set()

        for player in players:
            if player.discord_id is None:
                continue
            if self.has_assigned_today(player.discord_id, guild_id):
                continue
            try:
                self.assign_daily_mana(
                    player.discord_id, guild_id,
                    is_ash_fan=player.discord_id in ash_fan_ids,
                )
            except ValueError:
                logger.debug("Race condition: mana already assigned for player %s in guild %s", player.discord_id, guild_id)

    def assign_daily_mana(
        self, discord_id: int, guild_id: int | None, *, is_ash_fan: bool = False
    ) -> dict:
        """Assign today's mana land.  Raises ValueError if already assigned today.

        Returns:
            {"land": str, "color": str, "emoji": str}
        """
        if self.has_assigned_today(discord_id, guild_id):
            raise ValueError("Already assigned today")

        weights = self.calculate_land_weights(discord_id, guild_id, is_ash_fan=is_ash_fan)
        lands = list(weights.keys())
        w = list(weights.values())
        land = random.choices(lands, weights=w, k=1)[0]
        today = get_today_pst()
        self.mana_repo.set_mana(discord_id, guild_id, land, today)

        return {
            "land": land,
            "color": LAND_COLORS[land],
            "emoji": LAND_EMOJIS[land],
        }

    # ------------------------------------------------------------------
    # Weight calculation
    # ------------------------------------------------------------------

    def calculate_land_weights(
        self, discord_id: int, guild_id: int | None, *, is_ash_fan: bool = False
    ) -> dict[str, float]:
        """Return unnormalized weight dict for all five lands.

        random.choices() will normalise the weights automatically.
        """
        weights = {
            "Island": 1.0,
            "Mountain": 1.0,
            "Swamp": 1.0,
            "Plains": 1.0,
            "Forest": 2.0,  # Higher baseline — default for average players
        }

        # Gather data once, share across land calculations
        player = self.player_repo.get_by_id(discord_id, self.player_repo.normalize_guild_id(guild_id))
        balance: int = player.jopacoin_balance if player else 0
        wins: int = player.wins if player else 0
        losses: int = player.losses if player else 0
        glicko: float | None = player.glicko_rating if player else None
        lowest_balance: int | None = self.player_repo.get_lowest_balance(discord_id, guild_id)

        total_games = wins + losses
        win_rate = wins / total_games if total_games > 0 else 0.0

        degen = self.gambling_stats_service.calculate_degen_score(discord_id, guild_id)
        bk_state = self.bankruptcy_service.get_state(discord_id, guild_id)
        tip_stats = self.tip_repo.get_user_tip_stats(discord_id, guild_id)

        # Fetch current win streak from match repo (optional helper)
        current_streak = self._get_current_win_streak(discord_id, guild_id)

        # --- Island (Blue — wealth, intellect, upper class) ---
        if balance >= 500:
            weights["Island"] += 6.0
        elif balance >= 200:
            weights["Island"] += 4.0
        elif balance >= 100:
            weights["Island"] += 2.0

        if glicko is not None:
            if glicko >= 4500:
                weights["Island"] += 4.0
            elif glicko >= 3000:
                weights["Island"] += 2.0

        if win_rate >= 0.65 and total_games >= 10:
            weights["Island"] += 2.0

        if is_ash_fan:
            weights["Island"] += 4.0

        never_bankrupt = (bk_state.last_bankruptcy_at is None)
        if balance > 0 and never_bankrupt and degen.total < 30:
            weights["Island"] += 1.5

        # --- Mountain (Red — aggression, chaos, fire) ---
        if degen.total >= 80:
            weights["Mountain"] += 6.0
        elif degen.total >= 55:
            weights["Mountain"] += 4.0
        elif degen.total >= 30:
            weights["Mountain"] += 2.0

        if degen.max_leverage_score >= 20:
            weights["Mountain"] += 2.0

        if degen.loss_chase_score >= 4:
            weights["Mountain"] += 1.5

        if degen.bet_size_score >= 20:
            weights["Mountain"] += 1.5

        if degen.negative_loan_bonus > 0:
            weights["Mountain"] += 2.0

        if current_streak < -3:
            weights["Mountain"] += 1.0

        # --- Swamp (Black — ruin, debt, despair) ---
        if bk_state.penalty_games_remaining > 0:
            weights["Swamp"] += 7.0

        if balance < 0:
            weights["Swamp"] += 4.0
        elif balance < 5:
            weights["Swamp"] += 2.0

        if lowest_balance is not None and lowest_balance <= -300:
            weights["Swamp"] += 2.0

        if degen.bankruptcy_score >= 10:
            weights["Swamp"] += 2.0

        if degen.debt_depth_score >= 15:
            weights["Swamp"] += 1.5

        if total_games > 20 and win_rate < 0.30:
            weights["Swamp"] += 1.5

        # --- Plains (White — generosity, community, grace) ---
        total_sent: int = tip_stats.get("total_sent", 0)
        tips_sent_count: int = tip_stats.get("tips_sent_count", 0)

        if total_sent >= 500:
            weights["Plains"] += 6.0
        elif total_sent >= 200:
            weights["Plains"] += 4.0
        elif total_sent >= 50:
            weights["Plains"] += 2.0

        if tips_sent_count >= 20:
            weights["Plains"] += 2.0
        elif tips_sent_count >= 10:
            weights["Plains"] += 1.0

        if balance > 0 and degen.total < 20 and tips_sent_count >= 1:
            weights["Plains"] += 1.5

        if 0.45 <= win_rate <= 0.60 and total_games >= 20:
            weights["Plains"] += 1.0

        # --- Forest (Green — standard, balanced, dependable) ---
        if total_games >= 50:
            weights["Forest"] += 2.0
        elif total_games >= 20:
            weights["Forest"] += 1.0

        if 0.40 <= win_rate <= 0.60 and total_games >= 10:
            weights["Forest"] += 2.0

        if 5 <= balance <= 99:
            weights["Forest"] += 1.5

        if 10 <= degen.total <= 40:
            weights["Forest"] += 1.5

        no_debt = balance >= 0
        no_bankruptcy = bk_state.last_bankruptcy_at is None
        no_extreme_wealth = balance < 100
        if no_debt and no_bankruptcy and no_extreme_wealth:
            weights["Forest"] += 1.0

        if 1 <= tips_sent_count <= 4:
            weights["Forest"] += 0.5

        return weights

    # ------------------------------------------------------------------
    # Private helpers
    # ------------------------------------------------------------------

    def _get_current_win_streak(self, discord_id: int, guild_id: int | None) -> int:
        """Return current win/loss streak as a signed integer (positive=win, negative=loss)."""
        try:
            outcomes = self.gambling_stats_service.bet_repo.get_player_bet_history(
                discord_id, guild_id
            )
            if not outcomes:
                return 0
            streak = 0
            for bet in reversed(outcomes):
                won = bet.get("outcome") == "won"
                if streak == 0:
                    streak = 1 if won else -1
                elif streak > 0 and won:
                    streak += 1
                elif streak < 0 and not won:
                    streak -= 1
                else:
                    break
            return streak
        except Exception as e:
            logger.debug("Failed to get win streak for %s: %s", discord_id, e)
            return 0
