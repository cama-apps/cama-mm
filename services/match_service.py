"""
Match orchestration: shuffling and recording.
"""

import random
import threading
import time
from typing import Any

from config import BET_LOCK_SECONDS
from domain.models.player import Player
from domain.models.team import Team
from domain.services.team_balancing_service import TeamBalancingService
from rating_system import CamaRatingSystem
from repositories.interfaces import IMatchRepository, IPairingsRepository, IPlayerRepository
from services.betting_service import BettingService
from shuffler import BalancedShuffler


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
        """
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.use_glicko = use_glicko
        self.rating_system = CamaRatingSystem()
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
    ) -> None:
        """
        Store message metadata (id, channel, jump_url) for the pending shuffle.
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
            "betting_mode": state.get("betting_mode", "house"),
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
        self, player_ids: list[int], guild_id: int | None = None, betting_mode: str = "house"
    ) -> dict:
        """
        Shuffle players into balanced teams.

        Args:
            player_ids: List of Discord user IDs to shuffle
            guild_id: Guild ID for multi-guild support
            betting_mode: "house" for 1:1 payouts, "pool" for parimutuel betting

        Returns a payload containing teams, role assignments, and Radiant/Dire mapping.
        """
        if betting_mode not in ("house", "pool"):
            raise ValueError("betting_mode must be 'house' or 'pool'")
        players = self.player_repo.get_by_ids(player_ids)
        if len(players) != len(player_ids):
            raise ValueError(
                f"Could not load all players: expected {len(player_ids)}, got {len(players)}"
            )

        if len(players) < 10:
            raise ValueError("Need at least 10 players to shuffle.")

        # Cap to 12 for performance (mirrors prior behavior)
        if len(players) > 12:
            players = players[:12]
            player_ids = player_ids[:12]

        exclusion_counts_by_id = self.player_repo.get_exclusion_counts(player_ids)
        # Shuffler expects name->count mapping; this is internal to shuffler only
        exclusion_counts = {
            pl.name: exclusion_counts_by_id.get(pid, 0) for pid, pl in zip(player_ids, players)
        }

        if len(players) > 10:
            team1, team2, excluded_players = self.shuffler.shuffle_from_pool(
                players, exclusion_counts
            )
        else:
            team1, team2 = self.shuffler.shuffle(players)
            excluded_players = []

        off_role_mult = self.shuffler.off_role_multiplier
        team1_value = team1.get_team_value(self.use_glicko, off_role_mult)
        team2_value = team2.get_team_value(self.use_glicko, off_role_mult)
        value_diff = abs(team1_value - team2_value)

        team1_off_roles = team1.get_off_role_count()
        team2_off_roles = team2.get_off_role_count()
        off_role_penalty = (team1_off_roles + team2_off_roles) * self.shuffler.off_role_flat_penalty
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
            excluded_penalty = exclusion_sum * self.shuffler.exclusion_penalty_weight

        goodness_score = (
            value_diff + off_role_penalty + weighted_role_matchup_delta + excluded_penalty
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
        }

    def _load_glicko_player(self, player_id: int) -> tuple[Player, int]:
        rating_data = self.player_repo.get_glicko_rating(player_id)
        if rating_data:
            rating, rd, vol = rating_data
            player = self.rating_system.create_player_from_rating(rating, rd, vol)
            return player, player_id

        player_obj = self.player_repo.get_by_id(player_id)
        if player_obj and player_obj.mmr is not None:
            glicko_player = self.rating_system.create_player_from_mmr(player_obj.mmr)
        else:
            glicko_player = self.rating_system.create_player_from_mmr(None)
        return glicko_player, player_id

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

            match_id = self.match_repo.record_match(
                team1_ids=radiant_team_ids,
                team2_ids=dire_team_ids,
                winning_team=1 if winning_team == "radiant" else 2,
                dotabuff_match_id=dotabuff_match_id,
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
            }
        finally:
            with self._recording_lock:
                self._recording_in_progress.discard(normalized_gid)
