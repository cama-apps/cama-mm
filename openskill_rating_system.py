"""
OpenSkill Plackett-Luce rating system for fantasy-weighted team ratings.

This system runs parallel to Glicko-2, using fantasy points as weights
to reward individual performance within team context.

Key differences from Glicko-2:
- Uses mu (mean skill) and sigma (uncertainty) instead of rating/rd
- Fantasy points weight individual contribution to team result
- Higher fantasy = more credit for wins, less blame for losses
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

from openskill.models import PlackettLuce

if TYPE_CHECKING:
    from openskill.rate import Rating

logger = logging.getLogger("cama_bot.openskill")


class CamaOpenSkillSystem:
    """
    Manages OpenSkill Plackett-Luce ratings using fantasy points as weights.

    Weight behavior (no inversion needed):
    - Winners: high fantasy = high weight = more rating gain (you carried)
    - Winners: low fantasy = low weight = less rating gain (got carried)
    - Losers: high fantasy = high weight = less rating loss (did your job)
    - Losers: low fantasy = low weight = more rating loss (contributed to loss)
    """

    DEFAULT_MU = 25.0
    DEFAULT_SIGMA = 25.0 / 3.0  # ~8.333

    # Weight bounds for normalization (OpenSkill normalizes within this range)
    WEIGHT_MIN = 1.0
    WEIGHT_MAX = 10.0

    # Default weight when no fantasy data available
    DEFAULT_WEIGHT = 1.0

    # Calibration threshold (sigma below this = calibrated)
    CALIBRATION_THRESHOLD = 4.0

    def __init__(self):
        self.model = PlackettLuce(
            mu=self.DEFAULT_MU,
            sigma=self.DEFAULT_SIGMA,
        )

    def create_rating(
        self, mu: float | None = None, sigma: float | None = None, name: str | None = None
    ) -> Rating:
        """
        Create an OpenSkill rating from stored values or defaults.

        Args:
            mu: Skill estimate (default: 25.0)
            sigma: Uncertainty (default: ~8.333)
            name: Optional identifier for the rating

        Returns:
            OpenSkill Rating object
        """
        actual_mu = mu if mu is not None else self.DEFAULT_MU
        actual_sigma = sigma if sigma is not None else self.DEFAULT_SIGMA
        return self.model.create_rating([actual_mu, actual_sigma], name=name)

    def update_ratings_after_match(
        self,
        team1_data: list[tuple[int, float | None, float | None, float | None]],
        team2_data: list[tuple[int, float | None, float | None, float | None]],
        winning_team: int,
    ) -> dict[int, tuple[float, float, float | None]]:
        """
        Update ratings using Plackett-Luce with fantasy weights.

        Args:
            team1_data: List of (discord_id, mu, sigma, fantasy_points) for Radiant
            team2_data: List of (discord_id, mu, sigma, fantasy_points) for Dire
            winning_team: 1 for Radiant, 2 for Dire

        Returns:
            Dict mapping discord_id -> (new_mu, new_sigma, fantasy_weight_used)
        """
        # Create ratings for each player
        team1_ratings = []
        team1_ids = []
        team1_weights = []

        for discord_id, mu, sigma, fantasy_points in team1_data:
            rating = self.create_rating(mu, sigma, name=str(discord_id))
            team1_ratings.append(rating)
            team1_ids.append(discord_id)
            # Use fantasy points as weight, or default if not available
            weight = fantasy_points if fantasy_points is not None else self.DEFAULT_WEIGHT
            team1_weights.append(max(weight, self.DEFAULT_WEIGHT))  # Ensure positive weight

        team2_ratings = []
        team2_ids = []
        team2_weights = []

        for discord_id, mu, sigma, fantasy_points in team2_data:
            rating = self.create_rating(mu, sigma, name=str(discord_id))
            team2_ratings.append(rating)
            team2_ids.append(discord_id)
            weight = fantasy_points if fantasy_points is not None else self.DEFAULT_WEIGHT
            team2_weights.append(max(weight, self.DEFAULT_WEIGHT))

        # Set ranks based on winning team (1 = winner, 2 = loser)
        if winning_team == 1:
            ranks = [1, 2]  # Team 1 won
        else:
            ranks = [2, 1]  # Team 2 won

        teams = [team1_ratings, team2_ratings]
        weights = [team1_weights, team2_weights]

        # Update ratings
        try:
            updated_teams = self.model.rate(teams, ranks=ranks, weights=weights)
        except Exception as e:
            logger.error(f"OpenSkill rate() failed: {e}")
            raise

        # Extract results
        results: dict[int, tuple[float, float, float | None]] = {}

        for i, (rating, discord_id, weight) in enumerate(
            zip(updated_teams[0], team1_ids, team1_weights)
        ):
            fantasy_used = team1_data[i][3]  # Original fantasy points (may be None)
            results[discord_id] = (rating.mu, rating.sigma, fantasy_used)

        for i, (rating, discord_id, weight) in enumerate(
            zip(updated_teams[1], team2_ids, team2_weights)
        ):
            fantasy_used = team2_data[i][3]
            results[discord_id] = (rating.mu, rating.sigma, fantasy_used)

        return results

    def ordinal(self, mu: float, sigma: float, z: float = 3.0) -> float:
        """
        Get conservative skill estimate (99.7% confidence lower bound).

        This is similar to TrueSkill's display rating: mu - z*sigma
        A higher z value gives a more conservative estimate.

        Args:
            mu: Skill estimate
            sigma: Uncertainty
            z: Number of standard deviations (default: 3.0 for 99.7% confidence)

        Returns:
            Conservative skill estimate
        """
        return mu - z * sigma

    def is_calibrated(self, sigma: float) -> bool:
        """
        Check if a player is calibrated (uncertainty below threshold).

        A player is considered calibrated when their sigma drops below
        CALIBRATION_THRESHOLD (~4.0), roughly equivalent to Glicko RD of 100.

        Args:
            sigma: Player's uncertainty value

        Returns:
            True if player is calibrated
        """
        return sigma <= self.CALIBRATION_THRESHOLD

    def get_uncertainty_percentage(self, sigma: float) -> float:
        """
        Convert sigma to an uncertainty percentage (0-100%).

        Maps sigma from [0, DEFAULT_SIGMA] to [0, 100].
        Capped at 100% for sigma >= DEFAULT_SIGMA.

        Args:
            sigma: Player's uncertainty value

        Returns:
            Uncertainty percentage (0 = fully certain, 100 = fully uncertain)
        """
        return min(100.0, (sigma / self.DEFAULT_SIGMA) * 100.0)

    def get_certainty_percentage(self, sigma: float) -> float:
        """
        Convert sigma to a certainty percentage (0-100%).

        The inverse of get_uncertainty_percentage.

        Args:
            sigma: Player's uncertainty value

        Returns:
            Certainty percentage (100 = fully certain, 0 = fully uncertain)
        """
        return 100.0 - self.get_uncertainty_percentage(sigma)

    @staticmethod
    def mu_to_display(mu: float) -> int:
        """
        Convert mu to a display rating scaled to Dota 2 MMR-like range.

        Maps mu from OpenSkill range (~10-40) to display range (~0-3000).
        Formula: (mu - 10) * 100 with minimum of 0.

        Args:
            mu: Skill estimate

        Returns:
            Display rating (0-3000 range)
        """
        # OpenSkill mu typically ranges from ~10 (very low) to ~40 (very high)
        # Map to 0-3000 for familiar MMR-like display
        display = max(0, (mu - 10) * 100)
        return int(round(display))

    def os_predict_win_probability(
        self,
        team1_ratings: list[tuple[float, float]],
        team2_ratings: list[tuple[float, float]],
    ) -> float:
        """
        Predict OpenSkill win probability for team1 against team2.

        Uses OpenSkill's predict_win() which implements Bradley-Terry model.

        Args:
            team1_ratings: List of (mu, sigma) tuples for team 1
            team2_ratings: List of (mu, sigma) tuples for team 2

        Returns:
            Probability that team1 wins (0.0 to 1.0)
        """
        if not team1_ratings or not team2_ratings:
            return 0.5

        # Create Rating objects for each team
        team1 = [self.create_rating(mu, sigma) for mu, sigma in team1_ratings]
        team2 = [self.create_rating(mu, sigma) for mu, sigma in team2_ratings]

        # predict_win returns [team1_prob, team2_prob]
        try:
            probs = self.model.predict_win([team1, team2])
            return probs[0]  # team1 win probability
        except Exception as e:
            logger.warning(f"OpenSkill predict_win failed: {e}")
            return 0.5
