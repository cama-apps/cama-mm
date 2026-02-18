"""
Abstract repository interfaces for data access.

These interfaces define the contracts implemented by concrete repositories.
"""

from abc import ABC, abstractmethod


class IPlayerRepository(ABC):
    @abstractmethod
    def add(
        self,
        discord_id: int,
        discord_username: str,
        guild_id: int,
        dotabuff_url: str | None = None,
        initial_mmr: int | None = None,
        preferred_roles: list[str] | None = None,
        main_role: str | None = None,
        glicko_rating: float | None = None,
        glicko_rd: float | None = None,
        glicko_volatility: float | None = None,
    ) -> None: ...

    @abstractmethod
    def get_by_id(self, discord_id: int, guild_id: int): ...

    @abstractmethod
    def get_by_ids(self, discord_ids: list[int], guild_id: int): ...

    @abstractmethod
    def get_by_username(self, username: str, guild_id: int): ...

    @abstractmethod
    def get_all(self, guild_id: int): ...

    @abstractmethod
    def exists(self, discord_id: int, guild_id: int) -> bool: ...

    @abstractmethod
    def update_roles(self, discord_id: int, guild_id: int, roles: list[str]) -> None: ...

    @abstractmethod
    def update_glicko_rating(
        self, discord_id: int, guild_id: int, rating: float, rd: float, volatility: float
    ) -> None: ...

    @abstractmethod
    def get_glicko_rating(self, discord_id: int, guild_id: int) -> tuple[float, float, float] | None: ...

    @abstractmethod
    def update_mmr(self, discord_id: int, guild_id: int, new_mmr: float) -> None: ...

    @abstractmethod
    def get_balance(self, discord_id: int, guild_id: int) -> int: ...

    @abstractmethod
    def update_balance(self, discord_id: int, guild_id: int, amount: int) -> None: ...

    @abstractmethod
    def add_balance(self, discord_id: int, guild_id: int, amount: int) -> None: ...

    @abstractmethod
    def increment_wins(self, discord_id: int, guild_id: int) -> None: ...

    @abstractmethod
    def increment_losses(self, discord_id: int, guild_id: int) -> None: ...

    @abstractmethod
    def get_exclusion_counts(self, discord_ids: list[int], guild_id: int) -> dict[int, int]: ...

    @abstractmethod
    def increment_exclusion_count(self, discord_id: int, guild_id: int) -> None: ...

    @abstractmethod
    def increment_exclusion_count_half(self, discord_id: int, guild_id: int) -> None: ...

    @abstractmethod
    def decay_exclusion_count(self, discord_id: int, guild_id: int) -> None: ...

    @abstractmethod
    def delete(self, discord_id: int, guild_id: int) -> bool: ...

    @abstractmethod
    def delete_all(self, guild_id: int) -> int: ...

    @abstractmethod
    def delete_fake_users(self, guild_id: int) -> int: ...

    @abstractmethod
    def get_by_steam_id(self, steam_id: int):
        """Get player by Steam ID (32-bit account_id)."""
        ...

    @abstractmethod
    def get_steam_id(self, discord_id: int) -> int | None:
        """Get a player's Steam ID."""
        ...

    @abstractmethod
    def set_steam_id(self, discord_id: int, steam_id: int) -> None:
        """Set a player's Steam ID."""
        ...

    @abstractmethod
    def get_all_with_dotabuff_no_steam_id(self) -> list[dict]:
        """Get all players with dotabuff_url but no steam_id set."""
        ...

    # --- Multi-Steam ID methods ---

    @abstractmethod
    def get_steam_ids(self, discord_id: int) -> list[int]:
        """Get all Steam IDs for a player (primary first)."""
        ...

    @abstractmethod
    def add_steam_id(self, discord_id: int, steam_id: int, is_primary: bool = False) -> None:
        """Add a Steam ID to a player."""
        ...

    @abstractmethod
    def remove_steam_id(self, discord_id: int, steam_id: int) -> bool:
        """Remove a Steam ID from a player. Returns True if removed."""
        ...

    @abstractmethod
    def set_primary_steam_id(self, discord_id: int, steam_id: int) -> bool:
        """Set a Steam ID as the primary for a player. Returns True if successful."""
        ...

    @abstractmethod
    def get_primary_steam_id(self, discord_id: int) -> int | None:
        """Get the primary Steam ID for a player."""
        ...

    @abstractmethod
    def get_player_by_any_steam_id(self, steam_id: int):
        """Get player by any of their Steam IDs."""
        ...

    @abstractmethod
    def get_player_above(self, discord_id: int, guild_id: int):
        """Get the player ranked one position higher on the balance leaderboard.

        Used for Red Shell wheel mechanic.

        Returns:
            Player object of the player ranked above, or None if user is #1 or not found
        """
        ...

    @abstractmethod
    def steal_atomic(
        self,
        thief_discord_id: int,
        victim_discord_id: int,
        guild_id: int,
        amount: int,
    ) -> dict[str, int]:
        """Atomically transfer jopacoin from victim to thief (shell mechanic).

        Unlike tips, this transfer has no fee and can push victim below MAX_DEBT.

        Returns:
            Dict with 'amount', 'thief_new_balance', 'victim_new_balance'
        """
        ...


