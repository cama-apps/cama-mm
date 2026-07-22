"""
Service for the daily MTG mana land system.

Each player may claim exactly one mana land per day (reset at 4 AM PST).
The land is randomly selected from five options weighted by player attributes.
"""

import logging
import random
from collections.abc import Mapping
from datetime import datetime, timedelta
from typing import TYPE_CHECKING

logger = logging.getLogger("cama_bot.services.mana")

if TYPE_CHECKING:
    from domain.models.player import Player
    from repositories.mana_repository import ManaRepository
    from repositories.player_repository import PlayerRepository
    from repositories.tip_repository import TipRepository
    from services.bankruptcy_service import BankruptcyService
    from services.gambling_stats_service import GamblingStatsService

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

_UNSET = object()


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


def get_mana_day_start_timestamp(now: datetime | None = None) -> int:
    """Return the current mana day's 4 AM Los Angeles boundary as Unix time."""
    from zoneinfo import ZoneInfo

    la_tz = ZoneInfo(RESET_TZ)
    now_la = now.astimezone(la_tz) if now is not None else datetime.now(la_tz)
    boundary = now_la.replace(hour=RESET_HOUR, minute=0, second=0, microsecond=0)
    if now_la < boundary:
        boundary -= timedelta(days=1)
    return int(boundary.timestamp())


