"""
Match orchestration: shuffling and recording.

This module is the thin composition layer: ``MatchService`` keeps the
constructor, the shared ``_load_glicko_player`` cross-cutting helper, and the
per-guild recording-lock state, and composes the focused mixins from the
``services.match`` package into the single public service object. The logic
lives in those mixins.
"""

import threading
from datetime import UTC, datetime

from domain.models.player import Player
from domain.services.team_balancing_service import TeamBalancingService
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from repositories.interfaces import IMatchRepository, IPairingsRepository, IPlayerRepository
from services.betting_service import BettingService
from services.match._common import logger
from services.match.queries_mixin import QueriesMixin
from services.match.rating_update_mixin import RatingUpdateMixin
from services.match.recording_mixin import RecordingMixin
from services.match.shuffle_pending_mixin import ShufflePendingMixin
from services.match.voting_correction_mixin import VotingCorrectionMixin
from services.match_state_service import MatchStateService
from services.match_voting_service import MatchVotingService
from shuffler import BalancedShuffler

# Public surface plus the module-level ``logger`` that the former monolithic
# ``services.match_service`` module exposed. ``logger`` is re-exported from
# ``services.match._common`` above so existing
# ``from services.match_service import ...`` imports keep resolving after the
# package split.
__all__ = ["MatchService", "logger"]


class MatchService(
    RecordingMixin,
    RatingUpdateMixin,
    VotingCorrectionMixin,
    ShufflePendingMixin,
    QueriesMixin,
):
    """Handles team shuffling, state tracking, and match recording.

    The orchestration logic is split across focused mixins in the
    ``services.match`` package; this class holds the constructor, the shared
    ``_load_glicko_player`` helper, and the per-guild recording-lock state, and
    composes the mixins into the single public service object.
    """

    MIN_NON_ADMIN_SUBMISSIONS = MatchVotingService.MIN_NON_ADMIN_SUBMISSIONS
    MIN_ABORT_SUBMISSIONS = MatchVotingService.MIN_ABORT_SUBMISSIONS

    def __init__(
        self,
        player_repo: IPlayerRepository,
        match_repo: IMatchRepository,
        *,
        use_glicko: bool = True,
        betting_service: BettingService | None = None,
        pairings_repo: IPairingsRepository | None = None,
        loan_service=None,
        soft_avoid_repo=None,
        package_deal_repo=None,
        state_service: MatchStateService | None = None,
        low_priority_repo=None,
    ):
        """
        Initialize MatchService with required repository dependencies.

        Args:
            player_repo: Repository for player data access
            match_repo: Repository for match data access
            use_glicko: Whether to use Glicko rating system
            betting_service: Optional betting service for wager handling
            pairings_repo: Optional repository for pairwise player statistics
            loan_service: Optional loan service for deferred repayment
            soft_avoid_repo: Optional repository for soft avoid feature
            package_deal_repo: Optional repository for package deal feature
            state_service: Optional state service (created if not provided)
        """
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.use_glicko = use_glicko
        self.rating_system = CamaRatingSystem()
        self.openskill_system = CamaOpenSkillSystem()
        self.shuffler = BalancedShuffler(use_glicko=use_glicko, consider_roles=True)
        self.team_balancing_service = TeamBalancingService(
            use_glicko=use_glicko,
            off_role_multiplier=self.shuffler.off_role_multiplier,
            off_role_flat_penalty=self.shuffler.off_role_flat_penalty,
            role_matchup_delta_weight=self.shuffler.role_matchup_delta_weight,
        )
        # State management delegated to MatchStateService
        self.state_service = state_service or MatchStateService(match_repo)
        # Voting management delegated to MatchVotingService
        self.voting_service = MatchVotingService(self.state_service)
        self.betting_service = betting_service
        self.pairings_repo = pairings_repo
        self.loan_service = loan_service
        self.soft_avoid_repo = soft_avoid_repo
        self.package_deal_repo = package_deal_repo
        self.low_priority_repo = low_priority_repo
        # Guard against concurrent finalizations per guild
        self._recording_lock = threading.Lock()
        # Track matches being recorded as (guild_id, pending_match_id) tuples
        # to allow concurrent recording of different matches in the same guild
        self._recording_in_progress: set[tuple[int, int | None]] = set()

    def _load_glicko_player(
        self, player_id: int, guild_id: int | None = None
    ) -> tuple[Player, int]:
        rating_data = self.player_repo.get_glicko_rating(player_id, guild_id)
        last_dates = self.player_repo.get_last_match_date(player_id, guild_id)

        current_mmr = None
        if not rating_data:
            player_obj = self.player_repo.get_by_id(player_id, guild_id)
            current_mmr = player_obj.mmr if player_obj else None

        rating, rd, volatility = rating_data or (None, None, None)
        last_match_date, created_at = last_dates or (None, None)
        rating_input = {
            "current_mmr": current_mmr,
            "glicko_rating": rating,
            "glicko_rd": rd,
            "glicko_volatility": volatility,
            "last_match_date": last_match_date,
            "created_at": created_at,
        }
        return self._glicko_player_from_input(rating_input), player_id

    def _glicko_player_from_input(self, rating_input: dict | None) -> Player:
        """Build a decay-adjusted Glicko player from a preloaded repository row."""
        rating_input = rating_input or {}

        def _parse_dt(value):
            if not value:
                return None
            try:
                return datetime.fromisoformat(value)
            except Exception:
                return None

        last_match_dt = _parse_dt(rating_input.get("last_match_date"))
        created_at_dt = _parse_dt(rating_input.get("created_at"))

        rating = rating_input.get("glicko_rating")
        if rating is not None:
            base_player = self.rating_system.create_player_from_rating(
                rating,
                rating_input.get("glicko_rd"),
                rating_input.get("glicko_volatility"),
            )
        else:
            current_mmr = rating_input.get("current_mmr")
            base_player = self.rating_system.create_player_from_mmr(
                int(current_mmr) if current_mmr else None
            )

        # Apply RD decay if applicable
        reference_dt = last_match_dt or created_at_dt
        if reference_dt:
            now = datetime.now(UTC)
            if reference_dt.tzinfo is None:
                reference_dt = reference_dt.replace(tzinfo=UTC)
            days_since = (now - reference_dt).days
            base_player.rd = self.rating_system.apply_rd_decay(base_player.rd, days_since)

        return base_player

    def _openskill_rating_from_input(
        self,
        rating_input: dict | None,
    ) -> tuple[float, float]:
        """Build an inactivity-adjusted OpenSkill rating from a preload row."""
        rating_input = rating_input or {}
        mu = rating_input.get("os_mu")
        sigma = rating_input.get("os_sigma")
        if mu is None:
            source_mmr = rating_input.get("current_mmr")
            seed_mmr = self.rating_system.new_player_seed_mmr(
                int(source_mmr) if source_mmr is not None else 4000
            )
            mu = self.openskill_system.mmr_to_os_mu(seed_mmr)
        if sigma is None:
            sigma = self.openskill_system.DEFAULT_SIGMA

        reference_value = rating_input.get("last_match_date") or rating_input.get("created_at")
        if reference_value:
            try:
                reference_dt = datetime.fromisoformat(str(reference_value).replace("Z", "+00:00"))
            except (TypeError, ValueError):
                reference_dt = None
            if reference_dt is not None:
                if reference_dt.tzinfo is None:
                    reference_dt = reference_dt.replace(tzinfo=UTC)
                days_since = max(0, (datetime.now(UTC) - reference_dt).days)
                sigma = self.openskill_system.apply_sigma_decay(
                    float(sigma),
                    days_since,
                )

        return float(mu), float(sigma)