class IBetRepository(ABC):
    VALID_TEAMS: set

    @abstractmethod
    def create_bet(
        self, guild_id: int | None, discord_id: int, team: str, amount: int, bet_time: int
    ) -> int: ...

    @abstractmethod
    def get_player_pending_bet(
        self, guild_id: int | None, discord_id: int, since_ts: int | None = None
    ): ...

    @abstractmethod
    def get_bets_for_pending_match(self, guild_id: int | None, since_ts: int | None = None): ...

    @abstractmethod
    def delete_bets_for_guild(self, guild_id: int | None) -> int: ...

    @abstractmethod
    def get_total_bets_by_guild(
        self, guild_id: int | None, since_ts: int | None = None
    ) -> dict[str, int]: ...

    @abstractmethod
    def assign_match_id(
        self, guild_id: int | None, match_id: int, since_ts: int | None = None
    ) -> None: ...

    @abstractmethod
    def delete_pending_bets(self, guild_id: int | None, since_ts: int | None = None) -> int: ...

    @abstractmethod
    def get_bets_on_player_matches(self, target_discord_id: int) -> list[dict]:
        """Get all bets by OTHER players on matches where target participated."""
        ...


class IMatchRepository(ABC):
    @abstractmethod
    def record_match(
        self,
        team1_ids: list[int],
        team2_ids: list[int],
        winning_team: int,
        guild_id: int,
        radiant_team_ids: list[int] | None = None,
        dire_team_ids: list[int] | None = None,
        dotabuff_match_id: str | None = None,
        notes: str | None = None,
    ) -> int: ...

    @abstractmethod
    def add_rating_history(
        self,
        discord_id: int,
        guild_id: int,
        rating: float,
        match_id: int | None = None,
        rating_before: float | None = None,
        rd_before: float | None = None,
        rd_after: float | None = None,
        volatility_before: float | None = None,
        volatility_after: float | None = None,
        expected_team_win_prob: float | None = None,
        team_number: int | None = None,
        won: bool | None = None,
        streak_length: int | None = None,
        streak_multiplier: float | None = None,
    ) -> None: ...

    @abstractmethod
    def get_match(self, match_id: int, guild_id: int | None = None): ...

    @abstractmethod
    def get_player_matches(self, discord_id: int, guild_id: int, limit: int = 10): ...

    @abstractmethod
    def get_rating_history(self, discord_id: int, guild_id: int, limit: int = 20): ...

    @abstractmethod
    def get_player_recent_outcomes(self, discord_id: int, guild_id: int, limit: int = 20) -> list[bool]:
        """Get recent match outcomes for a player (True=win, most recent first)."""
        ...

    @abstractmethod
    def get_recent_rating_history(self, guild_id: int, limit: int = 200): ...

    @abstractmethod
    def get_match_count(self, guild_id: int) -> int: ...

    @abstractmethod
    def add_match_prediction(
        self,
        match_id: int,
        guild_id: int,
        radiant_rating: float,
        dire_rating: float,
        radiant_rd: float,
        dire_rd: float,
        expected_radiant_win_prob: float,
    ) -> None: ...

    @abstractmethod
    def get_recent_match_predictions(self, guild_id: int, limit: int = 200): ...

    @abstractmethod
    def get_biggest_upsets(self, guild_id: int, limit: int = 5): ...

    @abstractmethod
    def get_player_performance_stats(self, guild_id: int): ...

    @abstractmethod
    def delete_all_matches(self, guild_id: int) -> int: ...

    @abstractmethod
    def save_pending_match(self, guild_id: int | None, payload: dict) -> None: ...

    @abstractmethod
    def get_pending_match(self, guild_id: int | None) -> dict | None: ...

    @abstractmethod
    def clear_pending_match(self, guild_id: int | None) -> None: ...

    @abstractmethod
    def consume_pending_match(self, guild_id: int | None) -> dict | None: ...

    @abstractmethod
    def get_player_hero_stats(self, discord_id: int, guild_id: int) -> dict:
        """Get hero statistics for a player from enriched matches."""
        ...

    @abstractmethod
    def get_last_match_participant_ids(self, guild_id: int) -> set[int]:
        """Get Discord IDs of participants from the most recently recorded match."""
        ...