class ManaService:
    """Handles daily mana assignment and weight calculation."""

    def __init__(
        self,
        mana_repo: "ManaRepository",
        player_repo: "PlayerRepository",
        gambling_stats_service: "GamblingStatsService",
        bankruptcy_service: "BankruptcyService",
        tip_repo: "TipRepository",
        protection_service=None,
    ):
        self.mana_repo = mana_repo
        self.player_repo = player_repo
        self.gambling_stats_service = gambling_stats_service
        self.bankruptcy_service = bankruptcy_service
        self.tip_repo = tip_repo
        # ProtectionService is built later in the production container and is
        # back-filled there. Keeping this optional preserves lightweight tests.
        self.protection_service = protection_service

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
        """Return current mana details or ``None`` if never assigned.

        The result includes land, color, emoji, assigned date, Guardian capacity,
        and whether today's mana was consumed. A stale assignment always reports
        ``consumed=False`` and zero Guardian capacity.
        """
        row = self.mana_repo.get_mana(discord_id, guild_id)
        if row is None:
            return None
        land = row["current_land"]
        # Guardian/tap state only makes sense for today's assignment; a stale
        # row from a previous day must not present yesterday's shield as live.
        assigned_today = row["assigned_date"] == get_today_pst()
        return {
            "land": land,
            "color": LAND_COLORS.get(land, "Unknown"),
            "emoji": LAND_EMOJIS.get(land, "❓"),
            "assigned_date": row["assigned_date"],
            "guardian_remaining": (
                int(row.get("white_shield_remaining", 0) or 0)
                if (land == "Plains" and assigned_today)
                else 0
            ),
            "consumed": bool(row.get("consumed_today", 0)) if assigned_today else False,
        }

    def is_mana_consumed(self, discord_id: int, guild_id: int | None) -> bool:
        """Return True if today's mana has been tapped on a manashop ultimate.

        Tapped mana suppresses all passive effects until the 4 AM PST reset.
        """
        return self.mana_repo.is_mana_consumed(discord_id, guild_id)

    def assign_all_daily_mana(
        self, guild_id: int | None, *, ash_fan_ids: set[int] | None = None
    ) -> list[dict]:
        """Assign today's mana to every registered player who hasn't been assigned yet.

        Args:
            guild_id: Guild to process.
            ash_fan_ids: Discord IDs that have an "ash" role (checked by the command layer).

        Returns:
            One :meth:`assign_daily_mana` result dict (plus ``discord_id``) per
            player who was freshly assigned by this call, so the command layer
            can run once-per-claim side effects (e.g. the White stipend)
            exactly as the self-claim path does.
        """
        new_assignments, _ = self.assign_all_daily_mana_with_board(
            guild_id, ash_fan_ids=ash_fan_ids
        )
        return new_assignments

    def assign_all_daily_mana_with_board(
        self, guild_id: int | None, *, ash_fan_ids: set[int] | None = None
    ) -> tuple[list[dict], list[dict]]:
        """Batch-assign the guild and return fresh claims plus board rows.

        Existing mana is loaded once. Candidate lands are calculated without
        reopening player rows or bet histories, then claimed together under a
        single repository transaction. Only transaction winners receive
        once-per-claim Plains reconciliation and are returned to callers.
        """
        gid = self.player_repo.normalize_guild_id(guild_id)
        players = self.player_repo.get_all(gid)
        current_rows = self.mana_repo.get_all_mana(guild_id)
        current_by_id = {int(row["discord_id"]): dict(row) for row in current_rows}
        today = get_today_pst()
        ash_fan_ids = ash_fan_ids or set()

        candidate_players = [
            player
            for player in players
            if player.discord_id is not None
            and current_by_id.get(player.discord_id, {}).get("assigned_date") != today
        ]
        candidate_ids = [int(player.discord_id) for player in candidate_players]

        bankruptcy_states: Mapping[int, object] = {}
        if candidate_ids:
            try:
                loaded_states = self.bankruptcy_service.get_bulk_states(candidate_ids, guild_id)
                if isinstance(loaded_states, Mapping):
                    bankruptcy_states = loaded_states
            except Exception:
                logger.debug(
                    "Failed to bulk-load bankruptcy states for guild %s",
                    guild_id,
                    exc_info=True,
                )

        lowest_balances: Mapping[int, int | None] = {}
        if candidate_ids:
            try:
                loaded_balances = self.player_repo.get_lowest_balances_bulk(candidate_ids, gid)
                if isinstance(loaded_balances, Mapping):
                    lowest_balances = loaded_balances
            except Exception:
                logger.debug(
                    "Failed to bulk-load lowest balances for guild %s",
                    guild_id,
                    exc_info=True,
                )

        degen_scores: Mapping[int, object] = {}
        if candidate_ids:
            try:
                loaded_scores = self.gambling_stats_service.calculate_degen_scores_bulk(
                    candidate_ids, guild_id
                )
                if isinstance(loaded_scores, Mapping):
                    degen_scores = loaded_scores
            except Exception:
                logger.debug(
                    "Failed to bulk-calculate degen scores for guild %s",
                    guild_id,
                    exc_info=True,
                )

        current_streaks: Mapping[int, int] = {}
        if candidate_ids:
            try:
                loaded_streaks = self.gambling_stats_service.bet_repo.get_current_bet_streaks_bulk(
                    candidate_ids, guild_id
                )
                if isinstance(loaded_streaks, Mapping):
                    current_streaks = loaded_streaks
            except Exception:
                logger.debug(
                    "Failed to bulk-load bet streaks for guild %s",
                    guild_id,
                    exc_info=True,
                )

        tip_stats_by_id: Mapping[int, dict] = {}
        if candidate_ids:
            try:
                loaded_tip_stats = self.tip_repo.get_user_tip_stats_bulk(candidate_ids, guild_id)
                if isinstance(loaded_tip_stats, Mapping):
                    tip_stats_by_id = loaded_tip_stats
            except Exception:
                logger.debug(
                    "Failed to bulk-load tip stats for guild %s",
                    guild_id,
                    exc_info=True,
                )

        candidates: list[tuple[int, str]] = []
        for player in candidate_players:
            discord_id = int(player.discord_id)
            weights = self.calculate_land_weights(
                discord_id,
                guild_id,
                is_ash_fan=discord_id in ash_fan_ids,
                player=player,
                bankruptcy_state=bankruptcy_states.get(discord_id, _UNSET),
                lowest_balance=lowest_balances.get(discord_id, _UNSET),
                degen_score=degen_scores.get(discord_id, _UNSET),
                current_streak=current_streaks.get(discord_id, 0),
                tip_stats=tip_stats_by_id.get(discord_id, _UNSET),
            )
            land = random.choices(list(weights), weights=list(weights.values()), k=1)[0]
            candidates.append((discord_id, land))

        claimed_rows = self.mana_repo.claim_mana_batch_atomic(candidates, guild_id, today)
        new_assignments = self._finalize_batch_assignments(claimed_rows, guild_id)

        for row in claimed_rows:
            current_by_id[int(row["discord_id"])] = {
                "discord_id": int(row["discord_id"]),
                "current_land": row["current_land"],
                "assigned_date": row["assigned_date"],
            }
        return new_assignments, list(current_by_id.values())

    def assign_daily_mana(
        self, discord_id: int, guild_id: int | None, *, is_ash_fan: bool = False
    ) -> dict:
        """Assign today's mana land.  Raises ValueError if already assigned today.

        The claim itself is atomic: the pre-check inside ``claim_mana_atomic``
        runs under BEGIN IMMEDIATE, so two concurrent /mana calls can't both
        pass the check and each roll a different land.

        Returns:
            {"land": str, "color": str, "emoji": str}
        """
        bet_history = self.gambling_stats_service.bet_repo.get_player_bet_history(
            discord_id, guild_id
        )
        weights = self.calculate_land_weights(
            discord_id,
            guild_id,
            is_ash_fan=is_ash_fan,
            bet_history=bet_history,
        )
        lands = list(weights.keys())
        w = list(weights.values())
        land = random.choices(lands, weights=w, k=1)[0]
        today = get_today_pst()
        claimed = self.mana_repo.claim_mana_atomic(discord_id, guild_id, land, today)
        if not claimed:
            raise ValueError("Already assigned today")

        retro_refund = 0
        if land == "Plains" and self.protection_service is not None:
            try:
                retro_refund = self.protection_service.reconcile_guardian(
                    discord_id,
                    guild_id,
                    get_mana_day_start_timestamp(),
                )
            except Exception:
                # A reconciliation failure must not invalidate an otherwise
                # successful, atomically claimed daily land.
                logger.exception(
                    "Failed to reconcile White Guardian losses for player %s in guild %s",
                    discord_id,
                    guild_id,
                )

        guardian_remaining = (
            self.mana_repo.get_white_shield_remaining(discord_id, guild_id)
            if land == "Plains"
            else 0
        )
        return {
            "land": land,
            "color": LAND_COLORS[land],
            "emoji": LAND_EMOJIS[land],
            "assigned_date": today,
            "retro_refund": retro_refund,
            "guardian_remaining": guardian_remaining,
            "consumed": False,
        }

    # ------------------------------------------------------------------
    # Weight calculation
    # ------------------------------------------------------------------

    def calculate_land_weights(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        is_ash_fan: bool = False,
        player: "Player | None" = None,
        bet_history: list[dict] | None = None,
        bankruptcy_state=_UNSET,
        lowest_balance: int | None | object = _UNSET,
        degen_score=_UNSET,
        current_streak: int | object = _UNSET,
        tip_stats: dict | object = _UNSET,
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
        if player is None:
            player = self.player_repo.get_by_id(
                discord_id, self.player_repo.normalize_guild_id(guild_id)
            )
        balance: int = player.jopacoin_balance if player else 0
        wins: int = player.wins if player else 0
        losses: int = player.losses if player else 0
        glicko: float | None = player.glicko_rating if player else None
        if lowest_balance is _UNSET:
            lowest_balance = self.player_repo.get_lowest_balance(discord_id, guild_id)

        total_games = wins + losses
        win_rate = wins / total_games if total_games > 0 else 0.0

        if degen_score is not _UNSET:
            degen = degen_score
        elif bet_history is not None:
            degen = self.gambling_stats_service.calculate_degen_score(
                discord_id, guild_id, history=bet_history
            )
        else:
            degen = self.gambling_stats_service.calculate_degen_score(discord_id, guild_id)
        bk_state = (
            self.bankruptcy_service.get_state(discord_id, guild_id)
            if bankruptcy_state is _UNSET
            else bankruptcy_state
        )
        if tip_stats is _UNSET:
            tip_stats = self.tip_repo.get_user_tip_stats(discord_id, guild_id)

        if current_streak is _UNSET:
            current_streak = self._get_current_win_streak(
                discord_id, guild_id, bet_history=bet_history
            )

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

    def _finalize_batch_assignments(
        self, claimed_rows: list[dict], guild_id: int | None
    ) -> list[dict]:
        """Run post-claim Plains reconciliation for batch transaction winners."""
        results: list[dict] = []
        day_start: int | None = None
        for row in claimed_rows:
            discord_id = int(row["discord_id"])
            land = row["current_land"]
            retro_refund = 0
            guardian_remaining = int(row.get("white_shield_remaining", 0) or 0)
            if land == "Plains" and self.protection_service is not None:
                try:
                    if day_start is None:
                        day_start = get_mana_day_start_timestamp()
                    retro_refund = self.protection_service.reconcile_guardian(
                        discord_id, guild_id, day_start
                    )
                    guardian_remaining = max(0, guardian_remaining - retro_refund)
                except Exception:
                    logger.exception(
                        "Failed to reconcile White Guardian losses for player %s in guild %s",
                        discord_id,
                        guild_id,
                    )

            results.append(
                {
                    "discord_id": discord_id,
                    "land": land,
                    "color": LAND_COLORS[land],
                    "emoji": LAND_EMOJIS[land],
                    "assigned_date": row["assigned_date"],
                    "retro_refund": retro_refund,
                    "guardian_remaining": guardian_remaining,
                    "consumed": False,
                }
            )
        return results

    def _get_current_win_streak(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        bet_history: list[dict] | None = None,
    ) -> int:
        """Return current win/loss streak as a signed integer (positive=win, negative=loss)."""
        try:
            outcomes = (
                [bet["outcome"] for bet in bet_history]
                if bet_history is not None
                else self.gambling_stats_service.get_player_bet_outcomes(discord_id, guild_id)
            )
            if not outcomes:
                return 0
            streak = 0
            for outcome in reversed(outcomes):
                won = outcome == "won"
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
