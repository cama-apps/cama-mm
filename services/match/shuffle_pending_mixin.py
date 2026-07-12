"""ShufflePendingMixin mixin for :class:`MatchService`.

Team shuffling plus the pending-match state surface (the thin delegators over
``MatchStateService``) and the protected-hero reservation glue.

Mixin split out of the former monolithic ``match_service`` module; it carries
no state of its own and is composed into ``MatchService``.
"""

import random
import time
from datetime import UTC, datetime

from config import BET_LOCK_SECONDS, DOTA_BET_SEED_AMOUNT
from domain.models.pending_match_state import PendingMatchState
from domain.models.player import Player
from domain.models.team import Team
from rating_system import CamaRatingSystem
from shuffler import BalancedShuffler
from utils.region import region_split_mismatches


class ShufflePendingMixin:
    """ShufflePendingMixin — see module docstring.

    Composed into :class:`~services.match_service.MatchService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    def _map_player_ids(self, player_ids: list[int], players: list[Player]) -> dict[int, int]:
        """Map Player object identity (id()) to Discord ID for stable lookups."""
        return {id(pl): pid for pid, pl in zip(player_ids, players)}

    def _resolve_team_ids(self, team: Team, player_id_map: dict[int, int]) -> list[int]:
        """Resolve Team players to Discord IDs using object identity."""
        return [player_id_map[id(p)] for p in team.players]

    # ==================== State Management (delegated to MatchStateService) ====================


    def get_last_shuffle(self, guild_id: int | None = None, pending_match_id: int | None = None) -> PendingMatchState | None:
        """Get the pending shuffle state (delegates to state_service)."""
        return self.state_service.get_last_shuffle(guild_id, pending_match_id)

    def get_pending_match_for_player(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> PendingMatchState | None:
        """Find the pending match containing a player."""
        return self.state_service.get_pending_match_for_player(guild_id, discord_id)

    def set_last_shuffle(self, guild_id: int | None, state: PendingMatchState) -> None:
        """Set the pending shuffle state (delegates to state_service)."""
        self.state_service.set_last_shuffle(guild_id, state)

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
        cmd_message_id: int | None = None,
        cmd_channel_id: int | None = None,
    ) -> None:
        """Store message metadata for the pending shuffle (delegates to state_service)."""
        self.state_service.set_shuffle_message_info(
            guild_id,
            message_id,
            channel_id,
            jump_url,
            thread_message_id,
            thread_id,
            origin_channel_id,
            pending_match_id,
            cmd_message_id=cmd_message_id,
            cmd_channel_id=cmd_channel_id,
        )

    def get_shuffle_message_info(self, guild_id: int | None, pending_match_id: int | None = None) -> dict[str, int | None]:
        """Return message metadata for the pending shuffle (delegates to state_service)."""
        return self.state_service.get_shuffle_message_info(guild_id, pending_match_id)

    def clear_last_shuffle(self, guild_id: int | None, pending_match_id: int | None = None) -> None:
        """Clear the pending shuffle state (delegates to state_service)."""
        self.state_service.clear_last_shuffle(guild_id, pending_match_id)

    def purchase_protected_hero(
        self,
        *,
        guild_id: int | None,
        pending_match_id: int,
        discord_id: int,
        hero_id: int,
        team_side: str,
        cost: int,
    ) -> dict:
        """Atomically buy a protect-hero reservation for a pending match."""
        return self.match_repo.purchase_protected_hero_atomic(
            guild_id=guild_id,
            pending_match_id=pending_match_id,
            discord_id=discord_id,
            hero_id=hero_id,
            team_side=team_side,
            cost=cost,
        )



    def _build_pending_match_payload(self, state: PendingMatchState) -> dict:
        """Build payload for database persistence (delegates to state_service)."""
        return self.state_service.build_pending_match_payload(state)

    def _persist_match_state(self, guild_id: int | None, state: PendingMatchState) -> None:
        """Persist the pending match state (delegates to state_service)."""
        self.state_service.persist_state(guild_id, state)

    def reserve_betting_seed(
        self, guild_id: int | None, state: PendingMatchState
    ) -> PendingMatchState:
        """Reserve nonprofit funds as match betting seed and persist them on state."""
        if state.bet_seed_reserved > 0 or DOTA_BET_SEED_AMOUNT <= 0:
            return state
        loan_service = getattr(self, "loan_service", None)
        if loan_service is None:
            return state

        reserved = loan_service.deduct_up_to_nonprofit_fund(
            guild_id,
            DOTA_BET_SEED_AMOUNT,
            source="dota_bet_seed",
            related_type="pending_match",
            related_id=state.pending_match_id,
            reason="reserve-backed Dota betting seed",
            metadata={"betting_mode": state.betting_mode},
        )
        if reserved <= 0:
            return state

        state.bet_seed_reserved = reserved
        if state.betting_mode == "pool":
            state.bet_seed_radiant = (reserved + 1) // 2
            state.bet_seed_dire = reserved // 2
            state.bet_seed_bonus = 0
        else:
            state.bet_seed_radiant = 0
            state.bet_seed_dire = 0
            state.bet_seed_bonus = reserved
        self.state_service.persist_state(guild_id, state)
        return state

    def shuffle_players(
        self,
        player_ids: list[int],
        guild_id: int | None = None,
        betting_mode: str = "pool",
        rating_system: str = "glicko",
        shuffle_mode: str = "balanced",
        excluded_conditional_ids: list[int] | None = None,
    ) -> dict:
        """
        Shuffle players into balanced teams.

        Args:
            player_ids: List of Discord user IDs to shuffle
            guild_id: Guild ID for multi-guild support
            betting_mode: "pool" for parimutuel betting, "house" for 1:1 payouts
            rating_system: "glicko" or "openskill" - determines which rating system is used for balancing
            shuffle_mode: "balanced" or "region" - determines team-shape preference
            excluded_conditional_ids: Conditional lobby players not selected for this match

        Returns a payload containing teams, role assignments, and Radiant/Dire mapping.
        """
        if betting_mode not in ("house", "pool"):
            raise ValueError("betting_mode must be 'house' or 'pool'")
        if rating_system not in ("glicko", "openskill", "jopacoin"):
            raise ValueError("rating_system must be 'glicko', 'openskill', or 'jopacoin'")
        if shuffle_mode not in ("balanced", "region"):
            raise ValueError("shuffle_mode must be 'balanced' or 'region'")
        excluded_conditional_ids = list(excluded_conditional_ids or [])
        players = self.player_repo.get_by_ids(player_ids, guild_id)
        if len(players) != len(player_ids):
            raise ValueError(
                f"Could not load all players: expected {len(player_ids)}, got {len(players)}"
            )

        # Apply RD decay for shuffle priority calculation (not persisted).
        # This ensures returning players get appropriate priority boost.
        # Safe to mutate: players are fetched fresh via get_by_ids() each call.
        last_match_dates = self.player_repo.get_last_match_dates(player_ids, guild_id)
        now = datetime.now(UTC)
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

        # Fall back to Glicko if any player lacks OpenSkill ratings
        if rating_system == "openskill" and any(p.os_mu is None for p in players):
            rating_system = "glicko"

        # Create a shuffler configured for the requested rating system
        use_openskill = rating_system == "openskill"
        use_jopacoin = rating_system == "jopacoin"
        shuffler = BalancedShuffler(
            use_glicko=self.use_glicko,
            use_openskill=use_openskill,
            use_jopacoin=use_jopacoin,
            region_split=shuffle_mode == "region",
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

        region_split_penalty = 0.0
        if shuffle_mode == "region":
            region_split_penalty = (
                region_split_mismatches(radiant_team.players, dire_team.players)
                * shuffler.region_split_penalty
            )

        # Rating spread penalty: penalizes wide skill gaps among selected players
        selected_players_list = team1.players + team2.players
        selected_values = [
            p.get_value(self.use_glicko, use_openskill=use_openskill, use_jopacoin=use_jopacoin)
            for p in selected_players_list
        ]
        rating_spread_penalty = shuffler._calculate_rating_spread_penalty(selected_values)

        goodness_score = (
            value_diff + off_role_penalty + weighted_role_matchup_delta
            + excluded_penalty + recent_match_penalty + soft_avoid_penalty
            + package_deal_penalty + region_split_penalty + rating_spread_penalty
        )

        # Calculate Glicko-2 win probability for Radiant
        radiant_glicko_rating, _, _ = self.rating_system.aggregate_team_stats(
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
            radiant_glicko_rating, dire_glicko_rating, dire_glicko_rd
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
        raw_openskill_radiant_win_prob = self.openskill_system.os_predict_win_probability(
            radiant_os_ratings, dire_os_ratings
        )
        openskill_radiant_win_prob = self.openskill_system.calibrate_win_probability(
            raw_openskill_radiant_win_prob
        )

        included_player_ids = set(radiant_team_ids + dire_team_ids)

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
        shuffle_state = PendingMatchState(
            radiant_team_ids=radiant_team_ids,
            dire_team_ids=dire_team_ids,
            excluded_player_ids=excluded_ids,
            excluded_conditional_player_ids=excluded_conditional_ids,
            radiant_roles=radiant_roles,
            dire_roles=dire_roles,
            radiant_value=radiant_value,
            dire_value=dire_value,
            value_diff=value_diff,
            first_pick_team=first_pick_team,
            record_submissions={},
            shuffle_timestamp=now_ts,
            bet_lock_until=now_ts + BET_LOCK_SECONDS,
            shuffle_message_jump_url=None,
            shuffle_message_id=None,
            shuffle_channel_id=None,
            betting_mode=betting_mode,
            is_draft=False,
            balancing_rating_system=rating_system,
            effective_avoid_ids=effective_avoid_ids,  # Avoids to decrement on record
            effective_deal_ids=effective_deal_ids,  # Package deals to decrement on record
            exclusion_updates_deferred=True,
            full_exclusion_increment_ids=excluded_ids,
            half_exclusion_increment_ids=excluded_conditional_ids,
        )
        self.set_last_shuffle(guild_id, shuffle_state)
        self._persist_match_state(guild_id, shuffle_state)
        self.reserve_betting_seed(guild_id, shuffle_state)

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
            "raw_openskill_radiant_win_prob": raw_openskill_radiant_win_prob,
            "balancing_rating_system": rating_system,
            "shuffle_mode": shuffle_mode,
            "region_split_penalty": region_split_penalty,
            "pending_match_id": shuffle_state.pending_match_id,
        }
