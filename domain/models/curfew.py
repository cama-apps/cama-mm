"""
Curfew window domain model.
"""

from dataclasses import dataclass


@dataclass
class CurfewWindow:
    """A named recurring daily window during which a player is unavailable to queue.

    A player can have any number of these under different names — the name is
    entirely up to them (e.g. "work", "night shift", whatever fits). Each one
    independently locks /lobby join and auto-removes the player from any
    lobby once it starts. See services/curfew_service.py.
    """

    discord_id: int
    guild_id: int
    name: str
    start_hour: int
    start_minute: int
    end_hour: int
    end_minute: int
    timezone: str | None = None  # overrides the player's general timezone if set
