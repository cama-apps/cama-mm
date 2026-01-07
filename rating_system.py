"""
Glicko-2 rating system implementation for Cama matchmaking.
"""

import math
from config import CALIBRATION_RD_THRESHOLD, RD_DECAY_CONSTANT, RD_DECAY_GRACE_PERIOD_WEEKS

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

        Uses mean rating and RMS RD to represent overall uncertainty.
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

    def update_ratings_after_match(
        self,
        team1_players: list[tuple[Player, int]],  # (player, discord_id)
        team2_players: list[tuple[Player, int]],
        winning_team: int,
    ) -> tuple[list[tuple[float, float, float, int]], list[tuple[float, float, float, int]]]:
        """
        Update ratings after a match.

        Args:
            team1_players: List of (Glicko-2 Player, discord_id) for team 1
            team2_players: List of (Glicko-2 Player, discord_id) for team 2
            winning_team: 1 or 2 (which team won)

        Returns:
            Tuple of (team1_updated_ratings, team2_updated_ratings)
            Each rating is (rating, rd, volatility, discord_id)
        """

        # Aggregated team views (for opponent strength)
        team1_rating, team1_rd, team1_vol = self.aggregate_team_stats(
            [p for p, _ in team1_players]
        )
        team2_rating, team2_rd, team2_vol = self.aggregate_team_stats(
            [p for p, _ in team2_players]
        )

        team1_result = 1.0 if winning_team == 1 else 0.0
        team2_result = 1.0 if winning_team == 2 else 0.0

        # Team-level synthetic players to compute shared rating deltas
        team1_synthetic = Player(rating=team1_rating, rd=team1_rd, vol=team1_vol)
        team2_synthetic = Player(rating=team2_rating, rd=team2_rd, vol=team2_vol)
        team1_synthetic.update_player([team2_rating], [team2_rd], [team1_result])
        team2_synthetic.update_player([team1_rating], [team1_rd], [team2_result])

        def _dampen_delta(
            delta: float, team_rating: float, opponent_rating: float, result: float
        ) -> float:
            # If a heavy favorite loses, soften the drop to avoid over-penalizing.
            if result == 0.0 and (team_rating - opponent_rating) > 200:
                return delta * 0.05
            return delta

        team1_delta = _dampen_delta(
            team1_synthetic.rating - team1_rating, team1_rating, team2_rating, team1_result
        )
        team2_delta = _dampen_delta(
            team2_synthetic.rating - team2_rating, team2_rating, team1_rating, team2_result
        )

        team1_updated = []
        team2_updated = []

        # Update RD/vol individually, then apply shared team delta so deltas match across team members.
        for player, discord_id in team1_players:
            original = player.rating
            player.update_player([team2_rating], [team2_rd], [team1_result])
            # Keep RD/vol updates, but enforce uniform team delta for ratings.
            player.rating = max(0.0, original + team1_delta)
            team1_updated.append((player.rating, player.rd, player.vol, discord_id))

        for player, discord_id in team2_players:
            original = player.rating
            player.update_player([team1_rating], [team1_rd], [team2_result])
            player.rating = max(0.0, original + team2_delta)
            team2_updated.append((player.rating, player.rd, player.vol, discord_id))

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
