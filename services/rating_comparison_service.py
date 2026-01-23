"""
Rating comparison service for analyzing Glicko-2 vs OpenSkill predictive power.
"""

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from repositories.match_repository import MatchRepository
    from repositories.player_repository import PlayerRepository
    from services.match_service import MatchService

logger = logging.getLogger("cama_bot.services.rating_comparison")


@dataclass
class RatingSystemStats:
    """Statistics for a single rating system."""

    name: str
    total_predictions: int
    brier_score: float  # Lower is better (0 = perfect, 0.25 = coin flip)
    accuracy: float  # Percentage of correct favorite predictions
    calibration_buckets: dict[str, dict]  # Binned prediction vs actual outcomes
    log_loss: float  # Log-likelihood loss


@dataclass
class ComparisonResult:
    """Result of comparing two rating systems."""

    glicko: RatingSystemStats
    openskill: RatingSystemStats
    matches_analyzed: int
    # Per-match data for charting
    match_data: list[dict]


class RatingComparisonService:
    """
    Analyzes and compares predictive power of Glicko-2 and OpenSkill rating systems.
    """

    def __init__(
        self,
        match_repo: "MatchRepository",
        player_repo: "PlayerRepository",
        match_service: "MatchService",
    ):
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.match_service = match_service

    def analyze_rating_systems(self) -> ComparisonResult | None:
        """
        Analyze Glicko-2 and OpenSkill predictions against actual match outcomes.

        Returns:
            ComparisonResult with statistics for both systems, or None if insufficient data
        """
        # Get all matches with Glicko-2 predictions
        matches = self.match_repo.get_all_matches_with_predictions()
        if not matches:
            logger.warning("No matches with predictions found")
            return None

        logger.info(f"Analyzing {len(matches)} matches with predictions")

        # Collect per-match prediction data
        match_data = []

        for match in matches:
            match_id = match["match_id"]
            winning_team = match["winning_team"]  # 1 = Radiant, 2 = Dire
            radiant_won = winning_team == 1

            # Glicko-2 prediction
            glicko_radiant_prob = match.get("expected_radiant_win_prob")
            if glicko_radiant_prob is None:
                continue

            # OpenSkill prediction (recalculate from current ratings)
            team1_ids = match.get("team1_players", [])
            team2_ids = match.get("team2_players", [])

            if not team1_ids or not team2_ids:
                continue

            os_prediction = self.match_service.get_openskill_predictions_for_match(
                team1_ids, team2_ids
            )
            os_radiant_prob = os_prediction.get("team1_win_prob", 0.5)

            match_data.append({
                "match_id": match_id,
                "match_date": match["match_date"],
                "radiant_won": radiant_won,
                "glicko_radiant_prob": glicko_radiant_prob,
                "openskill_radiant_prob": os_radiant_prob,
                "glicko_correct": (glicko_radiant_prob >= 0.5) == radiant_won,
                "openskill_correct": (os_radiant_prob >= 0.5) == radiant_won,
            })

        if len(match_data) < 10:
            logger.warning(f"Only {len(match_data)} matches with complete data")
            return None

        # Calculate statistics for each system
        glicko_stats = self._calculate_system_stats(
            "Glicko-2",
            match_data,
            prob_key="glicko_radiant_prob",
        )
        openskill_stats = self._calculate_system_stats(
            "OpenSkill",
            match_data,
            prob_key="openskill_radiant_prob",
        )

        return ComparisonResult(
            glicko=glicko_stats,
            openskill=openskill_stats,
            matches_analyzed=len(match_data),
            match_data=match_data,
        )

    def _calculate_system_stats(
        self, name: str, match_data: list[dict], prob_key: str
    ) -> RatingSystemStats:
        """Calculate statistics for a single rating system."""
        import math

        total = len(match_data)
        if total == 0:
            return RatingSystemStats(
                name=name,
                total_predictions=0,
                brier_score=0.25,
                accuracy=0.5,
                calibration_buckets={},
                log_loss=1.0,
            )

        # Brier score: mean of (prediction - outcome)^2
        brier_sum = 0.0
        correct_count = 0
        log_loss_sum = 0.0

        # Calibration buckets: group predictions by probability range
        buckets = {
            "0-10%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "10-20%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "20-30%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "30-40%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "40-50%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "50-60%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "60-70%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "70-80%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "80-90%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
            "90-100%": {"predicted": 0.0, "actual_wins": 0, "count": 0},
        }

        for m in match_data:
            prob = m[prob_key]
            outcome = 1.0 if m["radiant_won"] else 0.0

            # Brier score component
            brier_sum += (prob - outcome) ** 2

            # Accuracy: did favorite win?
            if (prob >= 0.5 and m["radiant_won"]) or (prob < 0.5 and not m["radiant_won"]):
                correct_count += 1

            # Log loss (clamp probability to avoid log(0))
            prob_clamped = max(0.001, min(0.999, prob))
            if m["radiant_won"]:
                log_loss_sum -= math.log(prob_clamped)
            else:
                log_loss_sum -= math.log(1 - prob_clamped)

            # Calibration bucket
            bucket_key = self._get_bucket_key(prob)
            buckets[bucket_key]["predicted"] += prob
            buckets[bucket_key]["actual_wins"] += 1 if m["radiant_won"] else 0
            buckets[bucket_key]["count"] += 1

        # Finalize bucket stats
        for key, bucket in buckets.items():
            if bucket["count"] > 0:
                bucket["avg_predicted"] = bucket["predicted"] / bucket["count"]
                bucket["actual_rate"] = bucket["actual_wins"] / bucket["count"]
            else:
                bucket["avg_predicted"] = 0.0
                bucket["actual_rate"] = 0.0

        return RatingSystemStats(
            name=name,
            total_predictions=total,
            brier_score=brier_sum / total,
            accuracy=correct_count / total,
            calibration_buckets=buckets,
            log_loss=log_loss_sum / total,
        )

    def _get_bucket_key(self, prob: float) -> str:
        """Get calibration bucket key for a probability."""
        if prob < 0.1:
            return "0-10%"
        elif prob < 0.2:
            return "10-20%"
        elif prob < 0.3:
            return "20-30%"
        elif prob < 0.4:
            return "30-40%"
        elif prob < 0.5:
            return "40-50%"
        elif prob < 0.6:
            return "50-60%"
        elif prob < 0.7:
            return "60-70%"
        elif prob < 0.8:
            return "70-80%"
        elif prob < 0.9:
            return "80-90%"
        else:
            return "90-100%"

    def get_comparison_summary(self) -> dict:
        """
        Get a summary of rating system comparison suitable for display.

        Returns:
            Dict with comparison metrics and analysis
        """
        result = self.analyze_rating_systems()
        if not result:
            return {
                "error": "Insufficient data for comparison",
                "matches_analyzed": 0,
            }

        glicko = result.glicko
        openskill = result.openskill

        # Determine which system is better by Brier score
        brier_winner = "Glicko-2" if glicko.brier_score < openskill.brier_score else "OpenSkill"
        brier_diff = abs(glicko.brier_score - openskill.brier_score)

        accuracy_winner = "Glicko-2" if glicko.accuracy > openskill.accuracy else "OpenSkill"
        accuracy_diff = abs(glicko.accuracy - openskill.accuracy)

        return {
            "matches_analyzed": result.matches_analyzed,
            "glicko": {
                "brier_score": glicko.brier_score,
                "accuracy": glicko.accuracy,
                "log_loss": glicko.log_loss,
                "calibration": glicko.calibration_buckets,
            },
            "openskill": {
                "brier_score": openskill.brier_score,
                "accuracy": openskill.accuracy,
                "log_loss": openskill.log_loss,
                "calibration": openskill.calibration_buckets,
            },
            "comparison": {
                "brier_winner": brier_winner,
                "brier_difference": brier_diff,
                "accuracy_winner": accuracy_winner,
                "accuracy_difference": accuracy_diff,
            },
            "match_data": result.match_data,  # For charting
        }

    def get_calibration_curve_data(self) -> dict:
        """
        Get data formatted for calibration curve plotting.

        Returns predicted probability bins vs actual win rates for both systems.
        """
        result = self.analyze_rating_systems()
        if not result:
            return {"error": "Insufficient data"}

        def extract_curve_data(buckets: dict) -> list[tuple[float, float, int]]:
            """Extract (predicted, actual, count) tuples for non-empty buckets."""
            data = []
            for key, bucket in buckets.items():
                if bucket["count"] > 0:
                    data.append((
                        bucket["avg_predicted"],
                        bucket["actual_rate"],
                        bucket["count"],
                    ))
            return sorted(data, key=lambda x: x[0])

        return {
            "glicko": extract_curve_data(result.glicko.calibration_buckets),
            "openskill": extract_curve_data(result.openskill.calibration_buckets),
            "perfect_line": [(0.0, 0.0), (1.0, 1.0)],  # Perfect calibration reference
        }
