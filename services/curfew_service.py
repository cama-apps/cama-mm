"""
Curfew enforcement: sweeps active lobbies for players whose curfew window
has started and removes them, so "auto-unqueue at bedtime" doesn't depend on
the player taking any action.

Blocking a fresh /lobby join during curfew is handled inline in the command
(cheap: one already-loaded Player). This sweep instead exists for the case
the join check can't cover — a player already queued when their curfew
window starts a few minutes later.
"""

import logging
from dataclasses import dataclass
from datetime import datetime

from domain.models.lobby import LobbyKind
from utils.curfew import is_within_curfew

logger = logging.getLogger("cama_bot.curfew_service")


@dataclass(frozen=True)
class CurfewKick:
    discord_id: int
    guild_id: int
    lobby_kind: LobbyKind


class CurfewService:
    def __init__(self, player_repo, lobby_service):
        self.player_repo = player_repo
        self.lobby_service = lobby_service

    def sweep(self, guild_ids: list[int], *, now: datetime | None = None) -> list[CurfewKick]:
        """Remove any lobby member currently inside their curfew window.

        Returns the players actually removed, for the caller to DM. ``now``
        defaults to the real current time; tests can inject a fixed instant.
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

        players = self.player_repo.get_by_ids(list(lobby.players), guild_id)
        due_ids = {p.discord_id for p in players if is_within_curfew(p, now=now)}
        if not due_ids:
            return []

        removed = self.lobby_service.remove_players_from_lobby(due_ids, guild_id, lobby_kind)
        return [
            CurfewKick(discord_id=discord_id, guild_id=guild_id, lobby_kind=lobby_kind)
            for discord_id in removed
        ]
