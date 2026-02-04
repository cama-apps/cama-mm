"""
Service for handling player rating recalibration.

Recalibration resets a player's RD (rating deviation) to high uncertainty,
allowing their rating to adjust more quickly based on recent performance.
"""

import logging
import time
from dataclasses import dataclass

from config import (
    RECALIBRATION_COOLDOWN_SECONDS,
    RECALIBRATION_INITIAL_RD,
    RECALIBRATION_INITIAL_VOLATILITY,
)

# Minimum games required before recalibration is allowed
RECALIBRATION_MIN_GAMES = 5
from repositories.player_repository import PlayerRepository
from repositories.recalibration_repository import RecalibrationRepository

logger = logging.getLogger("cama_bot.services.recalibration")


@dataclass
class RecalibrationState:
    """Current recalibration state for a player."""

    discord_id: int
    last_recalibration_at: int | None  # Unix timestamp
    total_recalibrations: int
    is_on_cooldown: bool  # Computed
    cooldown_ends_at: int | None  # Computed (Unix timestamp)


class RecalibrationService:
    """
    Handles player rating recalibration.

    Recalibration allows players to reset their RD (rating deviation) to 350,
    effectively entering a new calibration phase where their rating can change
    more rapidly. The player's current rating is preserved.

    Requirements:
    - Player must have played at least 5 games
    - 90-day cooldown between recalibrations (admin-resettable)
    """

    def __init__(
        self,
        recalibration_repo: RecalibrationRepository,
        player_repo: PlayerRepository,
        cooldown_seconds: int | None = None,
        initial_rd: float | None = None,
        initial_volatility: float | None = None,
        min_games: int | None = None,
    ):
        self.recalibration_repo = recalibration_repo
        self.player_repo = player_repo
        self.cooldown_seconds = (
            cooldown_seconds if cooldown_seconds is not None else RECALIBRATION_COOLDOWN_SECONDS
        )
        self.initial_rd = initial_rd if initial_rd is not None else RECALIBRATION_INITIAL_RD
        self.initial_volatility = (
            initial_volatility if initial_volatility is not None else RECALIBRATION_INITIAL_VOLATILITY
        )
        self.min_games = min_games if min_games is not None else RECALIBRATION_MIN_GAMES

    def get_state(self, discord_id: int, guild_id: int) -> RecalibrationState:
        """Get the current recalibration state for a player."""
        state = self.recalibration_repo.get_state(discord_id, guild_id)
        now = int(time.time())

        if not state:
            return RecalibrationState(
                discord_id=discord_id,
                last_recalibration_at=None,
                total_recalibrations=0,
                is_on_cooldown=False,
                cooldown_ends_at=None,
            )

        last_recal = state["last_recalibration_at"]
        cooldown_ends = last_recal + self.cooldown_seconds if last_recal else None
        is_on_cooldown = cooldown_ends is not None and now < cooldown_ends

        return RecalibrationState(
            discord_id=discord_id,
            last_recalibration_at=last_recal,
            total_recalibrations=state["total_recalibrations"],
            is_on_cooldown=is_on_cooldown,
            cooldown_ends_at=cooldown_ends if is_on_cooldown else None,
        )

    def can_recalibrate(self, discord_id: int, guild_id: int | None) -> dict:
        """
        Check if a player can recalibrate.

        Returns:
            Dict with 'allowed' (bool) and 'reason' (str if not allowed)
        """
        # Normalize guild_id (None -> 0)
        normalized_guild_id = guild_id if guild_id is not None else 0

        # Check if player exists
        player = self.player_repo.get_by_id(discord_id, normalized_guild_id)
        if not player:
            return {
                "allowed": False,
                "reason": "not_registered",
            }

        # Check if player has a rating
        glicko_data = self.player_repo.get_glicko_rating(discord_id, normalized_guild_id)
        if not glicko_data:
            return {
                "allowed": False,
                "reason": "no_rating",
            }

        rating, rd, volatility = glicko_data

        # Check minimum games requirement
        games_played = self.player_repo.get_game_count(discord_id, normalized_guild_id)
        if games_played < self.min_games:
            return {
                "allowed": False,
                "reason": "insufficient_games",
                "games_played": games_played,
                "min_games": self.min_games,
            }

        # Check cooldown
        state = self.get_state(discord_id, normalized_guild_id)
        if state.is_on_cooldown:
            return {
                "allowed": False,
                "reason": "on_cooldown",
                "cooldown_ends_at": state.cooldown_ends_at,
            }

        return {
            "allowed": True,
            "current_rating": rating,
            "current_rd": rd,
            "current_volatility": volatility,
            "games_played": games_played,
        }

    def recalibrate(self, discord_id: int, guild_id: int | None) -> dict:
        """
        Execute recalibration for a player.

        - Preserves current rating
        - Resets RD to initial_rd (350)
        - Resets volatility to initial_volatility (0.06)
        - Records recalibration timestamp and increments count

        Returns:
            Dict with success status and details
        """
        # Normalize guild_id (None -> 0)
        normalized_guild_id = guild_id if guild_id is not None else 0

        check = self.can_recalibrate(discord_id, guild_id)
        if not check["allowed"]:
            return {"success": False, **check}

        now = int(time.time())
        state = self.get_state(discord_id, normalized_guild_id)

        # Get current rating to preserve
        old_rating = check["current_rating"]
        old_rd = check["current_rd"]
        old_volatility = check["current_volatility"]

        # Update player's Glicko rating (preserve rating, reset RD and volatility)
        self.player_repo.update_glicko_rating(
            discord_id=discord_id,
            guild_id=normalized_guild_id,
            rating=old_rating,
            rd=self.initial_rd,
            volatility=self.initial_volatility,
        )

        # Record recalibration state
        self.recalibration_repo.upsert_state(
            discord_id=discord_id,
            guild_id=normalized_guild_id,
            last_recalibration_at=now,
            total_recalibrations=state.total_recalibrations + 1,
            rating_at_recalibration=old_rating,
        )

        cooldown_ends_at = now + self.cooldown_seconds

        logger.info(
            f"Player {discord_id} recalibrated: rating={old_rating:.1f}, "
            f"RD {old_rd:.1f} -> {self.initial_rd}, "
            f"volatility {old_volatility:.4f} -> {self.initial_volatility}, "
            f"total_recalibrations={state.total_recalibrations + 1}"
        )

        return {
            "success": True,
            "old_rating": old_rating,
            "old_rd": old_rd,
            "old_volatility": old_volatility,
            "new_rd": self.initial_rd,
            "new_volatility": self.initial_volatility,
            "total_recalibrations": state.total_recalibrations + 1,
            "cooldown_ends_at": cooldown_ends_at,
        }

    def reset_cooldown(self, discord_id: int, guild_id: int) -> dict:
        """
        Reset recalibration cooldown for a player (admin action).

        Returns:
            Dict with success status
        """
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if not player:
            return {
                "success": False,
                "reason": "not_registered",
            }

        state = self.get_state(discord_id, guild_id)
        if state.last_recalibration_at is None:
            return {
                "success": False,
                "reason": "no_recalibration_history",
            }

        self.recalibration_repo.reset_cooldown(discord_id, guild_id)

        logger.info(f"Admin reset recalibration cooldown for player {discord_id}")

        return {
            "success": True,
            "total_recalibrations": state.total_recalibrations,
        }
