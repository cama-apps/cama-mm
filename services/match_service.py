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
        stake_service=None,
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
            stake_service: Optional stake service for draft mode player stakes
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
        self._last_shuffle_by_guild: dict[int, dict] = {}
        self.betting_service = betting_service
        self.pairings_repo = pairings_repo
        self.loan_service = loan_service
        self.stake_service = stake_service
        self.spectator_pool_service = None  # Set externally by bot.py
        # Guard against concurrent finalizations per guild
        self._recording_lock = threading.Lock()
        self._recording_in_progress: set[int] = set()

    def _map_player_ids(self, player_ids: list[int], players: list[Player]) -> dict[int, int]:
        """Map Player object identity (id()) to Discord ID for stable lookups."""
        return {id(pl): pid for pid, pl in zip(player_ids, players)}

    def _resolve_team_ids(self, team: Team, player_id_map: dict[int, int]) -> list[int]:
        """Resolve Team players to Discord IDs using object identity."""
        return [player_id_map[id(p)] for p in team.players]

    def _normalize_guild_id(self, guild_id: int | None) -> int:
        return guild_id if guild_id is not None else 0

    def get_last_shuffle(self, guild_id: int | None = None) -> dict | None:
        normalized = self._normalize_guild_id(guild_id)
        state = self._last_shuffle_by_guild.get(normalized)
        if state:
            return state
        persisted = self.match_repo.get_pending_match(guild_id)
        if persisted:
            self._last_shuffle_by_guild[normalized] = persisted
            return persisted
        return None

    def set_last_shuffle(self, guild_id: int | None, payload: dict) -> None:
        self._last_shuffle_by_guild[self._normalize_guild_id(guild_id)] = payload

    def set_shuffle_message_url(self, guild_id: int | None, jump_url: str) -> None:
        """
        Store the message link for the current pending shuffle so other commands can link to it.

        Legacy helper retained for backward compatibility; prefers set_shuffle_message_info.
        """
        self.set_shuffle_message_info(guild_id, message_id=None, channel_id=None, jump_url=jump_url)

    def set_shuffle_message_info(
        self,
        guild_id: int | None,
        message_id: int | None,
        channel_id: int | None,
        jump_url: str | None = None,
        thread_message_id: int | None = None,
        thread_id: int | None = None,
        origin_channel_id: int | None = None,
    ) -> None:
        """
        Store message metadata (id, channel, jump_url) for the pending shuffle.
        Also stores thread message info for updating betting display in thread.
        origin_channel_id is stored for betting reminders (since reset_lobby clears it).
        """
        state = self.get_last_shuffle(guild_id)
        if not state:
            return
        if message_id is not None:
            state["shuffle_message_id"] = message_id
        if channel_id is not None:
            state["shuffle_channel_id"] = channel_id
        if jump_url is not None:
            state["shuffle_message_jump_url"] = jump_url
        if thread_message_id is not None:
            state["thread_shuffle_message_id"] = thread_message_id
        if thread_id is not None:
            state["thread_shuffle_thread_id"] = thread_id
        if origin_channel_id is not None:
            state["origin_channel_id"] = origin_channel_id
        self._persist_match_state(guild_id, state)

    def get_shuffle_message_info(self, guild_id: int | None) -> dict[str, int | None]:
        """
        Return message metadata for the pending shuffle, if present.
        """
        state = self.get_last_shuffle(guild_id) or {}
        return {
            "message_id": state.get("shuffle_message_id"),
            "channel_id": state.get("shuffle_channel_id"),
            "jump_url": state.get("shuffle_message_jump_url"),
            "thread_message_id": state.get("thread_shuffle_message_id"),
            "thread_id": state.get("thread_shuffle_thread_id"),
            "origin_channel_id": state.get("origin_channel_id"),
        }

    def clear_last_shuffle(self, guild_id: int | None) -> None:
        self._last_shuffle_by_guild.pop(self._normalize_guild_id(guild_id), None)
        self.match_repo.clear_pending_match(guild_id)

    def _ensure_pending_state(self, guild_id: int | None) -> dict:
        state = self.get_last_shuffle(guild_id)
        if not state:
            raise ValueError("No recent shuffle found.")
        return state

    def _ensure_record_submissions(self, state: dict) -> dict[int, dict[str, Any]]:
        if "record_submissions" not in state:
            state["record_submissions"] = {}
        return state["record_submissions"]

    def _build_pending_match_payload(self, state: dict) -> dict:
        return {
            "radiant_team_ids": state["radiant_team_ids"],
            "dire_team_ids": state["dire_team_ids"],
            "radiant_roles": state["radiant_roles"],
            "dire_roles": state["dire_roles"],
            "radiant_value": state["radiant_value"],
            "dire_value": state["dire_value"],
            "value_diff": state["value_diff"],
            "first_pick_team": state["first_pick_team"],
            "excluded_player_ids": state.get("excluded_player_ids", []),
            "record_submissions": state.get("record_submissions", {}),
            "shuffle_timestamp": state.get("shuffle_timestamp"),
            "bet_lock_until": state.get("bet_lock_until"),
            "shuffle_message_jump_url": state.get("shuffle_message_jump_url"),
            "shuffle_message_id": state.get("shuffle_message_id"),
            "shuffle_channel_id": state.get("shuffle_channel_id"),
            "betting_mode": state.get("betting_mode", "pool"),
            "is_draft": state.get("is_draft", False),
        }

    def _persist_match_state(self, guild_id: int | None, state: dict) -> None:
        payload = self._build_pending_match_payload(state)
        self.match_repo.save_pending_match(guild_id, payload)
        # Update in-memory cache to keep it in sync
        self.set_last_shuffle(guild_id, state)

    def has_admin_submission(self, guild_id: int | None) -> bool:
        state = self.get_last_shuffle(guild_id)
        if not state:
            return False
        submissions = state.get("record_submissions", {})
        return any(
            sub.get("is_admin") and sub.get("result") in ("radiant", "dire")
            for sub in submissions.values()
        )

    def has_admin_abort_submission(self, guild_id: int | None) -> bool:
        state = self.get_last_shuffle(guild_id)
        if not state:
            return False
        submissions = state.get("record_submissions", {})
        return any(
            sub.get("is_admin") and sub.get("result") == "abort" for sub in submissions.values()
        )

    def add_record_submission(
        self, guild_id: int | None, user_id: int, result: str, is_admin: bool
    ) -> dict[str, Any]:
        if result not in ("radiant", "dire"):
            raise ValueError("Result must be 'radiant' or 'dire'.")
        state = self._ensure_pending_state(guild_id)
        submissions = self._ensure_record_submissions(state)
        existing = submissions.get(user_id)
        if existing and existing["result"] != result:
            raise ValueError("You already submitted a different result.")
        # Allow conflicting votes - requires MIN_NON_ADMIN_SUBMISSIONS matching submissions for non-admin results
        submissions[user_id] = {"result": result, "is_admin": is_admin}
        self._persist_match_state(guild_id, state)
        vote_counts = self.get_vote_counts(guild_id)
        return {
            "non_admin_count": self.get_non_admin_submission_count(guild_id),
            "total_count": len(submissions),
            "result": self.get_pending_record_result(guild_id),
            "is_ready": self.can_record_match(guild_id),
            "vote_counts": vote_counts,
        }

    def get_non_admin_submission_count(self, guild_id: int | None) -> int:
        state = self.get_last_shuffle(guild_id)
        if not state:
            return 0
        submissions = state.get("record_submissions", {})
        return sum(
            1
            for sub in submissions.values()
            if not sub.get("is_admin") and sub.get("result") in ("radiant", "dire")
        )

    def get_abort_submission_count(self, guild_id: int | None) -> int:
        state = self.get_last_shuffle(guild_id)
        if not state:
            return 0
        submissions = state.get("record_submissions", {})
        return sum(
            1
            for sub in submissions.values()
            if not sub.get("is_admin") and sub.get("result") == "abort"
        )

    def can_abort_match(self, guild_id: int | None) -> bool:
        if self.has_admin_abort_submission(guild_id):
            return True
        return self.get_abort_submission_count(guild_id) >= self.MIN_NON_ADMIN_SUBMISSIONS

    def add_abort_submission(
        self, guild_id: int | None, user_id: int, is_admin: bool
    ) -> dict[str, Any]:
        state = self._ensure_pending_state(guild_id)
        submissions = self._ensure_record_submissions(state)
        existing = submissions.get(user_id)
        if existing and existing["result"] != "abort":
            raise ValueError("You already submitted a different result.")
        submissions[user_id] = {"result": "abort", "is_admin": is_admin}
        self._persist_match_state(guild_id, state)
        return {
            "non_admin_count": self.get_abort_submission_count(guild_id),
            "total_count": len(submissions),
            "is_ready": self.can_abort_match(guild_id),
        }

    def get_vote_counts(self, guild_id: int | None) -> dict[str, int]:
        """Get vote counts for radiant and dire (non-admin only)."""
        state = self.get_last_shuffle(guild_id)
        if not state:
            return {"radiant": 0, "dire": 0}
        submissions = state.get("record_submissions", {})
        counts = {"radiant": 0, "dire": 0}
        for sub in submissions.values():
            if not sub.get("is_admin"):
                result = sub.get("result")
                if result in counts:
                    counts[result] += 1
        return counts

    def get_pending_record_result(self, guild_id: int | None) -> str | None:
        """
        Get the result to record.

        For admin submissions: returns the admin's vote.
        For non-admin: returns the first result to reach MIN_NON_ADMIN_SUBMISSIONS votes.
        """
        state = self.get_last_shuffle(guild_id)
        if not state:
            return None
        submissions = state.get("record_submissions", {})

        # If there's an admin submission (radiant/dire), use that result
        for sub in submissions.values():
            result = sub.get("result")
            if sub.get("is_admin") and result in ("radiant", "dire"):
                return result

        # For non-admin: requires MIN_NON_ADMIN_SUBMISSIONS matching submissions to determine the result
        vote_counts = self.get_vote_counts(guild_id)
        if vote_counts["radiant"] >= self.MIN_NON_ADMIN_SUBMISSIONS:
            return "radiant"
        if vote_counts["dire"] >= self.MIN_NON_ADMIN_SUBMISSIONS:
            return "dire"
        return None

    def can_record_match(self, guild_id: int | None) -> bool:
        if self.has_admin_submission(guild_id):
            return True
        # Requires MIN_NON_ADMIN_SUBMISSIONS matching submissions before a non-admin result can finalize
        return self.get_pending_record_result(guild_id) is not None

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
        if rating_system not in ("glicko", "openskill"):
            raise ValueError("rating_system must be 'glicko' or 'openskill'")
        players = self.player_repo.get_by_ids(player_ids)
        if len(players) != len(player_ids):
            raise ValueError(
                f"Could not load all players: expected {len(player_ids)}, got {len(players)}"
            )

        # Apply RD decay for shuffle priority calculation (not persisted)
        # This ensures returning players get appropriate priority boost
        last_match_dates = self.player_repo.get_last_match_dates(player_ids)
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

        exclusion_counts_by_id = self.player_repo.get_exclusion_counts(player_ids)
        # Shuffler expects name->count mapping; this is internal to shuffler only
        exclusion_counts = {
            pl.name: exclusion_counts_by_id.get(pid, 0) for pid, pl in zip(player_ids, players)
        }

        # Get recent match participants and convert to player names
        recent_match_ids = self.match_repo.get_last_match_participant_ids()
        recent_match_names = {
            p.name for p in players if p.discord_id in recent_match_ids
        }

        # Create a shuffler configured for the requested rating system
        use_openskill = rating_system == "openskill"
        shuffler = BalancedShuffler(
            use_glicko=self.use_glicko,
            use_openskill=use_openskill,
        )

        if len(players) > 10:
            team1, team2, excluded_players = shuffler.shuffle_from_pool(
                players, exclusion_counts, recent_match_names
            )
        else:
            team1, team2 = shuffler.shuffle(players)
            excluded_players = []

        off_role_mult = shuffler.off_role_multiplier
        team1_value = team1.get_team_value(
            self.use_glicko, off_role_mult, use_openskill=use_openskill
        )
        team2_value = team2.get_team_value(
            self.use_glicko, off_role_mult, use_openskill=use_openskill
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

        goodness_score = (
            value_diff + off_role_penalty + weighted_role_matchup_delta + excluded_penalty + recent_match_penalty
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
            self.player_repo.increment_exclusion_count(pid)
        for pid in included_player_ids:
            self.player_repo.decay_exclusion_count(pid)

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
        }

    def _load_glicko_player(self, player_id: int) -> tuple[Player, int]:
        rating_data = self.player_repo.get_glicko_rating(player_id)
        last_dates = self.player_repo.get_last_match_date(player_id)
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
            player_obj = self.player_repo.get_by_id(player_id)
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
    ) -> dict:
        """
        Record a match result and update ratings.

        winning_team: 'radiant' or 'dire'

        Thread-safe: prevents concurrent finalization for the same guild.
        """
        normalized_gid = self._normalize_guild_id(guild_id)

        # Acquire exclusive recording right for this guild
        with self._recording_lock:
            if normalized_gid in self._recording_in_progress:
                raise ValueError("Match recording already in progress for this guild.")
            self._recording_in_progress.add(normalized_gid)

        try:
            last_shuffle = self.get_last_shuffle(guild_id)
            if not last_shuffle:
                raise ValueError("No recent shuffle found.")

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
                dotabuff_match_id=dotabuff_match_id,
                lobby_type=lobby_type,
                balancing_rating_system=balancing_rating_system,
            )

            # Persist win/loss counters for all players in the match (prefer single transaction).
            if hasattr(self.player_repo, "apply_match_outcome"):
                self.player_repo.apply_match_outcome(winning_ids, losing_ids)  # type: ignore[attr-defined]
            else:
                for pid in winning_ids:
                    self.player_repo.increment_wins(pid)
                for pid in losing_ids:
                    self.player_repo.increment_losses(pid)

            distributions = {"winners": [], "losers": []}
            if self.betting_service:
                # Reward participation only for the losing team; winners get the win bonus separately.
                self.betting_service.award_participation(losing_ids)
                distributions = self.betting_service.settle_bets(
                    match_id, guild_id, winning_team, pending_state=last_shuffle
                )
                self.betting_service.award_win_bonus(winning_ids)
                if excluded_player_ids:
                    self.betting_service.award_exclusion_bonus(excluded_player_ids)
                # Award half exclusion bonus to conditional players who were excluded
                excluded_conditional_ids = last_shuffle.get("excluded_conditional_player_ids", [])
                if excluded_conditional_ids:
                    self.betting_service.award_exclusion_bonus_half(excluded_conditional_ids)

            # Settle player stakes (draft mode only)
            stake_distributions = {}
            if self.stake_service and last_shuffle.get("is_draft") and last_shuffle.get("stake_pool_created"):
                stake_distributions = self.stake_service.settle_stakes(
                    match_id, guild_id, winning_team, pending_state=last_shuffle
                )

            # Settle spectator pool (draft mode only)
            spectator_distributions = {}
            if self.spectator_pool_service and last_shuffle.get("is_draft"):
                spectator_distributions = self.spectator_pool_service.settle_bets(
                    match_id=match_id,
                    guild_id=guild_id,
                    winning_team=winning_team,
                    winning_player_ids=winning_ids,
                    pending_state=last_shuffle,
                )

            # Repay outstanding loans for all participants
            loan_repayments = []
            if self.loan_service:
                all_participant_ids = winning_ids + losing_ids
                for player_id in all_participant_ids:
                    state = self.loan_service.get_state(player_id)
                    if state.has_outstanding_loan:
                        result = self.loan_service.repay_loan(player_id, guild_id)
                        if result.get("success"):
                            loan_repayments.append({
                                "player_id": player_id,
                                **result,
                            })

            # Build Glicko players
            radiant_glicko = [self._load_glicko_player(pid) for pid in radiant_team_ids]
            dire_glicko = [self._load_glicko_player(pid) for pid in dire_team_ids]

            # Snapshot pre-match ratings for history + prediction stats
            pre_match = {}
            for player, pid in radiant_glicko:
                pre_match[pid] = {
                    "rating_before": player.rating,
                    "rd_before": player.rd,
                    "volatility_before": player.vol,
                    "team_number": 1,
                    "won": winning_team == "radiant",
                }
            for player, pid in dire_glicko:
                pre_match[pid] = {
                    "rating_before": player.rating,
                    "rd_before": player.rd,
                    "volatility_before": player.vol,
                    "team_number": 2,
                    "won": winning_team == "dire",
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
                    radiant_glicko, dire_glicko, 1
                )
            else:
                team1_updated, team2_updated = self.rating_system.update_ratings_after_match(
                    dire_glicko, radiant_glicko, 1
                )

            {pid for _, _, _, pid in team1_updated + team2_updated}
            expected_ids = set(radiant_team_ids + dire_team_ids)
            # Even if mismatch, continue but skip unknown IDs when writing

            updated_count = 0
            updates = [
                (pid, rating, rd, vol)
                for rating, rd, vol, pid in team1_updated + team2_updated
                if pid in expected_ids
            ]
            if hasattr(self.player_repo, "update_glicko_ratings_bulk"):
                updated_count = self.player_repo.update_glicko_ratings_bulk(updates)  # type: ignore[attr-defined]
            else:
                for pid, rating, rd, vol in updates:
                    self.player_repo.update_glicko_rating(pid, rating, rd, vol)
                    updated_count += 1

            # Update last_match_date for participants
            now_iso = datetime.now(timezone.utc).isoformat()
            for pid in expected_ids:
                self.player_repo.update_last_match_date(pid, now_iso)

            # Track first calibration for players who just became calibrated
            now_unix = int(time.time())
            for pid, rating, rd, vol in updates:
                if rd <= CALIBRATION_RD_THRESHOLD:
                    # Check if player doesn't have first_calibrated_at set yet
                    if hasattr(self.player_repo, "get_first_calibrated_at"):
                        first_cal = self.player_repo.get_first_calibrated_at(pid)
                        if first_cal is None:
                            self.player_repo.set_first_calibrated_at(pid, now_unix)

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
            os_ratings = self.player_repo.get_openskill_ratings_bulk(all_player_ids)

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
            self.player_repo.update_openskill_ratings_bulk(os_updates)

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
                )

            # Update pairwise player statistics
            if self.pairings_repo:
                self.pairings_repo.update_pairings_for_match(
                    match_id=match_id,
                    team1_ids=radiant_team_ids,
                    team2_ids=dire_team_ids,
                    winning_team=1 if winning_team == "radiant" else 2,
                )

            # Clear state after successful record
            self.clear_last_shuffle(guild_id)

            return {
                "match_id": match_id,
                "winning_team": winning_team,
                "updated_count": updated_count,
                "winning_player_ids": winning_ids,
                "losing_player_ids": losing_ids,
                "bet_distributions": distributions,
                "loan_repayments": loan_repayments,
                "stake_distributions": stake_distributions,
                "spectator_distributions": spectator_distributions,
            }
        finally:
            with self._recording_lock:
                self._recording_in_progress.discard(normalized_gid)

    def update_openskill_ratings_for_match(self, match_id: int) -> dict:
        """
        Update OpenSkill ratings for a match using fantasy points as weights.

        This method should be called AFTER match enrichment when fantasy_points
        have been calculated and stored in match_participants.

        Args:
            match_id: The internal match ID to update ratings for

        Returns:
            Dict with:
            - success: bool
            - players_updated: int
            - players_skipped: int (missing fantasy data)
            - error: str (if failed)
        """
        # Get match data
        match = self.match_repo.get_match(match_id)
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
        participants = self.match_repo.get_match_participants(match_id)
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
            os_ratings = self.player_repo.get_openskill_ratings_bulk(discord_ids)
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
        updated_count = self.player_repo.update_openskill_ratings_bulk(updates)

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
        # Try to update existing entry
        with self.match_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE rating_history
                SET os_mu_before = ?,
                    os_mu_after = ?,
                    os_sigma_before = ?,
                    os_sigma_after = ?,
                    fantasy_weight = ?
                WHERE match_id = ? AND discord_id = ?
                """,
                (
                    os_mu_before,
                    os_mu_after,
                    os_sigma_before,
                    os_sigma_after,
                    fantasy_weight,
                    match_id,
                    discord_id,
                ),
            )
            if cursor.rowcount == 0:
                logger.warning(
                    f"No rating_history entry found for match {match_id}, player {discord_id}"
                )

    def backfill_openskill_ratings(self, reset_first: bool = True) -> dict:
        """
        Backfill OpenSkill ratings from all enriched matches with fantasy data.

        Processes matches in chronological order to simulate rating progression.

        Args:
            reset_first: If True, reset all players' OpenSkill ratings to defaults before backfill

        Returns:
            Dict with:
            - matches_processed: int
            - players_updated: int (unique players)
            - errors: list of error messages
        """
        logger.info("Starting OpenSkill backfill...")

        errors = []
        matches_processed = 0
        players_touched = set()

        # Get all enriched matches in chronological order
        enriched_matches = self.match_repo.get_enriched_matches_chronological()
        total_matches = len(enriched_matches)
        logger.info(f"Found {total_matches} enriched matches to process")

        if total_matches == 0:
            return {
                "matches_processed": 0,
                "players_updated": 0,
                "errors": ["No enriched matches found"],
            }

        # Reset all players' OpenSkill ratings if requested
        if reset_first:
            all_players = self.player_repo.get_all()
            reset_updates = [
                (p.discord_id, self.openskill_system.DEFAULT_MU, self.openskill_system.DEFAULT_SIGMA)
                for p in all_players
                if p.discord_id is not None
            ]
            if reset_updates:
                self.player_repo.update_openskill_ratings_bulk(reset_updates)
                logger.info(f"Reset {len(reset_updates)} players to default OpenSkill ratings")

        # Process each match in chronological order
        for i, match in enumerate(enriched_matches):
            match_id = match["match_id"]
            try:
                result = self.update_openskill_ratings_for_match(match_id)
                if result.get("success"):
                    matches_processed += 1
                    # Track unique players
                    participants = self.match_repo.get_match_participants(match_id)
                    for p in participants:
                        players_touched.add(p["discord_id"])
                else:
                    error = result.get("error", "Unknown error")
                    if "No fantasy data" not in error:
                        errors.append(f"Match {match_id}: {error}")
            except Exception as e:
                errors.append(f"Match {match_id}: {str(e)}")
                logger.error(f"Backfill error for match {match_id}: {e}")

            # Log progress periodically
            if (i + 1) % 50 == 0 or (i + 1) == total_matches:
                logger.info(f"Backfill progress: {i + 1}/{total_matches} matches processed")

        logger.info(
            f"OpenSkill backfill complete: {matches_processed} matches, "
            f"{len(players_touched)} unique players"
        )

        return {
            "matches_processed": matches_processed,
            "players_updated": len(players_touched),
            "total_matches": total_matches,
            "errors": errors[:10],  # Limit error list
        }

    def get_openskill_predictions_for_match(
        self, team1_ids: list[int], team2_ids: list[int]
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
        os_ratings = self.player_repo.get_openskill_ratings_bulk(all_ids)

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
