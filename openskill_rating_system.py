"""
OpenSkill Plackett-Luce rating system for fantasy-weighted team ratings.

This system runs parallel to Glicko-2, using fantasy points as weights
to reward individual performance within team context.

Key differences from Glicko-2:
- Uses mu (mean skill) and sigma (uncertainty) instead of rating/rd
- Rates complete teams natively
- Uses bounded fantasy-performance contribution weights inside the native
  Plackett-Luce posterior
"""

from __future__ import annotations

import hashlib
import json
import logging
import math
from importlib.metadata import version as package_version
from typing import TYPE_CHECKING

from openskill.models import PlackettLuce

from config import (
    OPENSKILL_CALIBRATION_SIGMA_THRESHOLD,
    OPENSKILL_PERFORMANCE_STRENGTH,
    OPENSKILL_SIGMA_DECAY_GRACE_PERIOD_DAYS,
    OPENSKILL_SIGMA_DECAY_PER_WEEK,
    OPENSKILL_WIN_PROBABILITY_TEMPERATURE,
)
from domain.rating_constants import OPENSKILL_DISPLAY_SCALE, OPENSKILL_MIN_MU

if TYPE_CHECKING:
    from openskill.rate import Rating

logger = logging.getLogger("cama_bot.openskill")


class CamaOpenSkillSystem:
    """
    Manages native weighted OpenSkill Plackett-Luce ratings.

    Fantasy performance is converted into small contribution weights centered
    around 1.0 and passed directly to ``PlackettLuce.rate``. OpenSkill applies
    those weights to both mu and sigma: high-performing winners receive more
    credit, while high-performing losers receive less blame. No post-hoc
    rating cap or team-total conservation is imposed.
    """

    DEFAULT_MU = 25.0
    DEFAULT_SIGMA = 25.0 / 3.0  # ~8.333

    # Fantasy point bounds for normalization (typical observed range)
    FANTASY_MIN = 5.0
    FANTASY_MAX = 30.0

    # Calibration threshold (sigma below this = calibrated)
    CALIBRATION_THRESHOLD = OPENSKILL_CALIBRATION_SIGMA_THRESHOLD

    # Raw OpenSkill predict_win() is directionally useful for this league but
    # empirically too confident. This temperature softens probabilities toward
    # 50/50 without changing favorite direction.
    WIN_PROBABILITY_TEMPERATURE = OPENSKILL_WIN_PROBABILITY_TEMPERATURE
    WIN_PROBABILITY_EPSILON = 1e-12

    # Maximum contribution tilt around 1.0.
    PERFORMANCE_STRENGTH = OPENSKILL_PERFORMANCE_STRENGTH
    # Backwards-compatible alias for callers/tests that used the old name.
    FP_WEIGHT_BLEND = PERFORMANCE_STRENGTH

    SIGMA_DECAY_GRACE_PERIOD_DAYS = OPENSKILL_SIGMA_DECAY_GRACE_PERIOD_DAYS
    SIGMA_DECAY_PER_WEEK = OPENSKILL_SIGMA_DECAY_PER_WEEK

    # Display constants. Canonical values (with full rationale) live in
    # domain.rating_constants.
    MIN_MU = OPENSKILL_MIN_MU  # display origin; model mu is not floored here
    DISPLAY_SCALE = OPENSKILL_DISPLAY_SCALE  # (mu - MIN_MU) * scale = display

    @classmethod
    def algorithm_spec(cls) -> dict[str, object]:
        """Return every setting that can change persisted OpenSkill results."""
        return {
            "family": "weng_lin.plackett_luce",
            "openskill_package": package_version("openskill"),
            "mu": cls.DEFAULT_MU,
            "sigma": cls.DEFAULT_SIGMA,
            "beta": cls.DEFAULT_MU / 6.0,
            "kappa": 0.0001,
            "tau": cls.DEFAULT_MU / 300.0,
            "limit_sigma": False,
            "balance": False,
            "weight_bounds": None,
            "fantasy_min": cls.FANTASY_MIN,
            "fantasy_max": cls.FANTASY_MAX,
            "performance_strength": cls.PERFORMANCE_STRENGTH,
            "sigma_decay_grace_days": cls.SIGMA_DECAY_GRACE_PERIOD_DAYS,
            "sigma_decay_per_week": cls.SIGMA_DECAY_PER_WEEK,
            "win_probability_temperature": cls.WIN_PROBABILITY_TEMPERATURE,
            "display_scale": cls.DISPLAY_SCALE,
            "display_origin": cls.MIN_MU,
        }

    @classmethod
    def algorithm_fingerprint(cls) -> str:
        """Stable identifier for the exact configured algorithm."""
        encoded = json.dumps(
            cls.algorithm_spec(),
            sort_keys=True,
            separators=(",", ":"),
        ).encode()
        return hashlib.sha256(encoded).hexdigest()[:16]

    def __init__(self):
        if not 0.0 <= self.PERFORMANCE_STRENGTH <= 1.0:
            raise ValueError(
                "OPENSKILL_PERFORMANCE_STRENGTH must be between 0 and 1"
            )
        if self.SIGMA_DECAY_PER_WEEK < 0:
            raise ValueError(
                "OPENSKILL_SIGMA_DECAY_PER_WEEK cannot be negative"
            )
        self.model = PlackettLuce(
            mu=self.DEFAULT_MU,
            sigma=self.DEFAULT_SIGMA,
        )
        # OpenSkill otherwise min/max-normalizes every team's weights to its
        # default (1, 2) range, turning even a one-point fantasy difference
        # into the maximum contribution gap. The installed extension rejects
        # None in the constructor even though the public API documents it, so
        # disable normalization on the model attribute after construction.
        self.model.weight_bounds = None

    def normalize_fantasy_score(self, fantasy_points: float) -> float:
        """Map an absolute fantasy score from the configured range to [0, 1]."""
        clamped = max(self.FANTASY_MIN, min(self.FANTASY_MAX, fantasy_points))
        return (clamped - self.FANTASY_MIN) / (self.FANTASY_MAX - self.FANTASY_MIN)

    @classmethod
    def apply_sigma_decay(cls, sigma: float, days_since_last_match: int) -> float:
        """Increase uncertainty after inactivity, capped at new-player sigma."""
        if sigma >= cls.DEFAULT_SIGMA:
            return cls.DEFAULT_SIGMA
        if days_since_last_match <= cls.SIGMA_DECAY_GRACE_PERIOD_DAYS:
            return sigma
        periods = (days_since_last_match - cls.SIGMA_DECAY_GRACE_PERIOD_DAYS) / 7.0
        decayed = math.sqrt(sigma * sigma + cls.SIGMA_DECAY_PER_WEEK**2 * periods)
        return min(cls.DEFAULT_SIGMA, decayed)

    def _performance_factors(
        self,
        fantasy_points: list[float],
    ) -> list[float]:
        """Return native contribution weights centered on this team's mean."""
        normalized = [self.normalize_fantasy_score(fp) for fp in fantasy_points]
        team_mean = sum(normalized) / len(normalized)
        return [
            1.0 + self.PERFORMANCE_STRENGTH * (score - team_mean)
            for score in normalized
        ]

    def mmr_to_os_mu(self, mmr: int) -> float:
        """
        Convert OpenDota MMR to OpenSkill mu.

        Maps MMR to mu such that the display rating lands on the same 0-3000
        scale Glicko-2 uses:
        - 0 MMR → mu=25 → display 0
        - 4000 MMR → mu=45 → display 1000
        - 8000 MMR → mu=65 → display 2000
        - 12000 MMR → mu=85 → display 3000

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
        Update ratings through OpenSkill's native weighted posterior.

        Fantasy is used only when all ten players have scores. Contribution
        weights remain close to 1.0 and are passed without OpenSkill's default
        per-team min/max normalization.

        Args:
            team1_data: List of (discord_id, mu, sigma, fantasy_points) for Radiant
            team2_data: List of (discord_id, mu, sigma, fantasy_points) for Dire
            winning_team: 1 for Radiant, 2 for Dire

        Returns:
            Dict mapping discord_id -> (new_mu, new_sigma, contribution_weight)
        """
        if winning_team not in (1, 2):
            raise ValueError("winning_team must be 1 or 2")
        if not team1_data or not team2_data:
            raise ValueError("both teams must contain at least one player")

        # Create ratings for each player.
        team1_ratings = []
        team1_ids = []

        for discord_id, mu, sigma, _ in team1_data:
            rating = self.create_rating(mu, sigma, name=str(discord_id))
            team1_ratings.append(rating)
            team1_ids.append(discord_id)

        team2_ratings = []
        team2_ids = []

        for discord_id, mu, sigma, _ in team2_data:
            rating = self.create_rating(mu, sigma, name=str(discord_id))
            team2_ratings.append(rating)
            team2_ids.append(discord_id)

        # Set ranks based on winning team (1 = winner, 2 = loser)
        if winning_team == 1:
            ranks = [1, 2]  # Team 1 won
        else:
            ranks = [2, 1]  # Team 2 won

        teams = [team1_ratings, team2_ratings]
        all_fantasy = [fp for _, _, _, fp in team1_data + team2_data]
        complete_fantasy = all(fp is not None for fp in all_fantasy)
        if complete_fantasy:
            team1_factors = self._performance_factors(
                [float(fp) for _, _, _, fp in team1_data if fp is not None]
            )
            team2_factors = self._performance_factors(
                [float(fp) for _, _, _, fp in team2_data if fp is not None]
            )
            weights = [team1_factors, team2_factors]
        else:
            team1_factors = [None] * len(team1_data)
            team2_factors = [None] * len(team2_data)
            weights = None

        try:
            updated_teams = self.model.rate(
                teams,
                ranks=ranks,
                weights=weights,
            )
        except Exception as e:
            logger.error(f"OpenSkill rate() failed: {e}")
            raise

        results: dict[int, tuple[float, float, float | None]] = {}
        for discord_id, rating, factor in zip(
            team1_ids,
            updated_teams[0],
            team1_factors,
        ):
            results[discord_id] = (rating.mu, rating.sigma, factor)
        for discord_id, rating, factor in zip(
            team2_ids,
            updated_teams[1],
            team2_factors,
        ):
            results[discord_id] = (rating.mu, rating.sigma, factor)

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

    @classmethod
    def mu_to_display(cls, mu: float) -> int:
        """
        Convert mu to a display rating scaled to match Glicko-2's 0-3000 range.

        Maps mu from OpenSkill range (25-85) to display range (0-3000), which is
        the same scale Glicko-2 uses (see ``rating_system.CamaRatingSystem.RATING_MAX``).
        Keeping both systems on an identical scale prevents OpenSkill-weighted
        players from being overvalued in mixed team-value calculations.

        Formula: ``(mu - MIN_MU) * DISPLAY_SCALE`` with a minimum of 0.

        With ``mmr_to_os_mu(mmr) = 25 + mmr/200`` and fantasy-weighted
        Plackett-Luce updates:
        - New players start at mu=25 → display=0 (MMR 0)
        - Average active players ~mu=45 → display=1000 (MMR 4000)
        - Strong players ~mu=65 → display=2000 (MMR 8000)
        - Top-end ceiling ~mu=85 → display=3000 (MMR 12000)

        Args:
            mu: Skill estimate

        Returns:
            Display rating (0-3000 range)
        """
        display = max(0.0, (mu - cls.MIN_MU) * cls.DISPLAY_SCALE)
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

    @classmethod
    def calibrate_win_probability(
        cls, probability: float, temperature: float | None = None
    ) -> float:
        """Apply post-hoc temperature scaling to raw OpenSkill win probability."""
        actual_temperature = cls.WIN_PROBABILITY_TEMPERATURE if temperature is None else temperature
        if actual_temperature <= 0:
            raise ValueError("OpenSkill probability temperature must be positive")

        p = max(
            cls.WIN_PROBABILITY_EPSILON,
            min(1.0 - cls.WIN_PROBABILITY_EPSILON, probability),
        )
        log_odds = math.log(p / (1.0 - p))
        scaled_log_odds = log_odds / actual_temperature
        if scaled_log_odds >= 0:
            exp_neg = math.exp(-scaled_log_odds)
            return 1.0 / (1.0 + exp_neg)
        exp_pos = math.exp(scaled_log_odds)
        return exp_pos / (1.0 + exp_pos)

    def os_predict_calibrated_win_probability(
        self,
        team1_ratings: list[tuple[float, float]],
        team2_ratings: list[tuple[float, float]],
    ) -> float:
        """Predict team1 win probability with league-calibrated confidence."""
        return self.calibrate_win_probability(
            self.os_predict_win_probability(team1_ratings, team2_ratings)
        )
