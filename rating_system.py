"""
Glicko-2 rating system implementation for Cama matchmaking.
"""

import math
from config import (
    CALIBRATION_RD_THRESHOLD,
    MAX_RATING_SWING_PER_GAME,
    RD_DECAY_CONSTANT,
    RD_DECAY_GRACE_PERIOD_WEEKS,
)

from glicko2 import Player


class CamaRatingSystem:
    """
    Manages Glicko-2 ratings for players.

    Handles:
    - Seeding from OpenDota MMR
    - Rating updates after matches
    - Configurable scale conversion
    """

    # Glicko-2 constants
    TAU = 0.5  # Volatility constraint (default 0.5)
    GLICKO2_SCALE = 173.7178  # Rating scale conversion constant

    # MMR to Glicko-2 rating mapping
    # Maps full MMR range to full Glicko-2 range
    MMR_MIN = 0  # Minimum expected MMR
    MMR_MAX = 12000  # Maximum expected MMR (covers Immortal+)
    RATING_MIN = 0  # Minimum Glicko-2 rating
    RATING_MAX = 3000  # Maximum Glicko-2 rating (standard Glicko-2 range)

    @classmethod
    def mmr_to_rating_scale(cls) -> float:
        """Calculate the scale factor for MMR to rating conversion."""
        return (cls.RATING_MAX - cls.RATING_MIN) / (cls.MMR_MAX - cls.MMR_MIN)

    def __init__(self, initial_rd: float = 350.0, initial_volatility: float = 0.06):
        """
        Initialize rating system.

        Args:
            initial_rd: Initial rating deviation (uncertainty)
                       Higher = more uncertain (new players)
            initial_volatility: Initial volatility
        """
        self.initial_rd = initial_rd
        self.initial_volatility = initial_volatility

    def aggregate_team_stats(self, players: list[Player]) -> tuple[float, float, float]:
        """
        Aggregate a team into rating, RD, and volatility snapshots.

        Uses mean rating and RMS RD. This aggregate is used to represent the
        opponent team's strength when computing individual player updates.
        """
        if not players:
            return 0.0, 350.0, self.initial_volatility
        mean_rating = sum(p.rating for p in players) / len(players)
        rms_rd = math.sqrt(sum(p.rd**2 for p in players) / len(players))
        mean_vol = sum(p.vol for p in players) / len(players)
        return mean_rating, rms_rd, mean_vol

    @staticmethod
    def is_calibrated(rd: float) -> bool:
        """Return True if the player's RD is at or below the calibration threshold."""
        return rd <= CALIBRATION_RD_THRESHOLD

    @staticmethod
    def apply_rd_decay(rd: float, days_since_last_match: int) -> float:
        """
        Apply Glicko-2 style RD decay over time.

        - Uses c (RD_DECAY_CONSTANT) and time periods in weeks (rounded down).
        - Grace period: no decay for the first RD_DECAY_GRACE_PERIOD_WEEKS.
        - RD is capped at 350.
        - If RD is already 350, return as-is.
        """
        if rd >= 350.0:
            return 350.0

        if days_since_last_match < RD_DECAY_GRACE_PERIOD_WEEKS * 7:
            return rd

        weeks = max(0, days_since_last_match // 7)
        if weeks == 0:
            return rd

        new_rd = math.sqrt(rd * rd + (RD_DECAY_CONSTANT * RD_DECAY_CONSTANT) * weeks)
        return min(350.0, new_rd)

    @classmethod
    def expected_outcome(
        cls, rating: float, rd: float, opponent_rating: float, opponent_rd: float
    ) -> float:
        """
        Estimate win probability given two ratings and opponent RD.
        """
        g = 1.0 / math.sqrt(
            1.0 + (3.0 * (opponent_rd / cls.GLICKO2_SCALE) ** 2) / (math.pi**2)
        )
        expectation = 1.0 / (
            1.0 + math.exp(-g * (rating - opponent_rating) / cls.GLICKO2_SCALE)
        )
        return min(1.0, max(0.0, expectation))

    def mmr_to_rating(self, mmr: int) -> float:
        """
        Convert OpenDota MMR to Glicko-2 rating.

        Maps MMR range (0-12000) to Glicko-2 range (0-3000) linearly.
        This ensures new players aren't undervalued and the full range is used.

        Args:
            mmr: MMR from OpenDota

        Returns:
            Glicko-2 rating (0-3000 range)
        """
        # Clamp MMR to expected range
        mmr_clamped = max(self.MMR_MIN, min(mmr, self.MMR_MAX))

        # Linear mapping: (MMR - MMR_MIN) / (MMR_MAX - MMR_MIN) * (RATING_MAX - RATING_MIN) + RATING_MIN
        scale = self.mmr_to_rating_scale()
        rating = (mmr_clamped - self.MMR_MIN) * scale + self.RATING_MIN

        return rating

    def rating_to_display(self, rating: float) -> int:
        """
        Convert Glicko-2 rating to display value (Cama Rating).

        Cama Rating is displayed directly as the Glicko-2 rating (0-3000 range).

        Args:
            rating: Glicko-2 rating

        Returns:
            Display rating (rounded to integer)
        """
        return int(round(rating))

    def create_player_from_mmr(self, mmr: int | None) -> Player:
        """
        Create a Glicko-2 player seeded from MMR.

        Args:
            mmr: MMR from OpenDota (None if not available)

        Returns:
            Glicko-2 Player object
        """
        if mmr is not None:
            rating = self.mmr_to_rating(mmr)
        else:
            # Default rating if no MMR (use average MMR ~4000 = ~1000 rating)
            rating = self.mmr_to_rating(4000)

        return Player(rating=rating, rd=self.initial_rd, vol=self.initial_volatility)

    def create_player_from_rating(self, rating: float, rd: float, volatility: float) -> Player:
        """
        Create a Glicko-2 player from stored rating data.

        Args:
            rating: Current Glicko-2 rating
            rd: Current rating deviation
            volatility: Current volatility

        Returns:
            Glicko-2 Player object
        """
        return Player(rating=rating, rd=rd, vol=volatility)

    def _update_player_rating(
        self,
        player: Player,
        team_rating: float,
        opponent_rating: float,
        opponent_rd: float,
        result: float,
    ) -> tuple[float, float, float]:
        """
        Compute updated rating, RD, and volatility for a single player.

        Uses team-based expected outcome: the player's delta is computed as if
        a synthetic player at team_rating (with this player's RD) played the match.
        This ensures all teammates gain/lose in the same direction based on
        team-level expected outcome, preventing rating compression.

        Args:
            player: The player to update
            team_rating: This player's team average rating
            opponent_rating: Opponent team average rating
            opponent_rd: Opponent team RMS RD
            result: 1.0 for win, 0.0 for loss

        Returns:
            Tuple of (new_rating, new_rd, new_vol)
        """
        # Use TEAM rating for expected outcome, but PLAYER's RD for convergence speed
        synth = Player(rating=team_rating, rd=player.rd, vol=player.vol)
        synth.update_player([opponent_rating], [opponent_rd], [result])

        # Delta is how much the "team representative with this player's RD" moved
        delta = synth.rating - team_rating
        delta = max(-MAX_RATING_SWING_PER_GAME, min(MAX_RATING_SWING_PER_GAME, delta))

        final_rating = max(0.0, player.rating + delta)
        # RD and volatility from the synthetic player (team-based update)
        # RD should never increase after a match
        final_rd = min(player.rd, synth.rd)
        final_vol = synth.vol

        return final_rating, final_rd, final_vol

    def update_ratings_after_match(
        self,
        team1_players: list[tuple[Player, int]],  # (player, discord_id)
        team2_players: list[tuple[Player, int]],
        winning_team: int,
    ) -> tuple[list[tuple[float, float, float, int]], list[tuple[float, float, float, int]]]:
        """
        Update ratings after a match using team-based expected outcome with individual RD.

        V3 approach (fixes V2 RD² concentration issues):
        1. Expected outcome is TEAM vs TEAM (not individual vs team)
        2. Each player's delta magnitude is determined by their RD (convergence speed)
        3. Apply ±MAX_RATING_SWING_PER_GAME cap to prevent extreme outliers
        4. RD always decreases after match

        Key insight: When a team wins/loses, ALL players on that team should gain/lose
        in the same direction. The expected outcome depends on team ratings, not
        individual player rating vs opponent team.

        This prevents rating compression where:
        - Low-rated players slowly inflate (would lose less, gain more personally)
        - High-rated players slowly deflate (would gain less, lose more personally)

        Expected outcomes (based on RD, not rating):
        - New player (RD 350): ±150-200 (capped at MAX_RATING_SWING_PER_GAME)
        - Settling (RD 150): ±40-60
        - Calibrated (RD 80): ±10-20

        Args:
            team1_players: List of (Glicko-2 Player, discord_id) for team 1 (non-empty)
            team2_players: List of (Glicko-2 Player, discord_id) for team 2 (non-empty)
            winning_team: 1 or 2 (which team won)

        Returns:
            Tuple of (team1_updated_ratings, team2_updated_ratings)
            Each rating is (rating, rd, volatility, discord_id)

        Raises:
            ValueError: If either team is empty or winning_team is not 1 or 2
        """
        if not team1_players:
            raise ValueError("team1_players cannot be empty")
        if not team2_players:
            raise ValueError("team2_players cannot be empty")
        if winning_team not in (1, 2):
            raise ValueError(f"winning_team must be 1 or 2, got {winning_team}")

        # Get team aggregates (mean rating, RMS RD)
        team1_rating, team1_rd, _ = self.aggregate_team_stats([p for p, _ in team1_players])
        team2_rating, team2_rd, _ = self.aggregate_team_stats([p for p, _ in team2_players])

        team1_result = 1.0 if winning_team == 1 else 0.0
        team2_result = 1.0 if winning_team == 2 else 0.0

        # Update all players using the helper method
        team1_updated = [
            (*self._update_player_rating(player, team1_rating, team2_rating, team2_rd, team1_result), discord_id)
            for player, discord_id in team1_players
        ]
        team2_updated = [
            (*self._update_player_rating(player, team2_rating, team1_rating, team1_rd, team2_result), discord_id)
            for player, discord_id in team2_players
        ]

        return team1_updated, team2_updated

    def get_rating_uncertainty_percentage(self, rd: float) -> float:
        """
        Convert RD to a percentage uncertainty for display.

        Args:
            rd: Rating deviation

        Returns:
            Uncertainty percentage (0-100)
        """
        # RD ranges from ~30 (very certain) to ~350 (very uncertain)
        # Convert to percentage: 0% = certain, 100% = very uncertain
        uncertainty = min(100, (rd / 350.0) * 100)
        return round(uncertainty, 1)
