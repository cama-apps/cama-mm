"""
Repository layer for data access abstraction.
"""

from repositories.base_repository import BaseRepository
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository
from repositories.bet_repository import BetRepository
from repositories.lobby_repository import LobbyRepository
from repositories.interfaces import (
    IPlayerRepository,
    IBetRepository,
    IMatchRepository,
    ILobbyRepository,
)

__all__ = [
    "BaseRepository",
    "PlayerRepository",
    "MatchRepository",
    "BetRepository",
    "LobbyRepository",
    "IPlayerRepository",
    "IBetRepository",
    "IMatchRepository",
    "ILobbyRepository",
]