class ILobbyRepository(ABC):
    @abstractmethod
    def save_lobby_state(
        self,
        lobby_id: int,
        players: list[int],
        status: str,
        created_by: int,
        created_at: str,
        message_id: int | None = None,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        conditional_players: list[int] | None = None,
        origin_channel_id: int | None = None,
        player_join_times: dict[int, float] | None = None,
    ) -> None: ...

    @abstractmethod
    def load_lobby_state(self, lobby_id: int) -> dict | None: ...

    @abstractmethod
    def clear_lobby_state(self, lobby_id: int) -> None: ...


class IPairingsRepository(ABC):
    @abstractmethod
    def update_pairings_for_match(
        self,
        match_id: int,
        team1_ids: list[int],
        team2_ids: list[int],
        winning_team: int,
        guild_id: int,
    ) -> None:
        """Update pairwise statistics for all player pairs in a match."""
        ...

    @abstractmethod
    def get_pairings_for_player(self, discord_id: int, guild_id: int) -> list[dict]:
        """Get all pairwise stats involving a player."""
        ...

    @abstractmethod
    def get_best_teammates(self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with highest win rate when on same team."""
        ...

    @abstractmethod
    def get_worst_teammates(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get players with lowest win rate when on same team."""
        ...

    @abstractmethod
    def get_best_matchups(self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with highest win rate when on opposing teams."""
        ...

    @abstractmethod
    def get_worst_matchups(self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with lowest win rate when on opposing teams."""
        ...

    @abstractmethod
    def get_most_played_with(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get teammates sorted by most games played together."""
        ...

    @abstractmethod
    def get_most_played_against(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents sorted by most games played against."""
        ...

    @abstractmethod
    def get_evenly_matched_teammates(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get teammates with exactly 50% win rate."""
        ...

    @abstractmethod
    def get_evenly_matched_opponents(
        self, discord_id: int, guild_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents with exactly 50% win rate."""
        ...

    @abstractmethod
    def get_pairing_counts(self, discord_id: int, guild_id: int, min_games: int = 1) -> dict:
        """Get total counts of unique teammates and opponents."""
        ...

    @abstractmethod
    def get_head_to_head(self, player1_id: int, player2_id: int, guild_id: int) -> dict | None:
        """Get detailed stats between two specific players."""
        ...

    @abstractmethod
    def rebuild_all_pairings(self, guild_id: int) -> int:
        """Recalculate all pairings from match history. Returns count of pairings updated."""
        ...


class IGuildConfigRepository(ABC):
    @abstractmethod
    def get_config(self, guild_id: int) -> dict | None:
        """Get configuration for a guild."""
        ...

    @abstractmethod
    def set_league_id(self, guild_id: int, league_id: int) -> None:
        """Set the league ID for a guild."""
        ...

    @abstractmethod
    def get_league_id(self, guild_id: int) -> int | None:
        """Get the league ID for a guild."""
        ...

    @abstractmethod
    def set_ai_enabled(self, guild_id: int, enabled: bool) -> None:
        """Set whether AI features are enabled for a guild."""
        ...

    @abstractmethod
    def get_ai_enabled(self, guild_id: int) -> bool:
        """Get whether AI features are enabled for a guild. Defaults to False."""
        ...


class IPredictionRepository(ABC):
    """Repository for prediction market data access."""

    @abstractmethod
    def create_prediction(
        self,
        guild_id: int,
        creator_id: int,
        question: str,
        closes_at: int,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
    ) -> int:
        """Create a new prediction and return its ID."""
        ...

    @abstractmethod
    def get_prediction(self, prediction_id: int) -> dict | None:
        """Get a prediction by ID."""
        ...

    @abstractmethod
    def get_active_predictions(self, guild_id: int) -> list[dict]:
        """Get all open/locked predictions for a guild."""
        ...

    @abstractmethod
    def get_predictions_by_status(self, guild_id: int, status: str) -> list[dict]:
        """Get predictions filtered by status."""
        ...

    @abstractmethod
    def update_prediction_status(self, prediction_id: int, status: str) -> None:
        """Update prediction status (open -> locked -> resolved/cancelled)."""
        ...

    @abstractmethod
    def update_prediction_discord_ids(
        self,
        prediction_id: int,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
    ) -> None:
        """Update Discord IDs for a prediction (thread, embed message)."""
        ...

    @abstractmethod
    def add_resolution_vote(
        self, prediction_id: int, user_id: int, outcome: str, is_admin: bool
    ) -> dict:
        """Add a resolution vote. Returns vote counts."""
        ...

    @abstractmethod
    def get_resolution_votes(self, prediction_id: int) -> dict:
        """Get current resolution vote counts: {"yes": n, "no": m}."""
        ...

    @abstractmethod
    def resolve_prediction(
        self, prediction_id: int, outcome: str, resolved_by: int
    ) -> None:
        """Mark prediction as resolved with outcome."""
        ...

    @abstractmethod
    def cancel_prediction(self, prediction_id: int) -> None:
        """Cancel a prediction (status -> cancelled)."""
        ...

    @abstractmethod
    def place_bet_atomic(
        self, prediction_id: int, discord_id: int, position: str, amount: int
    ) -> dict:
        """Place a bet atomically (debit balance, insert bet). Returns bet info."""
        ...

    @abstractmethod
    def get_prediction_bets(self, prediction_id: int) -> list[dict]:
        """Get all bets for a prediction."""
        ...

    @abstractmethod
    def get_user_bet_on_prediction(
        self, prediction_id: int, discord_id: int
    ) -> dict | None:
        """Get user's bet on a specific prediction."""
        ...

    @abstractmethod
    def get_user_active_positions(self, discord_id: int, guild_id: int | None = None) -> list[dict]:
        """Get all active (unresolved) positions for a user."""
        ...

    @abstractmethod
    def get_prediction_totals(self, prediction_id: int) -> dict:
        """Get bet totals: {"yes_total": n, "no_total": m, "yes_count": x, "no_count": y}."""
        ...

    @abstractmethod
    def settle_prediction_bets(
        self, prediction_id: int, winning_position: str
    ) -> dict:
        """Settle all bets for a resolved prediction. Returns payout summary."""
        ...

    @abstractmethod
    def refund_prediction_bets(self, prediction_id: int) -> dict:
        """Refund all bets for a cancelled prediction. Returns refund summary."""
        ...


class IRecalibrationRepository(ABC):
    """Repository for recalibration state tracking."""

    @abstractmethod
    def get_state(self, discord_id: int, guild_id: int) -> dict | None:
        """Get recalibration state for a player."""
        ...

    @abstractmethod
    def upsert_state(
        self,
        discord_id: int,
        guild_id: int,
        last_recalibration_at: int | None = None,
        total_recalibrations: int | None = None,
        rating_at_recalibration: float | None = None,
    ) -> None:
        """Create or update recalibration state."""
        ...

    @abstractmethod
    def reset_cooldown(self, discord_id: int, guild_id: int) -> None:
        """Reset recalibration cooldown by setting last_recalibration_at to 0."""
        ...


class ISoftAvoidRepository(ABC):
    """Repository for soft avoid feature data access."""

    @abstractmethod
    def create_or_extend_avoid(
        self,
        guild_id: int | None,
        avoider_id: int,
        avoided_id: int,
        games: int = 10,
    ):
        """Create a new soft avoid or extend existing one."""
        ...

    @abstractmethod
    def get_active_avoids_for_players(
        self,
        guild_id: int | None,
        player_ids: list[int],
    ) -> list:
        """Get all active avoids where BOTH avoider and avoided are in player_ids."""
        ...

    @abstractmethod
    def get_user_avoids(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> list:
        """Get all active avoids created by a user."""
        ...

    @abstractmethod
    def decrement_avoids(
        self,
        guild_id: int | None,
        avoid_ids: list[int],
    ) -> int:
        """Decrement games_remaining for the given avoid IDs."""
        ...

    @abstractmethod
    def delete_expired_avoids(self, guild_id: int | None) -> int:
        """Delete avoids with games_remaining = 0."""
        ...


class IPackageDealRepository(ABC):
    """Repository for package deal feature data access."""

    @abstractmethod
    def create_or_extend_deal(
        self,
        guild_id: int | None,
        buyer_id: int,
        partner_id: int,
        games: int = 10,
        cost: int = 0,
    ):
        """Create a new package deal or extend existing one."""
        ...

    @abstractmethod
    def get_active_deals_for_players(
        self,
        guild_id: int | None,
        player_ids: list[int],
    ) -> list:
        """Get all active deals where BOTH buyer and partner are in player_ids."""
        ...

    @abstractmethod
    def get_user_deals(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> list:
        """Get all active deals created by a user."""
        ...

    @abstractmethod
    def decrement_deals(
        self,
        guild_id: int | None,
        deal_ids: list[int],
    ) -> int:
        """Decrement games_remaining for the given deal IDs."""
        ...

    @abstractmethod
    def delete_expired_deals(self, guild_id: int | None) -> int:
        """Delete deals with games_remaining = 0."""
        ...


class ITipRepository(ABC):
    """Repository for tip transaction logging."""

    @abstractmethod
    def log_tip(
        self,
        sender_id: int,
        recipient_id: int,
        amount: int,
        fee: int,
        guild_id: int | None,
    ) -> int:
        """Log a tip transaction. Returns the transaction ID."""
        ...

    @abstractmethod
    def get_tips_by_sender(
        self, sender_id: int, guild_id: int | None = None, limit: int = 10
    ) -> list[dict]:
        """Get tips sent by a user."""
        ...

    @abstractmethod
    def get_tips_by_recipient(
        self, recipient_id: int, guild_id: int | None = None, limit: int = 10
    ) -> list[dict]:
        """Get tips received by a user."""
        ...

    @abstractmethod
    def get_total_fees_collected(self, guild_id: int | None = None) -> int:
        """Get total fees collected from tips."""
        ...

    @abstractmethod
    def get_top_senders(self, guild_id: int | None, limit: int = 10) -> list[dict]:
        """Get top tip senders ranked by total amount sent."""
        ...

    @abstractmethod
    def get_top_receivers(self, guild_id: int | None, limit: int = 10) -> list[dict]:
        """Get top tip receivers ranked by total amount received."""
        ...

    @abstractmethod
    def get_user_tip_stats(self, discord_id: int, guild_id: int | None) -> dict:
        """Get individual user's tip statistics."""
        ...

    @abstractmethod
    def get_total_tip_volume(self, guild_id: int | None) -> dict:
        """Get server-wide tip statistics."""
        ...


class IWrappedRepository(ABC):
    """Repository for Cama Wrapped monthly summary data access."""

    @abstractmethod
    def get_wrapped(self, guild_id: int, year_month: str) -> dict | None:
        """Get existing wrapped generation record for a guild/month."""
        ...

    @abstractmethod
    def get_last_generation(self, guild_id: int) -> dict | None:
        """Get the most recent wrapped generation for a guild."""
        ...

    @abstractmethod
    def save_wrapped(
        self,
        guild_id: int,
        year_month: str,
        stats: dict,
        channel_id: int | None = None,
        message_id: int | None = None,
        generated_by: int | None = None,
        generation_type: str = "auto",
    ) -> int:
        """Save wrapped generation record."""
        ...

    @abstractmethod
    def get_month_match_stats(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """Get match participation stats for a time period."""
        ...

    @abstractmethod
    def get_month_hero_stats(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """Get hero pick stats for a time period."""
        ...

    @abstractmethod
    def get_month_player_heroes(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """Get per-player hero stats for a time period."""
        ...

    @abstractmethod
    def get_month_rating_changes(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """Get rating changes for players over a time period."""
        ...

    @abstractmethod
    def get_month_betting_stats(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """Get betting stats for players over a time period."""
        ...

    @abstractmethod
    def get_month_bankruptcy_count(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """Get bankruptcy counts for the period."""
        ...

    @abstractmethod
    def get_month_bets_against_player(
        self, guild_id: int, start_ts: int, end_ts: int
    ) -> list[dict]:
        """Get count of bets placed against each player's team."""
        ...

    @abstractmethod
    def get_month_summary(self, guild_id: int, start_ts: int, end_ts: int) -> dict:
        """Get high-level summary stats for the month."""
        ...


class IAIQueryRepository(ABC):
    """Repository for executing AI-generated SQL queries safely."""

    @abstractmethod
    def execute_readonly(
        self,
        sql: str,
        params: tuple = (),
        max_rows: int = 25,
    ) -> list[dict]:
        """Execute a validated SQL query in read-only mode."""
        ...

    @abstractmethod
    def get_table_schema(self, table_name: str) -> list[dict]:
        """Get schema information for a table."""
        ...

    @abstractmethod
    def get_all_tables(self) -> list[str]:
        """Get list of all tables in the database."""
        ...

    @abstractmethod
    def get_foreign_keys(self, table_name: str) -> list[dict]:
        """Get foreign key relationships for a table."""
        ...


class IDisburseRepository(ABC):
    """Repository for managing nonprofit fund disbursement proposals and votes."""

    @abstractmethod
    def get_active_proposal(self, guild_id: int | None) -> dict | None:
        """Get the active proposal for a guild, if any."""
        ...

    @abstractmethod
    def create_proposal(
        self,
        guild_id: int | None,
        proposal_id: int,
        fund_amount: int,
        quorum_required: int,
    ) -> None:
        """Create a new disbursement proposal."""
        ...

    @abstractmethod
    def set_proposal_message(
        self, guild_id: int | None, message_id: int, channel_id: int
    ) -> None:
        """Set the Discord message ID for an active proposal."""
        ...

    @abstractmethod
    def add_vote(
        self,
        guild_id: int | None,
        proposal_id: int,
        discord_id: int,
        method: str,
    ) -> None:
        """Add or update a vote for a disbursement proposal."""
        ...

    @abstractmethod
    def get_vote_counts(self, guild_id: int | None) -> dict[str, int]:
        """Get vote counts for each method for the active proposal."""
        ...

    @abstractmethod
    def get_total_votes(self, guild_id: int | None) -> int:
        """Get total number of votes for the active proposal."""
        ...

    @abstractmethod
    def get_voter_ids(self, guild_id: int | None) -> list[int]:
        """Get list of discord_ids who have voted on the active proposal."""
        ...

    @abstractmethod
    def get_individual_votes(self, guild_id: int | None) -> list[dict]:
        """Get individual vote details for the active proposal."""
        ...

    @abstractmethod
    def complete_proposal(self, guild_id: int | None) -> None:
        """Mark the active proposal as completed."""
        ...

    @abstractmethod
    def reset_proposal(self, guild_id: int | None) -> bool:
        """Reset (cancel) the active proposal."""
        ...

    @abstractmethod
    def record_disbursement(
        self,
        guild_id: int | None,
        total_amount: int,
        method: str,
        distributions: list[tuple[int, int]],
    ) -> int:
        """Record a completed disbursement for history."""
        ...

    @abstractmethod
    def get_last_disbursement(self, guild_id: int | None) -> dict | None:
        """Get the most recent disbursement for a guild."""
        ...
