"""
Abstract repository interfaces for data access.

These interfaces define the contracts implemented by concrete repositories.
"""

from abc import ABC, abstractmethod
from typing import Dict, List, Optional, Tuple


class IPlayerRepository(ABC):
    @abstractmethod
    def add(
        self,
        discord_id: int,
        discord_username: str,
        dotabuff_url: Optional[str] = None,
        initial_mmr: Optional[int] = None,
        preferred_roles: Optional[List[str]] = None,
        main_role: Optional[str] = None,
        glicko_rating: Optional[float] = None,
        glicko_rd: Optional[float] = None,
        glicko_volatility: Optional[float] = None,
    ) -> None:
        ...

    @abstractmethod
    def get_by_id(self, discord_id: int):
        ...

    @abstractmethod
    def get_by_ids(self, discord_ids: List[int]):
        ...

    @abstractmethod
    def get_by_username(self, username: str):
        ...

    @abstractmethod
    def get_all(self):
        ...

    @abstractmethod
    def exists(self, discord_id: int) -> bool:
        ...

    @abstractmethod
    def update_roles(self, discord_id: int, roles: List[str]) -> None:
        ...

    @abstractmethod
    def update_glicko_rating(self, discord_id: int, rating: float, rd: float, volatility: float) -> None:
        ...

    @abstractmethod
    def get_glicko_rating(self, discord_id: int) -> Optional[Tuple[float, float, float]]:
        ...

    @abstractmethod
    def update_mmr(self, discord_id: int, new_mmr: float) -> None:
        ...

    @abstractmethod
    def get_balance(self, discord_id: int) -> int:
        ...

    @abstractmethod
    def update_balance(self, discord_id: int, amount: int) -> None:
        ...

    @abstractmethod
    def add_balance(self, discord_id: int, amount: int) -> None:
        ...

    @abstractmethod
    def increment_wins(self, discord_id: int) -> None:
        ...

    @abstractmethod
    def increment_losses(self, discord_id: int) -> None:
        ...

    @abstractmethod
    def get_exclusion_counts(self, discord_ids: List[int]) -> Dict[int, int]:
        ...

    @abstractmethod
    def increment_exclusion_count(self, discord_id: int) -> None:
        ...

    @abstractmethod
    def decay_exclusion_count(self, discord_id: int) -> None:
        ...

    @abstractmethod
    def delete(self, discord_id: int) -> bool:
        ...

    @abstractmethod
    def delete_all(self) -> int:
        ...

    @abstractmethod
    def delete_fake_users(self) -> int:
        ...

    @abstractmethod
    def get_by_steam_id(self, steam_id: int):
        """Get player by Steam ID (32-bit account_id)."""
        ...

    @abstractmethod
    def get_steam_id(self, discord_id: int) -> Optional[int]:
        """Get a player's Steam ID."""
        ...

    @abstractmethod
    def set_steam_id(self, discord_id: int, steam_id: int) -> None:
        """Set a player's Steam ID."""
        ...

    @abstractmethod
    def get_all_with_dotabuff_no_steam_id(self) -> List[Dict]:
        """Get all players with dotabuff_url but no steam_id set."""
        ...


class IBetRepository(ABC):
    VALID_TEAMS: set

    @abstractmethod
    def create_bet(self, guild_id: Optional[int], discord_id: int, team: str, amount: int, bet_time: int) -> int:
        ...

    @abstractmethod
    def get_player_pending_bet(self, guild_id: Optional[int], discord_id: int, since_ts: Optional[int] = None):
        ...

    @abstractmethod
    def get_bets_for_pending_match(self, guild_id: Optional[int], since_ts: Optional[int] = None):
        ...

    @abstractmethod
    def delete_bets_for_guild(self, guild_id: Optional[int]) -> int:
        ...

    @abstractmethod
    def get_total_bets_by_guild(self, guild_id: Optional[int], since_ts: Optional[int] = None) -> Dict[str, int]:
        ...

    @abstractmethod
    def assign_match_id(self, guild_id: Optional[int], match_id: int, since_ts: Optional[int] = None) -> None:
        ...

    @abstractmethod
    def delete_pending_bets(self, guild_id: Optional[int], since_ts: Optional[int] = None) -> int:
        ...


