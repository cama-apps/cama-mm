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


class ILobbyRepository(ABC):
    @abstractmethod
    def save_lobby_state(self, lobby_id: int, players: List[int], status: str, created_by: int, created_at: str) -> None:
        ...

    @abstractmethod
    def load_lobby_state(self, lobby_id: int) -> Optional[Dict]:
        ...

    @abstractmethod
    def clear_lobby_state(self, lobby_id: int) -> None:
        ...

