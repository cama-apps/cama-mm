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
        dotabuff_url: str | None = None,
        initial_mmr: int | None = None,
        preferred_roles: list[str] | None = None,
        main_role: str | None = None,
        glicko_rating: float | None = None,
        glicko_rd: float | None = None,
        glicko_volatility: float | None = None,
    ) -> None: ...

    @abstractmethod
    def get_by_id(self, discord_id: int): ...

    @abstractmethod
    def get_by_ids(self, discord_ids: list[int]): ...

    @abstractmethod
    def get_by_username(self, username: str): ...

    @abstractmethod
    def get_all(self): ...

    @abstractmethod
    def exists(self, discord_id: int) -> bool: ...

    @abstractmethod
    def update_roles(self, discord_id: int, roles: list[str]) -> None: ...

    @abstractmethod
    def update_glicko_rating(
        self, discord_id: int, rating: float, rd: float, volatility: float
    ) -> None: ...

    @abstractmethod
    def get_glicko_rating(self, discord_id: int) -> tuple[float, float, float] | None: ...

    @abstractmethod
    def update_mmr(self, discord_id: int, new_mmr: float) -> None: ...

    @abstractmethod
    def get_balance(self, discord_id: int) -> int: ...

    @abstractmethod
    def update_balance(self, discord_id: int, amount: int) -> None: ...

    @abstractmethod
    def add_balance(self, discord_id: int, amount: int) -> None: ...

    @abstractmethod
    def increment_wins(self, discord_id: int) -> None: ...

    @abstractmethod
    def increment_losses(self, discord_id: int) -> None: ...

    @abstractmethod
    def get_exclusion_counts(self, discord_ids: list[int]) -> dict[int, int]: ...

    @abstractmethod
    def increment_exclusion_count(self, discord_id: int) -> None: ...

    @abstractmethod
    def decay_exclusion_count(self, discord_id: int) -> None: ...

    @abstractmethod
    def delete(self, discord_id: int) -> bool: ...

    @abstractmethod
    def delete_all(self) -> int: ...

    @abstractmethod
    def delete_fake_users(self) -> int: ...

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


class IMatchRepository(ABC):
    @abstractmethod
    def record_match(
        self,
        team1_ids: list[int],
        team2_ids: list[int],
        winning_team: int,
        radiant_team_ids: list[int] | None = None,
        dire_team_ids: list[int] | None = None,
        dotabuff_match_id: str | None = None,
        notes: str | None = None,
    ) -> int: ...

    @abstractmethod
    def add_rating_history(
        self,
        discord_id: int,
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
    ) -> None: ...

    @abstractmethod
    def get_match(self, match_id: int): ...

    @abstractmethod
    def get_player_matches(self, discord_id: int, limit: int = 10): ...

    @abstractmethod
    def get_rating_history(self, discord_id: int, limit: int = 20): ...

    @abstractmethod
    def get_recent_rating_history(self, limit: int = 200): ...

    @abstractmethod
    def get_match_count(self) -> int: ...

    @abstractmethod
    def add_match_prediction(
        self,
        match_id: int,
        radiant_rating: float,
        dire_rating: float,
        radiant_rd: float,
        dire_rd: float,
        expected_radiant_win_prob: float,
    ) -> None: ...

    @abstractmethod
    def get_recent_match_predictions(self, limit: int = 200): ...

    @abstractmethod
    def delete_all_matches(self) -> int: ...

    @abstractmethod
    def save_pending_match(self, guild_id: int | None, payload: dict) -> None: ...

    @abstractmethod
    def get_pending_match(self, guild_id: int | None) -> dict | None: ...

    @abstractmethod
    def clear_pending_match(self, guild_id: int | None) -> None: ...

    @abstractmethod
    def consume_pending_match(self, guild_id: int | None) -> dict | None: ...

    @abstractmethod
    def get_player_hero_stats(self, discord_id: int) -> dict:
        """Get hero statistics for a player from enriched matches."""
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
    ) -> None:
        """Update pairwise statistics for all player pairs in a match."""
        ...

    @abstractmethod
    def get_pairings_for_player(self, discord_id: int) -> list[dict]:
        """Get all pairwise stats involving a player."""
        ...

    @abstractmethod
    def get_best_teammates(self, discord_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with highest win rate when on same team."""
        ...

    @abstractmethod
    def get_worst_teammates(
        self, discord_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get players with lowest win rate when on same team."""
        ...

    @abstractmethod
    def get_best_matchups(self, discord_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with highest win rate when on opposing teams."""
        ...

    @abstractmethod
    def get_worst_matchups(self, discord_id: int, min_games: int = 3, limit: int = 5) -> list[dict]:
        """Get players with lowest win rate when on opposing teams."""
        ...

    @abstractmethod
    def get_most_played_with(
        self, discord_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get teammates sorted by most games played together."""
        ...

    @abstractmethod
    def get_most_played_against(
        self, discord_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents sorted by most games played against."""
        ...

    @abstractmethod
    def get_evenly_matched_teammates(
        self, discord_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get teammates with exactly 50% win rate."""
        ...

    @abstractmethod
    def get_evenly_matched_opponents(
        self, discord_id: int, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents with exactly 50% win rate."""
        ...

    @abstractmethod
    def get_pairing_counts(self, discord_id: int, min_games: int = 1) -> dict:
        """Get total counts of unique teammates and opponents."""
        ...

    @abstractmethod
    def get_head_to_head(self, player1_id: int, player2_id: int) -> dict | None:
        """Get detailed stats between two specific players."""
        ...

    @abstractmethod
    def rebuild_all_pairings(self) -> int:
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
