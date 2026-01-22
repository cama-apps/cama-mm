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

    def _compute_team_delta(
        self,
        team_rating: float,
        team_rd: float,
        opponent_rating: float,
        opponent_rd: float,
        result: float,
    ) -> tuple[float, float, float]:
        """
        Compute the team-level rating delta using a synthetic player.

        Args:
            team_rating: Aggregate team rating
            team_rd: RD to use for the synthetic player (typically avg calibrated RD)
            opponent_rating: Opponent aggregate rating
            opponent_rd: Opponent aggregate RD
            result: 1.0 for win, 0.0 for loss

        Returns:
            Tuple of (new_rating, new_rd, new_vol) for the synthetic player
        """
        synth = Player(rating=team_rating, rd=team_rd, vol=self.initial_volatility)
        synth.update_player([opponent_rating], [opponent_rd], [result])
        return synth.rating, synth.rd, synth.vol

    def update_ratings_after_match(
        self,
        team1_players: list[tuple[Player, int]],  # (player, discord_id)
        team2_players: list[tuple[Player, int]],
        winning_team: int,
    ) -> tuple[list[tuple[float, float, float, int]], list[tuple[float, float, float, int]]]:
        """
        Update ratings after a match using RD²-weighted team delta.

        V2 approach (based on TrueSkill/Team Glicko research):
        1. Compute team-level Glicko-2 update (team vs team)
        2. Distribute team delta to players by RD² weight
        3. Update each player's RD using team-level opponent stats

        This fixes the old system's problems:
        - Low-rated high-RD players no longer get +600 swings (bounded by team delta * weight)
        - Win/loss is symmetric (~2:1 ratio instead of 46:1)
        - High-RD players still converge faster (larger share of team delta)

        Args:
            team1_players: List of (Glicko-2 Player, discord_id) for team 1
            team2_players: List of (Glicko-2 Player, discord_id) for team 2
            winning_team: 1 or 2 (which team won)

        Returns:
            Tuple of (team1_updated_ratings, team2_updated_ratings)
            Each rating is (rating, rd, volatility, discord_id)
        """
        # Aggregated team views (mean rating, RMS RD)
        team1_rating, team1_rd, _ = self.aggregate_team_stats([p for p, _ in team1_players])
        team2_rating, team2_rd, _ = self.aggregate_team_stats([p for p, _ in team2_players])

        team1_result = 1.0 if winning_team == 1 else 0.0
        team2_result = 1.0 if winning_team == 2 else 0.0

        # Compute team-level deltas using RMS RD for each team
        team1_new_rating, _, _ = self._compute_team_delta(
            team1_rating, team1_rd, team2_rating, team2_rd, team1_result
        )
        team2_new_rating, _, _ = self._compute_team_delta(
            team2_rating, team2_rd, team1_rating, team1_rd, team2_result
        )

        team1_delta = team1_new_rating - team1_rating
        team2_delta = team2_new_rating - team2_rating

        # Calculate RD² weights for each team
        team1_total_var = sum(p.rd**2 for p, _ in team1_players)
        team2_total_var = sum(p.rd**2 for p, _ in team2_players)

        # Avoid division by zero (shouldn't happen with valid RDs)
        if team1_total_var == 0:
            team1_total_var = 1.0
        if team2_total_var == 0:
            team2_total_var = 1.0

        team1_updated = []
        team2_updated = []

        # Update team 1 players
        for player, discord_id in team1_players:
            original_rating = player.rating
            original_rd = player.rd
            original_vol = player.vol

            # RD² weight determines share of team delta
            weight = (original_rd**2) / team1_total_var

            # Player delta = team_delta * weight * num_players
            # (multiply by 5 so mean rating moves by team_delta)
            player_delta = team1_delta * weight * len(team1_players)

            final_rating = max(0.0, original_rating + player_delta)

            # Update RD: use synthetic player with own RD vs team-level opponent
            # This ensures RD always decreases (more certain after playing)
            rd_synth = Player(rating=original_rating, rd=original_rd, vol=original_vol)
            rd_synth.update_player([team2_rating], [team2_rd], [team1_result])
            # RD should never increase after a match
            final_rd = min(original_rd, rd_synth.rd)
            final_vol = rd_synth.vol

            team1_updated.append((final_rating, final_rd, final_vol, discord_id))

        # Update team 2 players
        for player, discord_id in team2_players:
            original_rating = player.rating
            original_rd = player.rd
            original_vol = player.vol

            # RD² weight determines share of team delta
            weight = (original_rd**2) / team2_total_var

            # Player delta = team_delta * weight * num_players
            player_delta = team2_delta * weight * len(team2_players)

            final_rating = max(0.0, original_rating + player_delta)

            # Update RD: use synthetic player with own RD vs team-level opponent
            rd_synth = Player(rating=original_rating, rd=original_rd, vol=original_vol)
            rd_synth.update_player([team1_rating], [team1_rd], [team2_result])
            # RD should never increase after a match
            final_rd = min(original_rd, rd_synth.rd)
            final_vol = rd_synth.vol

            team2_updated.append((final_rating, final_rd, final_vol, discord_id))

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
