"""Mafia subgame domain model."""

from dataclasses import dataclass, field
from enum import Enum


class MafiaPhase(str, Enum):
    SETUP = "SETUP"
    NIGHT = "NIGHT"
    DAY = "DAY"
    RESOLVED = "RESOLVED"


class MafiaRole(str, Enum):
    MAFIA = "MAFIA"
    DOCTOR = "DOCTOR"
    DETECTIVE = "DETECTIVE"
    VIGILANTE = "VIGILANTE"
    TOWNIE = "TOWNIE"
    JESTER = "JESTER"


class MafiaActionType(str, Enum):
    KILL = "KILL"
    SAVE = "SAVE"
    INVESTIGATE = "INVESTIGATE"
    VIG_KILL = "VIG_KILL"
    VOTE = "VOTE"


class MafiaTwist(str, Enum):
    BLOOD_MOON = "BLOOD_MOON"
    TOWN_HALL = "TOWN_HALL"
    MEMORY_FOG = "MEMORY_FOG"
    PLAGUE = "PLAGUE"


class MafiaWinner(str, Enum):
    TOWN = "TOWN"
    MAFIA = "MAFIA"
    JESTER = "JESTER"
    NONE = "NONE"


# Faction membership: which roles win when each side wins.
TOWN_ROLES = {MafiaRole.DOCTOR, MafiaRole.DETECTIVE, MafiaRole.VIGILANTE, MafiaRole.TOWNIE}
MAFIA_ROLES = {MafiaRole.MAFIA}


@dataclass
class MafiaPlayer:
    """A single roster slot in a Mafia game."""

    game_id: int
    discord_id: int
    guild_id: int
    role: MafiaRole
    is_godfather: bool = False
    hero_name: str | None = None
    is_alive: bool = True
    eliminated_phase: MafiaPhase | None = None
    eliminated_at: int | None = None
    acted: bool = False


@dataclass
class MafiaGame:
    """A single day's Mafia game state."""

    game_id: int
    guild_id: int
    game_date: str  # 'YYYY-MM-DD' from DigService._get_game_date()
    phase: MafiaPhase
    started_at: int  # unix ts
    roster_size: int
    twist_event: MafiaTwist | None = None
    night_ended_at: int | None = None
    day_ended_at: int | None = None
    winner: MafiaWinner | None = None
    entry_fee: int = 0
    payout_per_winner: int = 0
    mvp_id: int | None = None
    mafia_thread_id: int | None = None
    discussion_thread_id: int | None = None
    setup_message_id: int | None = None
    players: list[MafiaPlayer] = field(default_factory=list)
