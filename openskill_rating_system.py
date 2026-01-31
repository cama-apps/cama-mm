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

    OpenSkill weight semantics (asymmetric by win/loss):
    - For WINNERS: higher weight = larger gain, lower weight = smaller gain
    - For LOSERS: higher weight = smaller loss, lower weight = larger loss

    This naturally achieves our desired behavior without any inversion:
    - High FP winner → high weight → large gain (more credit)
    - Low FP winner → low weight → small gain (less credit)
    - High FP loser → high weight → small loss (less blame)
    - Low FP loser → low weight → large loss (more blame)

    Weight blending: 10% FP-based + 90% equal (FP is a nudge, not dominant)
    """

    DEFAULT_MU = 25.0
    DEFAULT_SIGMA = 25.0 / 3.0  # ~8.333

    # Weight bounds for normalization (before blending)
    WEIGHT_MIN = 1.0
    WEIGHT_MAX = 3.0  # Reduced from 10 to limit rating swings

    # Fantasy point bounds for normalization (typical observed range)
    FANTASY_MIN = 5.0
    FANTASY_MAX = 30.0

    # Default weight when no fantasy data available
    DEFAULT_WEIGHT = 1.0  # Equal weight (after blending this stays 1.0)

    # Calibration threshold (sigma below this = calibrated)
    CALIBRATION_THRESHOLD = 4.0

    # Weight blending: 10% FP-based, 90% equal weight
    # FP is a tiny nudge, not a significant factor
    FP_WEIGHT_BLEND = 0.10

    # Per-game mu swing cap (prevents massive rating swings)
    # 2.0 mu ≈ 150 display rating points
    MAX_MU_SWING_PER_GAME = 2.0

    # Minimum mu floor (display rating 0)
    MIN_MU = 25.0

    def __init__(self):
        self.model = PlackettLuce(
            mu=self.DEFAULT_MU,
            sigma=self.DEFAULT_SIGMA,
        )

    def normalize_fantasy_weight(self, fantasy_points: float | None) -> float:
        """
        Normalize fantasy points to a bounded weight range.

        Maps fantasy points from typical range (5-30) to weight range (1-3).
        This limits the impact of fantasy weighting to at most 3x difference
        between best and worst performers, reducing massive rating swings.

        Args:
            fantasy_points: Raw fantasy points (or None)

        Returns:
            Normalized weight in [WEIGHT_MIN, WEIGHT_MAX] range
        """
        if fantasy_points is None:
            return self.DEFAULT_WEIGHT

        # Clamp to expected range
        clamped = max(self.FANTASY_MIN, min(self.FANTASY_MAX, fantasy_points))

        # Linear interpolation from fantasy range to weight range
        # (fp - 5) / (30 - 5) * (3 - 1) + 1
        normalized = (
            (clamped - self.FANTASY_MIN)
            / (self.FANTASY_MAX - self.FANTASY_MIN)
            * (self.WEIGHT_MAX - self.WEIGHT_MIN)
            + self.WEIGHT_MIN
        )
        return normalized

    def compute_match_weights(
        self,
        team1_fantasy: list[float | None],
        team2_fantasy: list[float | None],
        team1_won: bool,
    ) -> tuple[list[float], list[float]]:
        """
        Compute weights with blending (no inversion needed).

        OpenSkill weight semantics (asymmetric):
        - For WINNERS: higher weight = larger gain, lower weight = smaller gain
        - For LOSERS: higher weight = smaller loss, lower weight = larger loss

        This naturally achieves our goal without inversion:
        - High FP winner (high weight) → large gain
        - Low FP winner (low weight) → small gain
        - High FP loser (high weight) → small loss (less blame)
        - Low FP loser (low weight) → large loss (more blame)

        Weight calculation:
        1. Normalize FP to raw weights (1-3 range)
        2. Blend: 10% FP weight + 90% equal weight (1.0)

        Args:
            team1_fantasy: Fantasy points for team 1 players (may contain None)
            team2_fantasy: Fantasy points for team 2 players (may contain None)
            team1_won: True if team 1 won (unused, kept for API compatibility)

        Returns:
            Tuple of (team1_weights, team2_weights)
        """
        def compute_team_weights(fantasy_list: list[float | None]) -> list[float]:
            weights = []
            for fp in fantasy_list:
                # Step 1: Normalize FP to raw weight (1-3)
                raw_weight = self.normalize_fantasy_weight(fp)

                # Step 2: Blend with equal weight
                # blended = 0.25 * raw_weight + 0.75 * 1.0
                blended = self.FP_WEIGHT_BLEND * raw_weight + (1.0 - self.FP_WEIGHT_BLEND) * 1.0

                weights.append(blended)
            return weights

        team1_weights = compute_team_weights(team1_fantasy)
        team2_weights = compute_team_weights(team2_fantasy)

        return team1_weights, team2_weights

    def mmr_to_os_mu(self, mmr: int) -> float:
        """
        Convert OpenDota MMR to OpenSkill mu.

        Maps MMR to mu such that display rating matches MMR scale:
        - 0 MMR → mu=25 → display 0
        - 4000 MMR → mu=45 → display 1500
        - 8000 MMR → mu=65 → display 3000

        Formula: mu = 25 + (mmr / 200)

        Args:
            mmr: OpenDota MMR (typically 0-12000+)

        Returns:
            OpenSkill mu value
        """
        return 25 + (mmr / 200)

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

        Weight processing:
        1. Normalize FP to raw weights (1-3)
        2. Blend: 10% FP + 90% equal weight (no inversion needed)

        OpenSkill naturally handles win/loss asymmetry:
        - Winners: high weight = more gain
        - Losers: high weight = less loss (protected from blame)

        Bounds enforcement:
        - Per-game mu change clamped to ±MAX_MU_SWING_PER_GAME
        - Mu floored at MIN_MU (display rating 0)

        Args:
            team1_data: List of (discord_id, mu, sigma, fantasy_points) for Radiant
            team2_data: List of (discord_id, mu, sigma, fantasy_points) for Dire
            winning_team: 1 for Radiant, 2 for Dire

        Returns:
            Dict mapping discord_id -> (new_mu, new_sigma, fantasy_weight_used)
        """
        # Create ratings for each player and track original mu
        team1_ratings = []
        team1_ids = []
        team1_original_mu = []

        for discord_id, mu, sigma, fantasy_points in team1_data:
            actual_mu = mu if mu is not None else self.DEFAULT_MU
            rating = self.create_rating(mu, sigma, name=str(discord_id))
            team1_ratings.append(rating)
            team1_ids.append(discord_id)
            team1_original_mu.append(actual_mu)

        team2_ratings = []
        team2_ids = []
        team2_original_mu = []

        for discord_id, mu, sigma, fantasy_points in team2_data:
            actual_mu = mu if mu is not None else self.DEFAULT_MU
            rating = self.create_rating(mu, sigma, name=str(discord_id))
            team2_ratings.append(rating)
            team2_ids.append(discord_id)
            team2_original_mu.append(actual_mu)

        # Compute weights with blending and loss inversion
        team1_won = winning_team == 1
        team1_fantasy = [fp for _, _, _, fp in team1_data]
        team2_fantasy = [fp for _, _, _, fp in team2_data]
        team1_weights, team2_weights = self.compute_match_weights(
            team1_fantasy, team2_fantasy, team1_won
        )

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

        # Extract results with mu clamping
        results: dict[int, tuple[float, float, float | None]] = {}

        for i, (rating, discord_id) in enumerate(zip(updated_teams[0], team1_ids)):
            old_mu = team1_original_mu[i]
            new_mu = rating.mu

            # Clamp mu change to ±MAX_MU_SWING_PER_GAME
            delta = new_mu - old_mu
            clamped_delta = max(-self.MAX_MU_SWING_PER_GAME, min(self.MAX_MU_SWING_PER_GAME, delta))
            clamped_mu = old_mu + clamped_delta

            # Enforce floor
            clamped_mu = max(self.MIN_MU, clamped_mu)

            fantasy_used = team1_data[i][3]  # Original fantasy points (may be None)
            results[discord_id] = (clamped_mu, rating.sigma, fantasy_used)

        for i, (rating, discord_id) in enumerate(zip(updated_teams[1], team2_ids)):
            old_mu = team2_original_mu[i]
            new_mu = rating.mu

            # Clamp mu change to ±MAX_MU_SWING_PER_GAME
            delta = new_mu - old_mu
            clamped_delta = max(-self.MAX_MU_SWING_PER_GAME, min(self.MAX_MU_SWING_PER_GAME, delta))
            clamped_mu = old_mu + clamped_delta

            # Enforce floor
            clamped_mu = max(self.MIN_MU, clamped_mu)

            fantasy_used = team2_data[i][3]
            results[discord_id] = (clamped_mu, rating.sigma, fantasy_used)

        return results

    def update_ratings_equal_weight(
        self,
        team1_data: list[tuple[int, float | None, float | None]],
        team2_data: list[tuple[int, float | None, float | None]],
        winning_team: int,
    ) -> dict[int, tuple[float, float]]:
        """
        Update ratings using Plackett-Luce with equal weights (1.0) for all players.

        This is used for Phase 1 (immediate) updates when fantasy data is not yet available.

        Args:
            team1_data: List of (discord_id, mu, sigma) for Radiant
            team2_data: List of (discord_id, mu, sigma) for Dire
            winning_team: 1 for Radiant, 2 for Dire

        Returns:
            Dict mapping discord_id -> (new_mu, new_sigma)
        """
        # Convert to the format expected by update_ratings_after_match
        # by adding None for fantasy_points
        team1_with_weights = [(pid, mu, sigma, None) for pid, mu, sigma in team1_data]
        team2_with_weights = [(pid, mu, sigma, None) for pid, mu, sigma in team2_data]

        results = self.update_ratings_after_match(
            team1_with_weights, team2_with_weights, winning_team
        )

        # Return only (mu, sigma), dropping the fantasy_weight field
        return {pid: (mu, sigma) for pid, (mu, sigma, _) in results.items()}

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

        Maps mu from OpenSkill range (~25-65) to display range (~0-3000).
        Formula: (mu - 25) * 75 with minimum of 0.

        With fantasy-weighted Plackett-Luce:
        - New players start at mu=25 → display=0
        - Average active players ~mu=45 → display=1500
        - Top players ~mu=65 → display=3000

        Args:
            mu: Skill estimate

        Returns:
            Display rating (0-3000 range)
        """
        # OpenSkill mu with fantasy weights ranges from ~25 (new) to ~65 (top)
        # Map to 0-3000 for familiar MMR-like display and matchmaking compatibility
        display = max(0, (mu - 25) * 75)
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