class IMatchRepository(ABC):
    @abstractmethod
    def record_match(
        self,
        team1_ids: List[int],
        team2_ids: List[int],
        winning_team: int,
        radiant_team_ids: Optional[List[int]] = None,
        dire_team_ids: Optional[List[int]] = None,
        dotabuff_match_id: Optional[str] = None,
        notes: Optional[str] = None,
    ) -> int:
        ...

    @abstractmethod
    def add_rating_history(self, discord_id: int, rating: float, match_id: Optional[int] = None) -> None:
        ...

    @abstractmethod
    def get_match(self, match_id: int):
        ...

    @abstractmethod
    def get_player_matches(self, discord_id: int, limit: int = 10):
        ...

    @abstractmethod
    def get_rating_history(self, discord_id: int, limit: int = 20):
        ...

    @abstractmethod
    def delete_all_matches(self) -> int:
        ...

    @abstractmethod
    def save_pending_match(self, guild_id: Optional[int], payload: Dict) -> None:
        ...

    @abstractmethod
    def get_pending_match(self, guild_id: Optional[int]) -> Optional[Dict]:
        ...

    @abstractmethod
    def clear_pending_match(self, guild_id: Optional[int]) -> None:
        ...

    @abstractmethod
    def consume_pending_match(self, guild_id: Optional[int]) -> Optional[Dict]:
        ...

    @abstractmethod
    def get_player_hero_stats(self, discord_id: int) -> Dict:
        """Get hero statistics for a player from enriched matches."""
        ...


class ILobbyRepository(ABC):
    @abstractmethod
    def save_lobby_state(
        self,
        lobby_id: int,
        players: List[int],
        status: str,
        created_by: int,
        created_at: str,
        message_id: Optional[int] = None,
        channel_id: Optional[int] = None,
    ) -> None:
        ...

    @abstractmethod
    def load_lobby_state(self, lobby_id: int) -> Optional[Dict]:
        ...

    @abstractmethod
    def clear_lobby_state(self, lobby_id: int) -> None:
        ...


class IPairingsRepository(ABC):
    @abstractmethod
    def update_pairings_for_match(
        self,
        match_id: int,
        team1_ids: List[int],
        team2_ids: List[int],
        winning_team: int,
    ) -> None:
        """Update pairwise statistics for all player pairs in a match."""
        ...

    @abstractmethod
    def get_pairings_for_player(self, discord_id: int) -> List[Dict]:
        """Get all pairwise stats involving a player."""
        ...

    @abstractmethod
    def get_best_teammates(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get players with highest win rate when on same team."""
        ...

    @abstractmethod
    def get_worst_teammates(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get players with lowest win rate when on same team."""
        ...

    @abstractmethod
    def get_best_matchups(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get players with highest win rate when on opposing teams."""
        ...

    @abstractmethod
    def get_worst_matchups(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get players with lowest win rate when on opposing teams."""
        ...

    @abstractmethod
    def get_most_played_with(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get teammates sorted by most games played together."""
        ...

    @abstractmethod
    def get_most_played_against(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get opponents sorted by most games played against."""
        ...

    @abstractmethod
    def get_evenly_matched_teammates(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get teammates with exactly 50% win rate."""
        ...

    @abstractmethod
    def get_evenly_matched_opponents(self, discord_id: int, min_games: int = 3, limit: int = 5) -> List[Dict]:
        """Get opponents with exactly 50% win rate."""
        ...

    @abstractmethod
    def get_pairing_counts(self, discord_id: int, min_games: int = 1) -> Dict:
        """Get total counts of unique teammates and opponents."""
        ...

    @abstractmethod
    def get_head_to_head(self, player1_id: int, player2_id: int) -> Optional[Dict]:
        """Get detailed stats between two specific players."""
        ...

    @abstractmethod
    def rebuild_all_pairings(self) -> int:
        """Recalculate all pairings from match history. Returns count of pairings updated."""
        ...


class IGuildConfigRepository(ABC):
    @abstractmethod
    def get_config(self, guild_id: int) -> Optional[Dict]:
        """Get configuration for a guild."""
        ...

    @abstractmethod
    def set_league_id(self, guild_id: int, league_id: int) -> None:
        """Set the league ID for a guild."""
        ...

    @abstractmethod
    def get_league_id(self, guild_id: int) -> Optional[int]:
        """Get the league ID for a guild."""
        ...

