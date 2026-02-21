"""
Match orchestration: shuffling and recording.
"""

import random
import threading
import time
from datetime import datetime, timezone
from typing import Any

import logging

from config import BET_LOCK_SECONDS, CALIBRATION_RD_THRESHOLD
from domain.models.player import Player
from domain.models.team import Team
from domain.services.team_balancing_service import TeamBalancingService
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from repositories.interfaces import IMatchRepository, IPairingsRepository, IPlayerRepository
from services.betting_service import BettingService
from services.match_state_service import MatchStateService
from services.match_voting_service import MatchVotingService
from shuffler import BalancedShuffler

logger = logging.getLogger("cama_bot.services.match")


class MatchService:
    """Handles team shuffling, state tracking, and match recording."""

    MIN_NON_ADMIN_SUBMISSIONS = 3

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
        # Guard against concurrent finalizations per guild
        self._recording_lock = threading.Lock()
        # Track matches being recorded as (guild_id, pending_match_id) tuples
        # to allow concurrent recording of different matches in the same guild
        self._recording_in_progress: set[tuple[int, int | None]] = set()

    def _map_player_ids(self, player_ids: list[int], players: list[Player]) -> dict[int, int]:
        """Map Player object identity (id()) to Discord ID for stable lookups."""
        return {id(pl): pid for pid, pl in zip(player_ids, players)}

    def _resolve_team_ids(self, team: Team, player_id_map: dict[int, int]) -> list[int]:
        """Resolve Team players to Discord IDs using object identity."""
        return [player_id_map[id(p)] for p in team.players]

    # ==================== State Management (delegated to MatchStateService) ====================

    def _normalize_guild_id(self, guild_id: int | None) -> int:
        """Normalize guild_id (delegates to state_service)."""
        return self.state_service._normalize_guild_id(guild_id)

    def get_last_shuffle(self, guild_id: int | None = None, pending_match_id: int | None = None) -> dict | None:
        """Get the pending shuffle state (delegates to state_service)."""
        return self.state_service.get_last_shuffle(guild_id, pending_match_id)

    def set_last_shuffle(self, guild_id: int | None, payload: dict) -> None:
        """Set the pending shuffle state (delegates to state_service)."""
        self.state_service.set_last_shuffle(guild_id, payload)

    def set_shuffle_message_url(self, guild_id: int | None, jump_url: str) -> None:
        """Store the message link for the current pending shuffle (delegates to state_service)."""
        self.state_service.set_shuffle_message_url(guild_id, jump_url)

    def set_shuffle_message_info(
        self,
        guild_id: int | None,
        message_id: int | None,
        channel_id: int | None,
        jump_url: str | None = None,
        thread_message_id: int | None = None,
        thread_id: int | None = None,
        origin_channel_id: int | None = None,
        pending_match_id: int | None = None,
    ) -> None:
        """Store message metadata for the pending shuffle (delegates to state_service)."""
        self.state_service.set_shuffle_message_info(
            guild_id, message_id, channel_id, jump_url,
            thread_message_id, thread_id, origin_channel_id, pending_match_id
        )

    def get_shuffle_message_info(self, guild_id: int | None, pending_match_id: int | None = None) -> dict[str, int | None]:
        """Return message metadata for the pending shuffle (delegates to state_service)."""
        return self.state_service.get_shuffle_message_info(guild_id, pending_match_id)

    def clear_last_shuffle(self, guild_id: int | None, pending_match_id: int | None = None) -> None:
        """Clear the pending shuffle state (delegates to state_service)."""
        self.state_service.clear_last_shuffle(guild_id, pending_match_id)

    def _ensure_pending_state(self, guild_id: int | None) -> dict:
        """Get the pending state, raising error if none exists (delegates to state_service)."""
        return self.state_service.ensure_pending_state(guild_id)

    def _ensure_record_submissions(self, state: dict) -> dict[int, dict[str, Any]]:
        """Ensure record_submissions dict exists in state (delegates to state_service)."""
        return self.state_service.ensure_record_submissions(state)

    def _build_pending_match_payload(self, state: dict) -> dict:
        """Build payload for database persistence (delegates to state_service)."""
        return self.state_service.build_pending_match_payload(state)

    def _persist_match_state(self, guild_id: int | None, state: dict) -> None:
        """Persist the pending match state (delegates to state_service)."""
        self.state_service.persist_state(guild_id, state)

    # ==================== Voting Management (delegated to MatchVotingService) ====================

    def has_admin_submission(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if an admin has submitted a result vote (delegates to voting_service)."""
        return self.voting_service.has_admin_submission(guild_id, pending_match_id)

    def has_admin_abort_submission(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if an admin has submitted an abort vote (delegates to voting_service)."""
        return self.voting_service.has_admin_abort_submission(guild_id, pending_match_id)

    def add_record_submission(
        self, guild_id: int | None, user_id: int, result: str, is_admin: bool,
        pending_match_id: int | None = None
    ) -> dict[str, Any]:
        """Add a vote for the match result (delegates to voting_service)."""
        return self.voting_service.add_record_submission(guild_id, user_id, result, is_admin, pending_match_id)

    def get_non_admin_submission_count(self, guild_id: int | None, pending_match_id: int | None = None) -> int:
        """Get count of non-admin result votes (delegates to voting_service)."""
        return self.voting_service.get_non_admin_submission_count(guild_id, pending_match_id)

    def get_abort_submission_count(self, guild_id: int | None, pending_match_id: int | None = None) -> int:
        """Get count of non-admin abort votes (delegates to voting_service)."""
        return self.voting_service.get_abort_submission_count(guild_id, pending_match_id)

    def can_abort_match(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if there are enough votes to abort (delegates to voting_service)."""
        return self.voting_service.can_abort_match(guild_id, pending_match_id)

    def add_abort_submission(
        self, guild_id: int | None, user_id: int, is_admin: bool,
        pending_match_id: int | None = None
    ) -> dict[str, Any]:
        """Add a vote to abort the match (delegates to voting_service)."""
        return self.voting_service.add_abort_submission(guild_id, user_id, is_admin, pending_match_id)

    def get_vote_counts(self, guild_id: int | None, pending_match_id: int | None = None) -> dict[str, int]:
        """Get vote counts for radiant and dire (delegates to voting_service)."""
        return self.voting_service.get_vote_counts(guild_id, pending_match_id)

    def get_pending_record_result(self, guild_id: int | None, pending_match_id: int | None = None) -> str | None:
        """Get the result to record if threshold met (delegates to voting_service)."""
        return self.voting_service.get_pending_record_result(guild_id, pending_match_id)

    def can_record_match(self, guild_id: int | None, pending_match_id: int | None = None) -> bool:
        """Check if there are enough votes to record (delegates to voting_service)."""
        return self.voting_service.can_record_match(guild_id, pending_match_id)

    def shuffle_players(
        self,
        player_ids: list[int],
        guild_id: int | None = None,
        betting_mode: str = "pool",
        rating_system: str = "glicko",
    ) -> dict:
        """
        Shuffle players into balanced teams.

        Args:
            player_ids: List of Discord user IDs to shuffle
            guild_id: Guild ID for multi-guild support
            betting_mode: "pool" for parimutuel betting, "house" for 1:1 payouts
            rating_system: "glicko" or "openskill" - determines which rating system is used for balancing

        Returns a payload containing teams, role assignments, and Radiant/Dire mapping.
        """
        if betting_mode not in ("house", "pool"):
            raise ValueError("betting_mode must be 'house' or 'pool'")
        if rating_system not in ("glicko", "openskill", "jopacoin"):
            raise ValueError("rating_system must be 'glicko', 'openskill', or 'jopacoin'")
        players = self.player_repo.get_by_ids(player_ids, guild_id)
        if len(players) != len(player_ids):
            raise ValueError(
                f"Could not load all players: expected {len(player_ids)}, got {len(players)}"
            )

        # Apply RD decay for shuffle priority calculation (not persisted)
        # This ensures returning players get appropriate priority boost
        last_match_dates = self.player_repo.get_last_match_dates(player_ids, guild_id)
        now = datetime.now(timezone.utc)
        for player in players:
            if player.discord_id and player.glicko_rd is not None:
                last_match_str = last_match_dates.get(player.discord_id)
                if last_match_str:
                    try:
                        last_match = datetime.fromisoformat(last_match_str.replace("Z", "+00:00"))
                        days_since = (now - last_match).days
                        player.glicko_rd = CamaRatingSystem.apply_rd_decay(
                            player.glicko_rd, days_since
                        )
                    except (ValueError, TypeError):
                        pass  # Keep original RD if date parsing fails

        if len(players) < 10:
            raise ValueError("Need at least 10 players to shuffle.")

        # Cap to 14 for performance (C(14,10)=1001 stays within sampling limit)
        if len(players) > 14:
            players = players[:14]
            player_ids = player_ids[:14]

        exclusion_counts_by_id = self.player_repo.get_exclusion_counts(player_ids, guild_id)
        # Shuffler expects name->count mapping; this is internal to shuffler only
        exclusion_counts = {
            pl.name: exclusion_counts_by_id.get(pid, 0) for pid, pl in zip(player_ids, players)
        }

        # Get recent match participants and convert to player names
        recent_match_ids = self.match_repo.get_last_match_participant_ids(guild_id)
        recent_match_names = {
            p.name for p in players if p.discord_id in recent_match_ids
        }

        # Create a shuffler configured for the requested rating system
        use_openskill = rating_system == "openskill"
        use_jopacoin = rating_system == "jopacoin"
        shuffler = BalancedShuffler(
            use_glicko=self.use_glicko,
            use_openskill=use_openskill,
            use_jopacoin=use_jopacoin,
        )

        # Load active soft avoids for these players
        avoids = []
        if self.soft_avoid_repo:
            avoids = self.soft_avoid_repo.get_active_avoids_for_players(guild_id, player_ids)

        # Load active package deals for these players
        deals = []
        if self.package_deal_repo:
            deals = self.package_deal_repo.get_active_deals_for_players(guild_id, player_ids)

        if len(players) > 10:
            team1, team2, excluded_players = shuffler.shuffle_from_pool(
                players, exclusion_counts, recent_match_names, avoids=avoids, deals=deals
            )
        else:
            team1, team2 = shuffler.shuffle(players, avoids=avoids, deals=deals)
            excluded_players = []

        off_role_mult = shuffler.off_role_multiplier
        team1_value = team1.get_team_value(
            self.use_glicko, off_role_mult, use_openskill=use_openskill, use_jopacoin=use_jopacoin
        )
        team2_value = team2.get_team_value(
            self.use_glicko, off_role_mult, use_openskill=use_openskill, use_jopacoin=use_jopacoin
        )
        value_diff = abs(team1_value - team2_value)

        team1_off_roles = team1.get_off_role_count()
        team2_off_roles = team2.get_off_role_count()
        off_role_penalty = (team1_off_roles + team2_off_roles) * shuffler.off_role_flat_penalty
        role_matchup_delta = self.team_balancing_service.calculate_role_matchup_delta(team1, team2)
        weighted_role_matchup_delta = (
            role_matchup_delta * self.team_balancing_service.role_matchup_delta_weight
        )

        team1_roles = (
            team1.role_assignments if team1.role_assignments else team1._assign_roles_optimally()
        )
        team2_roles = (
            team2.role_assignments if team2.role_assignments else team2._assign_roles_optimally()
        )

        # Randomly assign Radiant/Dire
        if random.random() < 0.5:
            radiant_team = team1
            dire_team = team2
            radiant_roles = team1_roles
            dire_roles = team2_roles
            radiant_value = team1_value
            dire_value = team2_value
        else:
            radiant_team = team2
            dire_team = team1
            radiant_roles = team2_roles
            dire_roles = team1_roles
            radiant_value = team2_value
            dire_value = team1_value

        first_pick_team = random.choice(["Radiant", "Dire"])

        player_id_map = self._map_player_ids(player_ids, players)
        radiant_team_ids = self._resolve_team_ids(radiant_team, player_id_map)
        dire_team_ids = self._resolve_team_ids(dire_team, player_id_map)

        excluded_ids = []
        if excluded_players:
            excluded_ids = [
                player_id_map[id(p)] for p in excluded_players if id(p) in player_id_map
            ]

        excluded_penalty = 0.0
        if excluded_players:
            excluded_names = [p.name for p in excluded_players]
            exclusion_sum = sum(exclusion_counts.get(name, 0) for name in excluded_names)
            excluded_penalty = exclusion_sum * shuffler.exclusion_penalty_weight

        # Calculate recent match penalty for selected players
        recent_match_penalty = 0.0
        if recent_match_names:
            selected_names = {p.name for p in radiant_team.players + dire_team.players}
            recent_in_match = len(selected_names & recent_match_names)
            recent_match_penalty = recent_in_match * shuffler.recent_match_penalty_weight

        # Calculate soft avoid penalty (for display only - already factored into shuffler)
        soft_avoid_penalty = 0.0
        radiant_ids_set = {p.discord_id for p in radiant_team.players if p.discord_id}
        dire_ids_set = {p.discord_id for p in dire_team.players if p.discord_id}
        if avoids:
            for avoid in avoids:
                avoider = avoid.avoider_discord_id
                avoided = avoid.avoided_discord_id
                # Check if both on same team (penalty was applied)
                both_radiant = avoider in radiant_ids_set and avoided in radiant_ids_set
                both_dire = avoider in dire_ids_set and avoided in dire_ids_set
                if both_radiant or both_dire:
                    soft_avoid_penalty += shuffler.soft_avoid_penalty

        # Calculate package deal penalty (for display only - already factored into shuffler)
        package_deal_penalty = 0.0
        if deals:
            for deal in deals:
                buyer = deal.buyer_discord_id
                partner = deal.partner_discord_id
                # Check if on OPPOSITE teams (penalty was applied)
                on_opposite = (
                    (buyer in radiant_ids_set and partner in dire_ids_set) or
                    (buyer in dire_ids_set and partner in radiant_ids_set)
                )
                if on_opposite:
                    package_deal_penalty += shuffler.package_deal_penalty

        goodness_score = (
            value_diff + off_role_penalty + weighted_role_matchup_delta + excluded_penalty + recent_match_penalty + soft_avoid_penalty + package_deal_penalty
        )

        # Calculate Glicko-2 win probability for Radiant
        radiant_glicko_rating, radiant_glicko_rd, _ = self.rating_system.aggregate_team_stats(
            [
                self.rating_system.create_player_from_rating(
                    p.glicko_rating or self.rating_system.mmr_to_rating(p.mmr or 4000),
                    p.glicko_rd or 350.0,
                    p.glicko_volatility or 0.06,
                )
                for p in radiant_team.players
            ]
        )
        dire_glicko_rating, dire_glicko_rd, _ = self.rating_system.aggregate_team_stats(
            [
                self.rating_system.create_player_from_rating(
                    p.glicko_rating or self.rating_system.mmr_to_rating(p.mmr or 4000),
                    p.glicko_rd or 350.0,
                    p.glicko_volatility or 0.06,
                )
                for p in dire_team.players
            ]
        )
        glicko_radiant_win_prob = self.rating_system.expected_outcome(
            radiant_glicko_rating, radiant_glicko_rd, dire_glicko_rating, dire_glicko_rd
        )

        # Calculate OpenSkill win probability for Radiant
        radiant_os_ratings = [
            (p.os_mu or self.openskill_system.DEFAULT_MU, p.os_sigma or self.openskill_system.DEFAULT_SIGMA)
            for p in radiant_team.players
        ]
        dire_os_ratings = [
            (p.os_mu or self.openskill_system.DEFAULT_MU, p.os_sigma or self.openskill_system.DEFAULT_SIGMA)
            for p in dire_team.players
        ]
        openskill_radiant_win_prob = self.openskill_system.os_predict_win_probability(
            radiant_os_ratings, dire_os_ratings
        )

        # Update exclusion counts
        included_player_ids = set(radiant_team_ids + dire_team_ids)
        for pid in excluded_ids:
            self.player_repo.increment_exclusion_count(pid, guild_id)
        for pid in included_player_ids:
            self.player_repo.decay_exclusion_count(pid, guild_id)

        # Calculate effective soft avoids (opposite teams) - will be decremented on record_match
        effective_avoid_ids = []
        if self.soft_avoid_repo and avoids:
            radiant_set = set(radiant_team_ids)
            dire_set = set(dire_team_ids)
            for avoid in avoids:
                avoider = avoid.avoider_discord_id
                avoided = avoid.avoided_discord_id
                # Both must be in the match (not excluded)
                if avoider not in included_player_ids or avoided not in included_player_ids:
                    continue
                # They must be on opposite teams (avoid "worked")
                on_opposite = (
                    (avoider in radiant_set and avoided in dire_set) or
                    (avoider in dire_set and avoided in radiant_set)
                )
                if on_opposite:
                    effective_avoid_ids.append(avoid.id)

        # Calculate effective package deals (same team) - will be decremented on record_match
        effective_deal_ids = []
        if self.package_deal_repo and deals:
            radiant_set = set(radiant_team_ids)
            dire_set = set(dire_team_ids)
            for deal in deals:
                buyer = deal.buyer_discord_id
                partner = deal.partner_discord_id
                # Both must be in the match (not excluded)
                if buyer not in included_player_ids or partner not in included_player_ids:
                    continue
                # They must be on the SAME team (deal "worked")
                both_radiant = buyer in radiant_set and partner in radiant_set
                both_dire = buyer in dire_set and partner in dire_set
                if both_radiant or both_dire:
                    effective_deal_ids.append(deal.id)

        # Persist last shuffle for recording
        now_ts = int(time.time())
        shuffle_state = {
            "radiant_team_ids": radiant_team_ids,
            "dire_team_ids": dire_team_ids,
            "excluded_player_ids": excluded_ids,
            "radiant_team": radiant_team,
            "dire_team": dire_team,
            "radiant_roles": radiant_roles,
            "dire_roles": dire_roles,
            "radiant_value": radiant_value,
            "dire_value": dire_value,
            "value_diff": value_diff,
            "first_pick_team": first_pick_team,
            "record_submissions": {},
            "shuffle_timestamp": now_ts,
            "bet_lock_until": now_ts + BET_LOCK_SECONDS,
            "shuffle_message_jump_url": None,
            "shuffle_message_id": None,
            "shuffle_channel_id": None,
            "betting_mode": betting_mode,
            "is_draft": False,
            "balancing_rating_system": rating_system,
            "effective_avoid_ids": effective_avoid_ids,  # Avoids to decrement on record
            "effective_deal_ids": effective_deal_ids,  # Package deals to decrement on record
        }
        self.set_last_shuffle(guild_id, shuffle_state)
        self._persist_match_state(guild_id, shuffle_state)

        return {
            "radiant_team": radiant_team,
            "dire_team": dire_team,
            "radiant_roles": radiant_roles,
            "dire_roles": dire_roles,
            "radiant_value": radiant_value,
            "dire_value": dire_value,
            "value_diff": value_diff,
            "goodness_score": goodness_score,
            "first_pick_team": first_pick_team,
            "excluded_ids": excluded_ids,
            "glicko_radiant_win_prob": glicko_radiant_win_prob,
            "openskill_radiant_win_prob": openskill_radiant_win_prob,
            "balancing_rating_system": rating_system,
            "pending_match_id": shuffle_state.get("pending_match_id"),
        }

    def _load_glicko_player(self, player_id: int, guild_id: int | None = None) -> tuple[Player, int]:
        rating_data = self.player_repo.get_glicko_rating(player_id, guild_id)
        last_dates = self.player_repo.get_last_match_date(player_id, guild_id)
        last_match_dt = None
        created_at_dt = None

        def _parse_dt(value):
            if not value:
                return None
            try:
                return datetime.fromisoformat(value)
            except Exception:
                return None

        if last_dates:
            last_match_dt = _parse_dt(last_dates[0])
            created_at_dt = _parse_dt(last_dates[1])

        base_player: Player
        if rating_data:
            rating, rd, vol = rating_data
            base_player = self.rating_system.create_player_from_rating(rating, rd, vol)
        else:
            player_obj = self.player_repo.get_by_id(player_id, guild_id)
            if player_obj and player_obj.mmr is not None:
                base_player = self.rating_system.create_player_from_mmr(player_obj.mmr)
            else:
                base_player = self.rating_system.create_player_from_mmr(None)

        # Apply RD decay if applicable
        reference_dt = last_match_dt or created_at_dt
        if reference_dt:
            now = datetime.now(timezone.utc)
            if reference_dt.tzinfo is None:
                reference_dt = reference_dt.replace(tzinfo=timezone.utc)
            days_since = (now - reference_dt).days
            base_player.rd = self.rating_system.apply_rd_decay(base_player.rd, days_since)

        return base_player, player_id

    def record_match(
        self,
        winning_team: str,
        guild_id: int | None = None,
        dotabuff_match_id: str | None = None,
        pending_match_id: int | None = None,
    ) -> dict:
        """
        Record a match result and update ratings.

        winning_team: 'radiant' or 'dire'
        pending_match_id: Optional specific match ID for concurrent match support.
                         If None and only one pending match exists, uses that one.

        Thread-safe: prevents concurrent finalization for the same match.
        """
        normalized_gid = self._normalize_guild_id(guild_id)

        # Get state first to determine the actual pending_match_id
        last_shuffle = self.get_last_shuffle(guild_id, pending_match_id)
        if not last_shuffle:
            raise ValueError("No recent shuffle found.")
        # Get the actual pending_match_id from the state (in case it wasn't provided)
        pending_match_id = last_shuffle.get("pending_match_id")

        # Create lock key as (guild_id, pending_match_id) tuple
        # This allows concurrent recording of different matches in the same guild
        lock_key = (normalized_gid, pending_match_id)

        # Acquire exclusive recording right for this specific match
        with self._recording_lock:
            if lock_key in self._recording_in_progress:
                match_note = f" (Match #{pending_match_id})" if pending_match_id else ""
                raise ValueError(f"Match recording already in progress{match_note}.")
            self._recording_in_progress.add(lock_key)

        try:

            if winning_team not in ("radiant", "dire"):
                raise ValueError("winning_team must be 'radiant' or 'dire'.")

            radiant_team_ids = last_shuffle["radiant_team_ids"]
            dire_team_ids = last_shuffle["dire_team_ids"]
            excluded_player_ids = last_shuffle.get("excluded_player_ids", [])

            all_match_ids = set(radiant_team_ids + dire_team_ids)
            if set(excluded_player_ids).intersection(all_match_ids):
                raise ValueError("Excluded players detected in match teams.")

            # Map winners/losers for DB
            winning_ids = radiant_team_ids if winning_team == "radiant" else dire_team_ids
            losing_ids = dire_team_ids if winning_team == "radiant" else radiant_team_ids

            # Determine lobby type from pending state (draft sets is_draft=True)
            lobby_type = "draft" if last_shuffle.get("is_draft") else "shuffle"
            balancing_rating_system = last_shuffle.get("balancing_rating_system", "glicko")

            match_id = self.match_repo.record_match(
                team1_ids=radiant_team_ids,
                team2_ids=dire_team_ids,
                winning_team=1 if winning_team == "radiant" else 2,
                guild_id=guild_id,
                dotabuff_match_id=dotabuff_match_id,
                lobby_type=lobby_type,
                balancing_rating_system=balancing_rating_system,
            )

            # Persist win/loss counters for all players in the match (prefer single transaction).
            if hasattr(self.player_repo, "apply_match_outcome"):
                self.player_repo.apply_match_outcome(winning_ids, losing_ids, guild_id)  # type: ignore[attr-defined]
            else:
                for pid in winning_ids:
                    self.player_repo.increment_wins(pid, guild_id)
                for pid in losing_ids:
                    self.player_repo.increment_losses(pid, guild_id)

            distributions = {"winners": [], "losers": []}
            is_bomb_pot = last_shuffle.get("is_bomb_pot", False)
            if self.betting_service:
                # Reward participation for the losing team (1 JC base + bomb pot bonus if applicable)
                self.betting_service.award_participation(losing_ids, guild_id, is_bomb_pot=is_bomb_pot)

                # In bomb pot mode, winners ALSO get the bomb pot participation bonus (+1 JC)
                # This is on top of their win bonus, giving all 10 players the +1 JC bomb pot bonus
                # Use bomb_pot_bonus_only=True so they don't get the base participation (just the bonus)
                if is_bomb_pot:
                    self.betting_service.award_participation(
                        winning_ids, guild_id, is_bomb_pot=True, bomb_pot_bonus_only=True
                    )

                distributions = self.betting_service.settle_bets(
                    match_id, guild_id, winning_team, pending_state=last_shuffle
                )
                self.betting_service.award_win_bonus(winning_ids, guild_id)
                if excluded_player_ids:
                    self.betting_service.award_exclusion_bonus(excluded_player_ids, guild_id)
                # Award half exclusion bonus to conditional players who were excluded
                excluded_conditional_ids = last_shuffle.get("excluded_conditional_player_ids", [])
                if excluded_conditional_ids:
                    self.betting_service.award_exclusion_bonus_half(excluded_conditional_ids, guild_id)

            # Repay outstanding loans for all participants
            loan_repayments = []
            if self.loan_service:
                all_participant_ids = winning_ids + losing_ids
                for player_id in all_participant_ids:
                    state = self.loan_service.get_state(player_id, guild_id)
                    if state.has_outstanding_loan:
                        result = self.loan_service.repay_loan(player_id, guild_id)
                        if result.get("success"):
                            loan_repayments.append({
                                "player_id": player_id,
                                **result,
                            })

            # Build Glicko players
            radiant_glicko = [self._load_glicko_player(pid, guild_id) for pid in radiant_team_ids]
            dire_glicko = [self._load_glicko_player(pid, guild_id) for pid in dire_team_ids]

            # Calculate streak multipliers for each player
            all_player_ids_for_streak = radiant_team_ids + dire_team_ids
            streak_multipliers: dict[int, float] = {}
            streak_data: dict[int, tuple[int, float]] = {}  # pid -> (length, multiplier)

            for pid in all_player_ids_for_streak:
                won = (pid in radiant_team_ids and winning_team == "radiant") or \
                      (pid in dire_team_ids and winning_team == "dire")
                recent_outcomes = self.match_repo.get_player_recent_outcomes(pid, guild_id, limit=20)
                streak_length, multiplier = self.rating_system.calculate_streak_multiplier(
                    recent_outcomes, won=won
                )
                streak_multipliers[pid] = multiplier
                streak_data[pid] = (streak_length, multiplier)

            # Snapshot pre-match ratings for history + prediction stats
            pre_match = {}
            for player, pid in radiant_glicko:
                pre_match[pid] = {
                    "rating_before": player.rating,
                    "rd_before": player.rd,
                    "volatility_before": player.vol,
                    "team_number": 1,
                    "won": winning_team == "radiant",
                    "streak_length": streak_data.get(pid, (1, 1.0))[0],
                    "streak_multiplier": streak_data.get(pid, (1, 1.0))[1],
                }
            for player, pid in dire_glicko:
                pre_match[pid] = {
                    "rating_before": player.rating,
                    "rd_before": player.rd,
                    "volatility_before": player.vol,
                    "team_number": 2,
                    "won": winning_team == "dire",
                    "streak_length": streak_data.get(pid, (1, 1.0))[0],
                    "streak_multiplier": streak_data.get(pid, (1, 1.0))[1],
                }

            radiant_rating, radiant_rd, _ = self.rating_system.aggregate_team_stats(
                [p for p, _ in radiant_glicko]
            )
            dire_rating, dire_rd, _ = self.rating_system.aggregate_team_stats(
                [p for p, _ in dire_glicko]
            )
            expected_radiant_win_prob = self.rating_system.expected_outcome(
                radiant_rating, radiant_rd, dire_rating, dire_rd
            )
            expected_team_win_prob = {
                1: expected_radiant_win_prob,
                2: 1.0 - expected_radiant_win_prob,
            }

            if winning_team == "radiant":
                team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                    radiant_glicko, dire_glicko, 1, streak_multipliers=streak_multipliers
                )
            else:
                team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                    dire_glicko, radiant_glicko, 1, streak_multipliers=streak_multipliers
                )

            expected_ids = set(radiant_team_ids + dire_team_ids)
            # Even if mismatch, continue but skip unknown IDs when writing

            updated_count = 0
            updates = [
                (pid, rating, rd, vol)
                for rating, rd, vol, pid in team1_updated + team2_updated
                if pid in expected_ids
            ]
            if hasattr(self.player_repo, "update_glicko_ratings_bulk"):
                updated_count = self.player_repo.update_glicko_ratings_bulk(updates, guild_id)  # type: ignore[attr-defined]
            else:
                for pid, rating, rd, vol in updates:
                    self.player_repo.update_glicko_rating(pid, guild_id, rating, rd, vol)
                    updated_count += 1

            # Update last_match_date for participants
            now_iso = datetime.now(timezone.utc).isoformat()
            for pid in expected_ids:
                self.player_repo.update_last_match_date(pid, guild_id, now_iso)

            # Track first calibration for players who just became calibrated
            now_unix = int(time.time())
            for pid, rating, rd, vol in updates:
                if rd <= CALIBRATION_RD_THRESHOLD:
                    # Check if player doesn't have first_calibrated_at set yet
                    if hasattr(self.player_repo, "get_first_calibrated_at"):
                        first_cal = self.player_repo.get_first_calibrated_at(pid, guild_id)
                        if first_cal is None:
                            self.player_repo.set_first_calibrated_at(pid, guild_id, now_unix)

            # Store match prediction snapshot (pre-match)
            if hasattr(self.match_repo, "add_match_prediction"):
                self.match_repo.add_match_prediction(
                    match_id=match_id,
                    radiant_rating=radiant_rating,
                    dire_rating=dire_rating,
                    radiant_rd=radiant_rd,
                    dire_rd=dire_rd,
                    expected_radiant_win_prob=expected_radiant_win_prob,
                )

            # === Phase 1: OpenSkill update with equal weights ===
            # This runs immediately at match record to keep OpenSkill ratings fresh.
            # Phase 2 (update_openskill_ratings_for_match) will recalculate with
            # fantasy weights after enrichment, using the baseline stored here.
            all_player_ids = radiant_team_ids + dire_team_ids
            os_ratings = self.player_repo.get_openskill_ratings_bulk(all_player_ids, guild_id)

            radiant_os_data = [
                (pid, *os_ratings.get(pid, (None, None)))
                for pid in radiant_team_ids
            ]
            dire_os_data = [
                (pid, *os_ratings.get(pid, (None, None)))
                for pid in dire_team_ids
            ]

            os_results = self.openskill_system.update_ratings_equal_weight(
                radiant_os_data, dire_os_data,
                winning_team=1 if winning_team == "radiant" else 2
            )

            # Update player OpenSkill ratings immediately
            os_updates = [(pid, mu, sigma) for pid, (mu, sigma) in os_results.items()]
            self.player_repo.update_openskill_ratings_bulk(os_updates, guild_id)

            # Store os_* data in pre_match dict for rating_history
            DEFAULT_MU = CamaOpenSkillSystem.DEFAULT_MU
            DEFAULT_SIGMA = CamaOpenSkillSystem.DEFAULT_SIGMA
            for pid, (new_mu, new_sigma) in os_results.items():
                old_mu, old_sigma = os_ratings.get(pid, (None, None))
                if pid in pre_match:
                    pre_match[pid]["os_mu_before"] = old_mu if old_mu is not None else DEFAULT_MU
                    pre_match[pid]["os_mu_after"] = new_mu
                    pre_match[pid]["os_sigma_before"] = old_sigma if old_sigma is not None else DEFAULT_SIGMA
                    pre_match[pid]["os_sigma_after"] = new_sigma

            # Record rating history snapshots per player
            for pid, rating, rd, vol in updates:
                pre = pre_match.get(pid)
                if not pre:
                    continue
                self.match_repo.add_rating_history(
                    discord_id=pid,
                    guild_id=guild_id,
                    rating=rating,
                    match_id=match_id,
                    rating_before=pre["rating_before"],
                    rd_before=pre["rd_before"],
                    rd_after=rd,
                    volatility_before=pre["volatility_before"],
                    volatility_after=vol,
                    expected_team_win_prob=expected_team_win_prob.get(pre["team_number"]),
                    team_number=pre["team_number"],
                    won=pre["won"],
                    os_mu_before=pre.get("os_mu_before"),
                    os_mu_after=pre.get("os_mu_after"),
                    os_sigma_before=pre.get("os_sigma_before"),
                    os_sigma_after=pre.get("os_sigma_after"),
                    streak_length=pre.get("streak_length"),
                    streak_multiplier=pre.get("streak_multiplier"),
                )

            # Update pairwise player statistics
            if self.pairings_repo:
                self.pairings_repo.update_pairings_for_match(
                    match_id=match_id,
                    guild_id=guild_id,
                    team1_ids=radiant_team_ids,
                    team2_ids=dire_team_ids,
                    winning_team=1 if winning_team == "radiant" else 2,
                )

            # Decrement soft avoids that were effective (opposite teams)
            effective_avoid_ids = last_shuffle.get("effective_avoid_ids", [])
            if self.soft_avoid_repo and effective_avoid_ids:
                self.soft_avoid_repo.decrement_avoids(guild_id, effective_avoid_ids)

            # Decrement package deals that were effective (same team)
            effective_deal_ids = last_shuffle.get("effective_deal_ids", [])
            if self.package_deal_repo and effective_deal_ids:
                self.package_deal_repo.decrement_deals(guild_id, effective_deal_ids)

            # Find the most notable streak (longest, >=5 games) for neon hooks
            notable_streak = None
            for pid, (slen, _smult) in streak_data.items():
                if slen >= 5:
                    if notable_streak is None or slen > notable_streak["streak"]:
                        won = (pid in radiant_team_ids and winning_team == "radiant") or \
                              (pid in dire_team_ids and winning_team == "dire")
                        notable_streak = {
                            "discord_id": pid,
                            "streak": slen,
                            "is_win": won,
                        }

            # --- Easter Egg Data Collection ---
            easter_egg_data = {
                "games_milestones": [],
                "win_streak_records": [],
                "rivalries_detected": [],
            }

            # Check for rivalries (10+ games together with 70%+ winrate imbalance)
            if self.pairings_repo:
                try:
                    all_players = radiant_team_ids + dire_team_ids
                    for i, p1 in enumerate(all_players):
                        for p2 in all_players[i+1:]:
                            pairing = self.pairings_repo.get_pairing(p1, p2, guild_id)
                            if pairing and pairing.get("games_together", 0) >= 10:
                                wins = pairing.get("p1_wins", 0)
                                losses = pairing.get("p1_losses", 0)
                                total = wins + losses
                                if total >= 10:
                                    winrate = (wins / total) * 100
                                    if winrate >= 70 or winrate <= 30:
                                        easter_egg_data["rivalries_detected"].append({
                                            "player1_id": p1,
                                            "player2_id": p2,
                                            "games_together": total,
                                            "winrate_vs": winrate,
                                        })
                except Exception as e:
                    logger.debug(f"Rivalry detection error: {e}")

            # Check for games milestones (10, 50, 100, 200, 500)
            milestone_values = {10, 50, 100, 200, 500}
            for pid in expected_ids:
                player = self.player_repo.get_by_id(pid, guild_id)
                if player:
                    total_games = player.wins + player.losses
                    if total_games in milestone_values:
                        easter_egg_data["games_milestones"].append({
                            "discord_id": pid,
                            "total_games": total_games,
                        })

            # Check for personal best win streak records (for winners only)
            for pid in winning_ids:
                slen, _ = streak_data.get(pid, (1, 1.0))
                if slen >= 5:
                    # Get player's personal best
                    if hasattr(self.player_repo, "get_personal_best_win_streak"):
                        prev_best = self.player_repo.get_personal_best_win_streak(pid, guild_id)
                        if slen > prev_best:
                            # Update the record
                            self.player_repo.update_personal_best_win_streak(pid, guild_id, slen)
                            easter_egg_data["win_streak_records"].append({
                                "discord_id": pid,
                                "current_streak": slen,
                                "previous_best": prev_best,
                            })

            # Clear state after successful record (only this specific match)
            self.clear_last_shuffle(guild_id, pending_match_id)

            return {
                "match_id": match_id,
                "winning_team": winning_team,
                "updated_count": updated_count,
                "winning_player_ids": winning_ids,
                "losing_player_ids": losing_ids,
                "bet_distributions": distributions,
                "loan_repayments": loan_repayments,
                "notable_streak": notable_streak,
                "easter_egg_data": easter_egg_data,
            }
        finally:
            with self._recording_lock:
                self._recording_in_progress.discard(lock_key)

    def update_openskill_ratings_for_match(self, match_id: int, guild_id: int | None = None) -> dict:
        """
        Update OpenSkill ratings for a match using fantasy points as weights.

        This method should be called AFTER match enrichment when fantasy_points
        have been calculated and stored in match_participants.

        Args:
            match_id: The internal match ID to update ratings for
            guild_id: Guild ID for multi-guild support

        Returns:
            Dict with:
            - success: bool
            - players_updated: int
            - players_skipped: int (missing fantasy data)
            - error: str (if failed)
        """
        # Get match data
        match = self.match_repo.get_match(match_id, guild_id)
        if not match:
            return {
                "success": False,
                "error": f"Match {match_id} not found",
                "players_updated": 0,
                "players_skipped": 0,
            }

        winning_team = match.get("winning_team")  # 1 = Radiant, 2 = Dire
        if winning_team not in (1, 2):
            return {
                "success": False,
                "error": f"Invalid winning_team: {winning_team}",
                "players_updated": 0,
                "players_skipped": 0,
            }

        # Get participants with fantasy points
        participants = self.match_repo.get_match_participants(match_id, guild_id)
        if not participants:
            return {
                "success": False,
                "error": "No participants found for match",
                "players_updated": 0,
                "players_skipped": 0,
            }

        # Separate by team
        radiant = [p for p in participants if p.get("side") == "radiant"]
        dire = [p for p in participants if p.get("side") == "dire"]

        if len(radiant) != 5 or len(dire) != 5:
            logger.warning(
                f"Match {match_id}: unexpected team sizes radiant={len(radiant)}, dire={len(dire)}"
            )

        # Check if any participants have fantasy data
        has_fantasy = any(p.get("fantasy_points") is not None for p in participants)
        if not has_fantasy:
            logger.info(f"Match {match_id}: no fantasy data available, skipping OpenSkill update")
            return {
                "success": True,
                "players_updated": 0,
                "players_skipped": len(participants),
                "reason": "No fantasy data available",
            }

        # === Phase 2: Get baseline from rating_history (Phase 1 values) ===
        # This retrieves the pre-match OpenSkill ratings that were stored during
        # Phase 1, allowing us to recalculate with fantasy weights from the same
        # starting point.
        discord_ids = [p["discord_id"] for p in participants]
        os_baseline = self.match_repo.get_os_baseline_for_match(match_id)

        if os_baseline:
            # Use Phase 1 baseline (os_mu_before/os_sigma_before from rating_history)
            os_ratings = os_baseline
            logger.debug(f"Match {match_id}: using Phase 1 baseline for {len(os_baseline)} players")
        else:
            # Legacy fallback: use current player ratings (pre-Phase 1 matches)
            os_ratings = self.player_repo.get_openskill_ratings_bulk(discord_ids, guild_id)
            logger.debug(f"Match {match_id}: no Phase 1 baseline, using current ratings")

        # Build team data for OpenSkill update
        # Format: (discord_id, mu, sigma, fantasy_points)
        team1_data = []  # Radiant
        team2_data = []  # Dire

        for p in radiant:
            discord_id = p["discord_id"]
            mu, sigma = os_ratings.get(discord_id, (None, None))
            fantasy_points = p.get("fantasy_points")
            team1_data.append((discord_id, mu, sigma, fantasy_points))

        for p in dire:
            discord_id = p["discord_id"]
            mu, sigma = os_ratings.get(discord_id, (None, None))
            fantasy_points = p.get("fantasy_points")
            team2_data.append((discord_id, mu, sigma, fantasy_points))

        # Run OpenSkill update
        try:
            results = self.openskill_system.update_ratings_after_match(
                team1_data=team1_data,
                team2_data=team2_data,
                winning_team=winning_team,
            )
        except Exception as e:
            logger.error(f"OpenSkill update failed for match {match_id}: {e}")
            return {
                "success": False,
                "error": str(e),
                "players_updated": 0,
                "players_skipped": 0,
            }

        # Persist updated ratings
        updates = [(pid, mu, sigma) for pid, (mu, sigma, _) in results.items()]
        updated_count = self.player_repo.update_openskill_ratings_bulk(updates, guild_id)

        # Record in rating history (bulk update existing entries for this match)
        history_updates = []
        for pid, (new_mu, new_sigma, fantasy_weight) in results.items():
            old_mu, old_sigma = os_ratings.get(pid, (None, None))
            history_updates.append({
                "discord_id": pid,
                "os_mu_before": old_mu,
                "os_mu_after": new_mu,
                "os_sigma_before": old_sigma,
                "os_sigma_after": new_sigma,
                "fantasy_weight": fantasy_weight,
            })

        if history_updates:
            history_updated = self.match_repo.update_rating_history_openskill_bulk(
                match_id, history_updates
            )
            if history_updated < len(history_updates):
                logger.warning(
                    f"Only {history_updated}/{len(history_updates)} rating_history entries found for match {match_id}"
                )

        logger.info(
            f"OpenSkill update complete for match {match_id}: {updated_count} players updated"
        )

        return {
            "success": True,
            "players_updated": updated_count,
            "players_skipped": len(participants) - updated_count,
        }

    def _update_rating_history_openskill(
        self,
        match_id: int,
        discord_id: int,
        os_mu_before: float | None,
        os_mu_after: float,
        os_sigma_before: float | None,
        os_sigma_after: float,
        fantasy_weight: float | None,
    ) -> None:
        """
        Update an existing rating_history entry with OpenSkill data.

        If no existing entry exists, this is a no-op (OpenSkill updates happen
        after enrichment, so rating_history should already exist from record_match).
        """
        updated = self.match_repo.update_rating_history_openskill(
            match_id=match_id,
            discord_id=discord_id,
            os_mu_before=os_mu_before,
            os_mu_after=os_mu_after,
            os_sigma_before=os_sigma_before,
            os_sigma_after=os_sigma_after,
            fantasy_weight=fantasy_weight,
        )
        if not updated:
            logger.warning(
                f"No rating_history entry found for match {match_id}, player {discord_id}"
            )

    def backfill_openskill_ratings(self, guild_id: int | None = None, reset_first: bool = True) -> dict:
        """
        Backfill OpenSkill ratings from ALL matches.

        Processes matches in chronological order to simulate rating progression.
        - Enriched matches (with fantasy data): Use FP-weighted update with blending
        - Non-enriched matches: Use equal-weight update

        Args:
            reset_first: If True, reset all players' OpenSkill ratings to defaults before backfill

        Returns:
            Dict with:
            - matches_processed: int
            - matches_with_fantasy: int
            - matches_equal_weight: int
            - players_updated: int (unique players)
            - errors: list of error messages
        """
        logger.info("Starting OpenSkill backfill (all matches)...")

        errors = []
        matches_processed = 0
        matches_with_fantasy = 0
        matches_equal_weight = 0
        players_touched = set()

        # Get ALL matches in chronological order (not just enriched)
        normalized_guild = guild_id if guild_id is not None else 0
        all_matches = self.match_repo.get_all_matches_chronological(normalized_guild)
        total_matches = len(all_matches)
        logger.info(f"Found {total_matches} total matches to process")

        if total_matches == 0:
            return {
                "matches_processed": 0,
                "matches_with_fantasy": 0,
                "matches_equal_weight": 0,
                "players_updated": 0,
                "errors": ["No matches found"],
            }

        # Reset all players' OpenSkill ratings if requested
        # Seed from initial_mmr (what they started with), falling back to DEFAULT_MU
        if reset_first:
            all_players = self.player_repo.get_all(normalized_guild)
            reset_updates = []
            for p in all_players:
                if p.discord_id is None:
                    continue
                # Seed mu from initial_mmr (OpenDota MMR at registration)
                if p.initial_mmr is not None:
                    # Convert MMR to mu: mu = 25 + (mmr / 200)
                    seed_mu = self.openskill_system.mmr_to_os_mu(p.initial_mmr)
                else:
                    seed_mu = self.openskill_system.DEFAULT_MU
                reset_updates.append((p.discord_id, seed_mu, self.openskill_system.DEFAULT_SIGMA))
            if reset_updates:
                self.player_repo.update_openskill_ratings_bulk(reset_updates, normalized_guild)
                logger.info(f"Reset {len(reset_updates)} players to seeded OpenSkill ratings")

        # Process each match in chronological order
        for i, match in enumerate(all_matches):
            match_id = match["match_id"]
            winning_team = match["winning_team"]

            try:
                # Get participants to check for fantasy data
                participants = self.match_repo.get_match_participants(match_id, normalized_guild)

                if not participants:
                    # No participants recorded - use team lists from match
                    radiant_ids = match.get("team1_players", [])
                    dire_ids = match.get("team2_players", [])
                    if not radiant_ids or not dire_ids:
                        errors.append(f"Match {match_id}: No participant data")
                        continue
                    has_fantasy = False
                else:
                    radiant_ids = [p["discord_id"] for p in participants if p.get("side") == "radiant"]
                    dire_ids = [p["discord_id"] for p in participants if p.get("side") == "dire"]
                    has_fantasy = any(p.get("fantasy_points") is not None for p in participants)

                    # If no side info, fall back to match team lists and use equal weight
                    # (can't use fantasy weights without knowing which player was on which team)
                    if not radiant_ids or not dire_ids:
                        radiant_ids = match.get("team1_players", [])
                        dire_ids = match.get("team2_players", [])
                        has_fantasy = False

                # Get guild_id from match for per-guild updates
                match_guild_id = match.get("guild_id")

                if has_fantasy:
                    # Use FP-weighted update (with blending)
                    result = self._backfill_match_with_fantasy(match_id, match_guild_id, participants, winning_team)
                    if result.get("success"):
                        matches_with_fantasy += 1
                else:
                    # Use equal-weight update
                    result = self._backfill_match_equal_weight(
                        match_id, match_guild_id, radiant_ids, dire_ids, winning_team
                    )
                    if result.get("success"):
                        matches_equal_weight += 1

                if result.get("success"):
                    matches_processed += 1
                    for pid in radiant_ids + dire_ids:
                        players_touched.add(pid)
                else:
                    error = result.get("error", "Unknown error")
                    errors.append(f"Match {match_id}: {error}")

            except Exception as e:
                errors.append(f"Match {match_id}: {str(e)}")
                logger.error(f"Backfill error for match {match_id}: {e}")

            # Log progress periodically
            if (i + 1) % 50 == 0 or (i + 1) == total_matches:
                logger.info(f"Backfill progress: {i + 1}/{total_matches} matches processed")

        logger.info(
            f"OpenSkill backfill complete: {matches_processed} matches "
            f"({matches_with_fantasy} FP-weighted, {matches_equal_weight} equal-weight), "
            f"{len(players_touched)} unique players"
        )

        return {
            "matches_processed": matches_processed,
            "matches_with_fantasy": matches_with_fantasy,
            "matches_equal_weight": matches_equal_weight,
            "players_updated": len(players_touched),
            "total_matches": total_matches,
            "errors": errors[:10],  # Limit error list
        }

    def _backfill_match_with_fantasy(
        self,
        match_id: int,
        guild_id: int | None,
        participants: list[dict],
        winning_team: int,
    ) -> dict:
        """
        Backfill a single match using FP-weighted OpenSkill update.

        Uses current player ratings (after reset) and fantasy points from participants.
        """
        radiant = [p for p in participants if p.get("side") == "radiant"]
        dire = [p for p in participants if p.get("side") == "dire"]

        if len(radiant) != 5 or len(dire) != 5:
            return {"success": False, "error": f"Invalid team sizes: {len(radiant)}/{len(dire)}"}

        # Get current ratings (from DB, after potential reset)
        all_ids = [p["discord_id"] for p in participants]
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids, guild_id)

        # Build team data: (discord_id, mu, sigma, fantasy_points)
        team1_data = []
        for p in radiant:
            pid = p["discord_id"]
            mu, sigma = os_ratings.get(pid, (None, None))
            fp = p.get("fantasy_points")
            team1_data.append((pid, mu, sigma, fp))

        team2_data = []
        for p in dire:
            pid = p["discord_id"]
            mu, sigma = os_ratings.get(pid, (None, None))
            fp = p.get("fantasy_points")
            team2_data.append((pid, mu, sigma, fp))

        try:
            results = self.openskill_system.update_ratings_after_match(
                team1_data, team2_data, winning_team
            )
            # Persist updated ratings
            updates = [(pid, mu, sigma) for pid, (mu, sigma, _) in results.items()]
            self.player_repo.update_openskill_ratings_bulk(updates, guild_id)
            return {"success": True, "players_updated": len(updates)}
        except Exception as e:
            return {"success": False, "error": str(e)}

    def _backfill_match_equal_weight(
        self,
        match_id: int,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        winning_team: int,
    ) -> dict:
        """
        Backfill a single match using equal-weight OpenSkill update.

        Used for non-enriched matches without fantasy data.
        """
        if len(radiant_ids) != 5 or len(dire_ids) != 5:
            return {"success": False, "error": f"Invalid team sizes: {len(radiant_ids)}/{len(dire_ids)}"}

        # Get current ratings (from DB, after potential reset)
        all_ids = radiant_ids + dire_ids
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids, guild_id)

        # Build team data: (discord_id, mu, sigma)
        radiant_data = [
            (pid, *os_ratings.get(pid, (None, None)))
            for pid in radiant_ids
        ]
        dire_data = [
            (pid, *os_ratings.get(pid, (None, None)))
            for pid in dire_ids
        ]

        try:
            results = self.openskill_system.update_ratings_equal_weight(
                radiant_data, dire_data, winning_team
            )
            # Persist updated ratings
            updates = [(pid, mu, sigma) for pid, (mu, sigma) in results.items()]
            self.player_repo.update_openskill_ratings_bulk(updates, guild_id)
            return {"success": True, "players_updated": len(updates)}
        except Exception as e:
            return {"success": False, "error": str(e)}

    def get_openskill_predictions_for_match(
        self, team1_ids: list[int], team2_ids: list[int], guild_id: int | None = None
    ) -> dict:
        """
        Get OpenSkill predicted win probability for a match.

        Args:
            team1_ids: Discord IDs for team 1 (Radiant)
            team2_ids: Discord IDs for team 2 (Dire)

        Returns:
            Dict with team1_win_prob, team1_ordinal, team2_ordinal
        """
        from openskill.models import PlackettLuce

        # Get current ratings
        all_ids = team1_ids + team2_ids
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids, guild_id)

        # Build ratings for each team
        team1_ratings = []
        team1_ordinals = []
        for pid in team1_ids:
            mu, sigma = os_ratings.get(pid, (None, None))
            rating = self.openskill_system.create_rating(mu, sigma)
            team1_ratings.append(rating)
            actual_mu = mu if mu is not None else self.openskill_system.DEFAULT_MU
            actual_sigma = sigma if sigma is not None else self.openskill_system.DEFAULT_SIGMA
            team1_ordinals.append(self.openskill_system.ordinal(actual_mu, actual_sigma))

        team2_ratings = []
        team2_ordinals = []
        for pid in team2_ids:
            mu, sigma = os_ratings.get(pid, (None, None))
            rating = self.openskill_system.create_rating(mu, sigma)
            team2_ratings.append(rating)
            actual_mu = mu if mu is not None else self.openskill_system.DEFAULT_MU
            actual_sigma = sigma if sigma is not None else self.openskill_system.DEFAULT_SIGMA
            team2_ordinals.append(self.openskill_system.ordinal(actual_mu, actual_sigma))

        # Calculate win probability using ordinals
        # Higher ordinal = higher skill
        team1_avg_ordinal = sum(team1_ordinals) / len(team1_ordinals) if team1_ordinals else 0
        team2_avg_ordinal = sum(team2_ordinals) / len(team2_ordinals) if team2_ordinals else 0

        # Use a logistic function to convert ordinal difference to win probability
        # Similar to Elo expected score calculation
        ordinal_diff = team1_avg_ordinal - team2_avg_ordinal
        # Scale factor: typical ordinal range is roughly -10 to +15, so 10 point diff ≈ 76% win
        team1_win_prob = 1.0 / (1.0 + 10 ** (-ordinal_diff / 10.0))

        return {
            "team1_win_prob": team1_win_prob,
            "team1_avg_ordinal": team1_avg_ordinal,
            "team2_avg_ordinal": team2_avg_ordinal,
        }

    def correct_match_result(
        self,
        match_id: int,
        new_winning_team: str,
        guild_id: int | None = None,
        corrected_by: int | None = None,
    ) -> dict:
        """
        Correct an incorrectly recorded match result.

        This reverses all effects of the original recording and re-applies
        them with the correct winning team. Effects reversed/reapplied:
        - Win/loss counters
        - Glicko-2 ratings (restored from rating_history snapshots)
        - OpenSkill ratings (restored from rating_history snapshots)
        - Bet payouts (winners become losers and vice versa)
        - Pairings statistics

        Note: Loan repayments are NOT reversed (they are deferred payments,
        not match-dependent rewards).

        Args:
            match_id: The match ID to correct
            new_winning_team: 'radiant' or 'dire'
            guild_id: Guild ID (for bet operations)
            corrected_by: Discord ID of the admin making the correction

        Returns:
            Dict with correction details and summary

        Raises:
            ValueError: If match not found, result unchanged, or missing data
        """
        if new_winning_team not in ("radiant", "dire"):
            raise ValueError("new_winning_team must be 'radiant' or 'dire'")

        # 1. Load match data
        match = self.match_repo.get_match(match_id, guild_id)
        if not match:
            raise ValueError(f"Match {match_id} not found")

        old_winning_team_num = match.get("winning_team")  # 1 = Radiant, 2 = Dire
        new_winning_team_num = 1 if new_winning_team == "radiant" else 2

        if old_winning_team_num == new_winning_team_num:
            raise ValueError(
                f"Match {match_id} already has {new_winning_team} as winner"
            )

        old_winning_team = "radiant" if old_winning_team_num == 1 else "dire"

        # 2. Get participant data
        participants = self.match_repo.get_match_participants(match_id, guild_id)
        if not participants:
            raise ValueError(f"No participants found for match {match_id}")

        radiant_ids = [p["discord_id"] for p in participants if p.get("side") == "radiant"]
        dire_ids = [p["discord_id"] for p in participants if p.get("side") == "dire"]

        if len(radiant_ids) != 5 or len(dire_ids) != 5:
            logger.warning(
                f"Match {match_id} correction: unexpected team sizes "
                f"radiant={len(radiant_ids)}, dire={len(dire_ids)}"
            )

        # Determine old/new winners and losers
        old_winner_ids = radiant_ids if old_winning_team == "radiant" else dire_ids
        old_loser_ids = dire_ids if old_winning_team == "radiant" else radiant_ids
        new_winner_ids = radiant_ids if new_winning_team == "radiant" else dire_ids
        new_loser_ids = dire_ids if new_winning_team == "radiant" else radiant_ids

        # 3. Load rating history for restoration
        rating_history = self.match_repo.get_full_rating_history_for_match(match_id)
        if not rating_history:
            raise ValueError(
                f"No rating history found for match {match_id}. "
                "Cannot correct matches without stored snapshots."
            )

        # 4. Reverse win/loss counters
        # Old winners: wins-- | Old losers: losses--
        # New winners: wins++ | New losers: losses++
        # Since old winners become new losers and vice versa, we can just swap
        self.player_repo.correct_win_loss_counts(old_winner_ids, old_loser_ids, guild_id)

        # 5. Restore pre-match ratings from rating_history
        glicko_updates = []
        os_updates = []
        for entry in rating_history:
            pid = entry["discord_id"]
            # Restore Glicko-2 from before values
            if entry.get("rating_before") is not None:
                glicko_updates.append((
                    pid,
                    entry["rating_before"],
                    entry["rd_before"],
                    entry["volatility_before"],
                ))
            # Restore OpenSkill from before values
            if entry.get("os_mu_before") is not None:
                os_updates.append((pid, entry["os_mu_before"], entry["os_sigma_before"]))

        # 6. Recalculate ratings with correct winner
        # Build Glicko players from restored ratings
        radiant_glicko = []
        dire_glicko = []
        rating_by_id = {e["discord_id"]: e for e in rating_history}

        for pid in radiant_ids:
            entry = rating_by_id.get(pid)
            if entry and entry.get("rating_before") is not None:
                player = self.rating_system.create_player_from_rating(
                    entry["rating_before"],
                    entry["rd_before"],
                    entry["volatility_before"],
                )
            else:
                # Fallback to current rating
                player, _ = self._load_glicko_player(pid, guild_id)
            radiant_glicko.append((player, pid))

        for pid in dire_ids:
            entry = rating_by_id.get(pid)
            if entry and entry.get("rating_before") is not None:
                player = self.rating_system.create_player_from_rating(
                    entry["rating_before"],
                    entry["rd_before"],
                    entry["volatility_before"],
                )
            else:
                player, _ = self._load_glicko_player(pid, guild_id)
            dire_glicko.append((player, pid))

        # Update Glicko-2 with correct winner
        if new_winning_team == "radiant":
            team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                radiant_glicko, dire_glicko, 1
            )
        else:
            team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                dire_glicko, radiant_glicko, 1
            )

        # Persist new Glicko ratings
        new_glicko_updates = [
            (pid, rating, rd, vol)
            for rating, rd, vol, pid in team1_updated + team2_updated
        ]
        self.player_repo.update_glicko_ratings_bulk(new_glicko_updates, guild_id)

        # Update rating history with new values
        glicko_by_id = {pid: (rating, rd, vol) for pid, rating, rd, vol in new_glicko_updates}
        for entry in rating_history:
            pid = entry["discord_id"]
            new_won = pid in new_winner_ids
            if pid in glicko_by_id:
                new_rating, new_rd, new_vol = glicko_by_id[pid]
                self.match_repo.update_rating_history_for_correction(
                    match_id=match_id,
                    discord_id=pid,
                    new_rating=new_rating,
                    new_rd=new_rd,
                    new_volatility=new_vol,
                    new_won=new_won,
                )

        # 7. Recalculate OpenSkill ratings with correct winner
        if os_updates:
            # Build OpenSkill data from restored ratings
            os_rating_by_id = {pid: (mu, sigma) for pid, mu, sigma in os_updates}

            radiant_os_data = [
                (pid, *os_rating_by_id.get(pid, (None, None)))
                for pid in radiant_ids
            ]
            dire_os_data = [
                (pid, *os_rating_by_id.get(pid, (None, None)))
                for pid in dire_ids
            ]

            os_results = self.openskill_system.update_ratings_equal_weight(
                radiant_os_data, dire_os_data,
                winning_team=new_winning_team_num
            )

            # Persist new OpenSkill ratings
            new_os_updates = [(pid, mu, sigma) for pid, (mu, sigma) in os_results.items()]
            self.player_repo.update_openskill_ratings_bulk(new_os_updates, guild_id)

            # Update rating history OpenSkill after values
            for pid, (new_mu, new_sigma) in os_results.items():
                self.match_repo.update_rating_history_for_correction(
                    match_id=match_id,
                    discord_id=pid,
                    new_rating=glicko_by_id.get(pid, (0, 0, 0))[0],
                    new_rd=glicko_by_id.get(pid, (0, 0, 0))[1],
                    new_volatility=glicko_by_id.get(pid, (0, 0, 0))[2],
                    new_won=pid in new_winner_ids,
                    new_os_mu=new_mu,
                    new_os_sigma=new_sigma,
                )

        # 8. Update match result in database
        self.match_repo.update_match_result(match_id, new_winning_team_num)

        # 9. Reverse and recalculate bet payouts
        bet_correction_summary = {}
        if self.betting_service and hasattr(self.betting_service, "bet_repo"):
            bet_repo = self.betting_service.bet_repo
            all_bets = bet_repo.get_settled_bets_for_match(match_id)

            if all_bets:
                # Identify old winners (bets that won under old result)
                old_winning_bets = [
                    b for b in all_bets
                    if (old_winning_team == "radiant" and b["team_bet_on"] == "radiant")
                    or (old_winning_team == "dire" and b["team_bet_on"] == "dire")
                ]

                # New winning bets
                new_winning_bets = [
                    b for b in all_bets
                    if (new_winning_team == "radiant" and b["team_bet_on"] == "radiant")
                    or (new_winning_team == "dire" and b["team_bet_on"] == "dire")
                ]

                # Reverse payouts from old winners
                reversal_deltas = bet_repo.reverse_bet_payouts_for_correction(
                    match_id, old_winning_bets
                )

                # Apply new payouts
                betting_mode = "pool"  # Default to pool mode
                new_deltas = bet_repo.apply_new_bet_payouts_for_correction(
                    match_id, new_winning_bets, pool_mode=(betting_mode == "pool")
                )

                # Combine deltas and apply to player balances
                combined_deltas: dict[int, int] = {}
                for pid, delta in reversal_deltas.items():
                    combined_deltas[pid] = combined_deltas.get(pid, 0) + delta
                for pid, delta in new_deltas.items():
                    combined_deltas[pid] = combined_deltas.get(pid, 0) + delta

                if combined_deltas:
                    self.player_repo.add_balance_many(combined_deltas, guild_id)

                bet_correction_summary = {
                    "bets_affected": len(all_bets),
                    "old_winners_reversed": len(old_winning_bets),
                    "new_winners_paid": len(new_winning_bets),
                    "balance_changes": combined_deltas,
                }

        # 10. Reverse and recalculate pairings
        if self.pairings_repo:
            # Reverse original pairings
            self.pairings_repo.reverse_pairings_for_match(
                guild_id=guild_id,
                team1_ids=radiant_ids,
                team2_ids=dire_ids,
                original_winning_team=old_winning_team_num,
            )
            # Apply new pairings with correct winner
            self.pairings_repo.update_pairings_for_match(
                match_id=match_id,
                guild_id=guild_id,
                team1_ids=radiant_ids,
                team2_ids=dire_ids,
                winning_team=new_winning_team_num,
            )

        # 11. Log correction for audit
        correction_id = None
        if corrected_by is not None:
            correction_id = self.match_repo.add_match_correction(
                match_id=match_id,
                old_winning_team=old_winning_team_num,
                new_winning_team=new_winning_team_num,
                corrected_by=corrected_by,
            )

        logger.info(
            f"Match {match_id} corrected: {old_winning_team} -> {new_winning_team} "
            f"(by user {corrected_by})"
        )

        return {
            "match_id": match_id,
            "old_winning_team": old_winning_team,
            "new_winning_team": new_winning_team,
            "correction_id": correction_id,
            "players_affected": len(radiant_ids) + len(dire_ids),
            "ratings_updated": len(new_glicko_updates),
            "bet_correction": bet_correction_summary,
            "new_winner_ids": new_winner_ids,
            "new_loser_ids": new_loser_ids,
        }

    # ==================== Query Methods ====================
    # These methods provide query access without exposing the repository directly

    def get_match_by_id(self, match_id: int, guild_id: int | None = None) -> dict | None:
        """
        Get a match by its internal ID.

        Args:
            match_id: Internal match ID
            guild_id: Guild ID (optional, for validation)

        Returns:
            Match dict or None if not found
        """
        return self.match_repo.get_match(match_id, guild_id)

    def get_most_recent_match(self, guild_id: int | None = None) -> dict | None:
        """
        Get the most recently recorded match for a guild.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Match dict or None if no matches found
        """
        return self.match_repo.get_most_recent_match(guild_id)

    def get_match_participants(self, match_id: int, guild_id: int | None = None) -> list[dict]:
        """
        Get all participants for a match with their stats.

        Args:
            match_id: Internal match ID
            guild_id: Guild ID for multi-guild isolation (optional)

        Returns:
            List of participant dicts with hero, KDA, etc.
        """
        return self.match_repo.get_match_participants(match_id, guild_id)

    def get_player_matches(
        self, discord_id: int, guild_id: int | None, limit: int = 10
    ) -> list[dict]:
        """
        Get a player's recent matches.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            limit: Maximum number of matches to return

        Returns:
            List of match dicts, most recent first
        """
        return self.match_repo.get_player_matches(discord_id, guild_id, limit=limit)

    def get_rating_history_for_match(self, match_id: int) -> list[dict]:
        """
        Get rating history entries for a specific match.

        Args:
            match_id: Internal match ID

        Returns:
            List of rating history entries for participants
        """
        return self.match_repo.get_rating_history_for_match(match_id)

    def get_matches_without_fantasy_data(self, limit: int = 100) -> list[dict]:
        """
        Get matches that have enrichment but no fantasy data.

        Used for fantasy data backfill operations.

        Args:
            limit: Maximum number of matches to return

        Returns:
            List of match dicts needing fantasy data
        """
        return self.match_repo.get_matches_without_fantasy_data(limit=limit)

    def get_enriched_count(self, guild_id: int | None = None) -> int:
        """
        Get count of enriched matches for a guild.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Number of enriched matches
        """
        return self.match_repo.get_enriched_count(guild_id)

    def wipe_all_enrichments(self, guild_id: int | None = None) -> int:
        """
        Clear all match enrichments for a guild.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Number of matches wiped
        """
        return self.match_repo.wipe_all_enrichments(guild_id)

    def wipe_match_enrichment(self, match_id: int, guild_id: int | None = None) -> bool:
        """
        Clear enrichment data for a specific match.

        Args:
            match_id: Internal match ID
            guild_id: Guild ID (optional)

        Returns:
            True if match was found and wiped, False otherwise
        """
        return self.match_repo.wipe_match_enrichment(match_id)

    def get_player_openskill_history(
        self, discord_id: int, guild_id: int, limit: int = 10
    ) -> list[dict]:
        """
        Get a player's OpenSkill rating history.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            limit: Maximum number of entries to return

        Returns:
            List of OpenSkill history entries, most recent first
        """
        return self.match_repo.get_player_openskill_history(discord_id, guild_id, limit=limit)

    # --- Statistics and calibration facade methods ---

    def get_match_count(self, guild_id: int | None = None) -> int:
        """Get total number of matches recorded for a guild."""
        return self.match_repo.get_match_count(guild_id)

    def get_recent_match_predictions(self, guild_id: int | None, limit: int = 200) -> list[dict]:
        """Get recent match predictions for calibration analysis."""
        return self.match_repo.get_recent_match_predictions(guild_id, limit)

    def get_recent_rating_history(self, guild_id: int | None, limit: int = 500) -> list[dict]:
        """Get recent rating history entries for calibration analysis."""
        return self.match_repo.get_recent_rating_history(guild_id, limit)

    def get_biggest_upsets(self, guild_id: int | None, limit: int = 5) -> list[dict]:
        """Get biggest upset matches (underdogs who won against the odds)."""
        return self.match_repo.get_biggest_upsets(guild_id, limit)

    def get_player_performance_stats(self, guild_id: int | None) -> list[dict]:
        """Get player performance vs expected stats."""
        return self.match_repo.get_player_performance_stats(guild_id)

    def get_lobby_type_stats(self, guild_id: int | None) -> list[dict]:
        """Get rating swing statistics by lobby type (shuffle vs draft)."""
        return self.match_repo.get_lobby_type_stats(guild_id)

    def get_player_rating_history_detailed(
        self, discord_id: int, guild_id: int | None, limit: int = 50
    ) -> list[dict]:
        """Get detailed rating history for a player including predictions."""
        return self.match_repo.get_player_rating_history_detailed(discord_id, guild_id, limit)

    def get_os_ratings_for_match(self, match_id: int) -> dict:
        """Get OpenSkill ratings for teams in a match."""
        return self.match_repo.get_os_ratings_for_match(match_id)

    def get_player_lobby_type_stats(self, discord_id: int, guild_id: int | None) -> list[dict]:
        """Get lobby type statistics for a specific player."""
        return self.match_repo.get_player_lobby_type_stats(discord_id, guild_id)

    def get_player_hero_stats_detailed(
        self, discord_id: int, guild_id: int | None, limit: int = 8
    ) -> list[dict]:
        """Get detailed hero performance stats for a player."""
        return self.match_repo.get_player_hero_stats_detailed(discord_id, guild_id, limit)

    def get_player_hero_role_breakdown(self, discord_id: int, guild_id: int | None) -> list[dict]:
        """Get hero role breakdown (core vs support) for a player."""
        return self.match_repo.get_player_hero_role_breakdown(discord_id, guild_id)

    def get_player_fantasy_stats(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Get fantasy points statistics for a player."""
        return self.match_repo.get_player_fantasy_stats(discord_id, guild_id)

    def update_participant_stats(
        self,
        match_id: int,
        discord_id: int,
        hero_id: int,
        kills: int,
        deaths: int,
        assists: int,
        gpm: int,
        xpm: int,
        hero_damage: int,
        tower_damage: int,
        last_hits: int,
        denies: int,
        net_worth: int,
        hero_healing: int = 0,
        lane_role: int | None = None,
        lane_efficiency: int | None = None,
        towers_killed: int | None = None,
        roshans_killed: int | None = None,
        teamfight_participation: float | None = None,
        obs_placed: int | None = None,
        sen_placed: int | None = None,
        camps_stacked: int | None = None,
        rune_pickups: int | None = None,
        firstblood_claimed: int | None = None,
        stuns: float | None = None,
        fantasy_points: float | None = None,
    ) -> bool:
        """
        Update stats for a match participant.

        Args:
            match_id: Internal match ID
            discord_id: Player's Discord ID
            hero_id: Hero ID played
            kills, deaths, assists: KDA stats
            gpm, xpm: Gold/XP per minute
            hero_damage, tower_damage: Damage dealt
            last_hits, denies: Farming stats
            net_worth: Final net worth
            hero_healing: Healing done
            lane_role, lane_efficiency: Laning phase stats
            towers_killed, roshans_killed: Objectives
            teamfight_participation: Teamfight participation rate
            obs_placed, sen_placed: Vision stats
            camps_stacked, rune_pickups: Utility stats
            firstblood_claimed: First blood participation
            stuns: Total stun time dealt
            fantasy_points: Calculated fantasy points

        Returns:
            True if participant was updated, False otherwise
        """
        return self.match_repo.update_participant_stats(
            match_id=match_id,
            discord_id=discord_id,
            hero_id=hero_id,
            kills=kills,
            deaths=deaths,
            assists=assists,
            gpm=gpm,
            xpm=xpm,
            hero_damage=hero_damage,
            tower_damage=tower_damage,
            last_hits=last_hits,
            denies=denies,
            net_worth=net_worth,
            hero_healing=hero_healing,
            lane_role=lane_role,
            lane_efficiency=lane_efficiency,
            towers_killed=towers_killed,
            roshans_killed=roshans_killed,
            teamfight_participation=teamfight_participation,
            obs_placed=obs_placed,
            sen_placed=sen_placed,
            camps_stacked=camps_stacked,
            rune_pickups=rune_pickups,
            firstblood_claimed=firstblood_claimed,
            stuns=stuns,
            fantasy_points=fantasy_points,
        )

    def record_match_raw(
        self,
        team1_ids: list[int],
        team2_ids: list[int],
        winning_team: int,
        guild_id: int | None = None,
        lobby_type: str = "shuffle",
    ) -> int:
        """
        Record a match directly without shuffle state or rating updates.

        Used for test data seeding. For normal match recording with ratings,
        use record_match() instead.

        Args:
            team1_ids: Discord IDs of Radiant players
            team2_ids: Discord IDs of Dire players
            winning_team: 1 (Radiant won) or 2 (Dire won)
            guild_id: Guild ID for multi-server isolation
            lobby_type: 'shuffle' or 'draft'

        Returns:
            Match ID
        """
        return self.match_repo.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=winning_team,
            guild_id=guild_id or 0,
            lobby_type=lobby_type,
        )

    def get_players_with_enriched_data(self, guild_id: int | None) -> list[dict]:
        """
        Get players who have enriched match data.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            List of player dicts with discord_id and enriched match count
        """
        return self.match_repo.get_players_with_enriched_data(guild_id)

    def get_last_match_participant_ids(self) -> list[int]:
        """
        Get participant IDs from the most recent recorded match.

        Returns:
            List of Discord IDs from the last match, or empty list
        """
        return self.match_repo.get_last_match_participant_ids()

    def get_multi_player_hero_stats(self, player_ids: list[int], guild_id: int | None) -> list[dict]:
        """
        Get hero statistics for multiple players for hero grid.

        Args:
            player_ids: List of Discord IDs
            guild_id: Guild ID for multi-server isolation

        Returns:
            List of dicts with player hero stats for grid visualization
        """
        return self.match_repo.get_multi_player_hero_stats(player_ids, guild_id)

    def get_scout_data(
        self, player_ids: list[int], guild_id: int | None, limit: int = 10
    ) -> dict:
        """
        Get aggregated hero scouting data for multiple players.

        Aggregates hero stats across all specified players, showing their combined
        hero pool with games, wins, losses, bans, and primary role.

        Args:
            player_ids: List of Discord IDs to scout
            guild_id: Guild ID for multi-server isolation
            limit: Maximum number of heroes to return (default 10)

        Returns:
            Dict with:
                - player_count: Number of players included
                - heroes: List of top heroes by games, each containing:
                    - hero_id, games, wins, losses, bans, primary_role
        """
        if not player_ids:
            return {"player_count": 0, "heroes": []}

        # Get per-player hero stats
        player_stats = self.match_repo.get_player_hero_stats_for_scout(player_ids, guild_id)

        # Get deduplicated ban counts (opposing team only)
        ban_data = self.match_repo.get_bans_for_players(player_ids, guild_id)

        # Get total unique match count for contest rate calculation
        total_matches = self.match_repo.get_match_count_for_players(player_ids, guild_id)

        # Aggregate hero stats across all players
        aggregated: dict[int, dict] = {}  # hero_id -> {games, wins, losses, roles}

        for discord_id, heroes in player_stats.items():
            for hero in heroes:
                hero_id = hero["hero_id"]
                if hero_id not in aggregated:
                    aggregated[hero_id] = {
                        "games": 0,
                        "wins": 0,
                        "losses": 0,
                        "roles": [],
                    }
                aggregated[hero_id]["games"] += hero["games"]
                aggregated[hero_id]["wins"] += hero["wins"]
                aggregated[hero_id]["losses"] += hero["losses"]
                if hero.get("primary_role"):
                    aggregated[hero_id]["roles"].append(hero["primary_role"])

        # Sort by total relevance (games + bans), take top N
        sorted_heroes = sorted(
            aggregated.items(),
            key=lambda x: -(x[1]["games"] + ban_data.get(x[0], 0)),
        )[:limit]

        # Build result with primary_role (mode) and ban counts
        # Default to Carry (1) when no role data is available
        DEFAULT_ROLE = 1
        result_heroes = []
        for hero_id, stats in sorted_heroes:
            roles = stats["roles"]
            if roles:
                primary_role = max(set(roles), key=roles.count)
            else:
                primary_role = DEFAULT_ROLE

            result_heroes.append({
                "hero_id": hero_id,
                "games": stats["games"],
                "wins": stats["wins"],
                "losses": stats["losses"],
                "bans": ban_data.get(hero_id, 0),
                "primary_role": primary_role,
            })

        return {
            "player_count": len(player_ids),
            "total_matches": total_matches,
            "heroes": result_heroes,
        }
