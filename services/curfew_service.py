"""
Curfew-window management and enforcement.

A player can register any number of named windows (e.g. "work", "night
shift" — the name and purpose are entirely up to them). Each independently
blocks a fresh /lobby join and gets swept out of any lobby they're already
queued in once the window starts.
"""

import logging
from dataclasses import dataclass
from datetime import datetime

from domain.models.curfew import CurfewWindow
from domain.models.lobby import LobbyKind
from utils.curfew import find_active_window
from utils.timezone import is_valid_timezone

logger = logging.getLogger("cama_bot.curfew_service")

MAX_WINDOW_NAME_LENGTH = 40


@dataclass(frozen=True)
class CurfewKick:
    discord_id: int
    guild_id: int
    lobby_kind: LobbyKind
    window_name: str


class CurfewService:
    def __init__(self, player_repo, curfew_repo, lobby_service):
        self.player_repo = player_repo
        self.curfew_repo = curfew_repo
        self.lobby_service = lobby_service

    # ------------------------------------------------------------------
    # Window management
    # ------------------------------------------------------------------

    def add_window(
        self,
        discord_id: int,
        guild_id: int,
        name: str,
        *,
        start_hour: int,
        start_minute: int,
        end_hour: int,
        end_minute: int,
        timezone: str | None = None,
    ) -> CurfewWindow:
        """Create a window, or overwrite it if the player already has one by that name."""
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if not player:
            raise ValueError("Player not registered.")
        name = name.strip()
        if not name:
            raise ValueError("Give this window a name.")
        if len(name) > MAX_WINDOW_NAME_LENGTH:
            raise ValueError(f"Name must be {MAX_WINDOW_NAME_LENGTH} characters or fewer.")
        for hour, minute in ((start_hour, start_minute), (end_hour, end_minute)):
            if not (0 <= hour <= 23):
                raise ValueError("Hour must be between 0 and 23.")
            if not (0 <= minute <= 59):
                raise ValueError("Minute must be between 0 and 59.")
        if start_hour == end_hour and start_minute == end_minute:
            raise ValueError("Start and end time can't be the same.")
        if timezone is not None and not is_valid_timezone(timezone):
            raise ValueError(
                f"Unknown timezone '{timezone}'. Use an IANA name like 'America/New_York'."
            )
        self.curfew_repo.add_or_replace(
            discord_id,
            guild_id,
            name,
            start_hour=start_hour,
            start_minute=start_minute,
            end_hour=end_hour,
            end_minute=end_minute,
            timezone=timezone,
        )
        return CurfewWindow(
            discord_id=discord_id,
            guild_id=guild_id,
            name=name,
            start_hour=start_hour,
            start_minute=start_minute,
            end_hour=end_hour,
            end_minute=end_minute,
            timezone=timezone,
        )

    def remove_window(self, discord_id: int, guild_id: int, name: str) -> bool:
        """Delete a named window. Returns True if it existed."""
        return self.curfew_repo.remove(discord_id, guild_id, name.strip())

    def list_windows(self, discord_id: int, guild_id: int):
        return self.curfew_repo.list_for_player(discord_id, guild_id)

    # ------------------------------------------------------------------
    # Enforcement
    # ------------------------------------------------------------------

    def active_window(self, discord_id: int, guild_id: int, *, now: datetime | None = None):
        """Return the player's currently-active window, if any."""
        windows = self.curfew_repo.list_for_player(discord_id, guild_id)
        if not windows:
            return None
        player = self.player_repo.get_by_id(discord_id, guild_id)
        general_timezone = player.timezone if player else None
        return find_active_window(windows, general_timezone=general_timezone, now=now)

    def sweep(self, guild_ids: list[int], *, now: datetime | None = None) -> list[CurfewKick]:
        """Remove any lobby member currently inside one of their windows.

        Returns the players actually removed, for the caller to notify.
        ``now`` defaults to the real current time; tests can inject a fixed instant.
        """
        kicks: list[CurfewKick] = []
        for guild_id in guild_ids:
            for lobby_kind in (LobbyKind.OPEN, LobbyKind.LOWSKILL):
                kicks.extend(self._sweep_lobby(guild_id, lobby_kind, now))
        return kicks

    def _sweep_lobby(
        self, guild_id: int, lobby_kind: LobbyKind, now: datetime | None
    ) -> list[CurfewKick]:
        try:
            lobby = self.lobby_service.get_lobby(guild_id=guild_id, lobby_kind=lobby_kind)
        except Exception:
            logger.exception(
                "Failed to load lobby for curfew sweep (guild_id=%s, kind=%s)",
                guild_id,
                lobby_kind,
            )
            return []
        if not lobby or not lobby.players:
            return []

        windows_by_player = self.curfew_repo.list_for_players(list(lobby.players), guild_id)
        if not windows_by_player:
            return []
        players = self.player_repo.get_by_ids(list(windows_by_player.keys()), guild_id)
        timezone_by_id = {p.discord_id: p.timezone for p in players}

        due: dict[int, str] = {}
        for discord_id, windows in windows_by_player.items():
            match = find_active_window(
                windows, general_timezone=timezone_by_id.get(discord_id), now=now
            )
            if match:
                due[discord_id] = match.name
        if not due:
            return []

        removed = self.lobby_service.remove_players_from_lobby(set(due), guild_id, lobby_kind)
        return [
            CurfewKick(
                discord_id=discord_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
                window_name=due[discord_id],
            )
            for discord_id in removed
        ]
