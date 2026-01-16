"""Service for managing ready check lifecycle."""

import logging
from datetime import datetime
from threading import Lock
from typing import Dict

import discord

from domain.models.ready_check import ReadyCheck, ReadyCheckStatus, ReadyStatus
from services.lobby_service import LobbyService

logger = logging.getLogger("cama_bot.services.ready_check")


class ReadyCheckService:
    """Manages ready check lifecycle for lobby players."""

    def __init__(
        self,
        lobby_service: LobbyService,
        timeout_seconds: int = 60,
        voice_auto_ready_enabled: bool = True,
    ):
        self.lobby_service = lobby_service
        self.timeout_seconds = timeout_seconds
        self.voice_auto_ready_enabled = voice_auto_ready_enabled

        # In-memory storage per guild (no DB persistence needed for 60s checks)
        self._active_checks: Dict[int, ReadyCheck] = {}
        self._lock = Lock()

        # Track message IDs for embed updates
        self._message_ids: Dict[int, tuple[int, int]] = {}  # guild_id -> (message_id, channel_id)

    def start_check(
        self,
        guild_id: int | None,
        player_ids: list[int],
        guild: discord.Guild | None = None,
    ) -> ReadyCheck:
        """
        Start a ready check for the given players.

        Args:
            guild_id: Guild ID (normalized to 0 for None)
            player_ids: List of player Discord IDs
            guild: Discord Guild object for voice state detection

        Returns:
            ReadyCheck object
        """
        normalized_guild_id = guild_id if guild_id is not None else 0

        with self._lock:
            # Cancel any existing check for this guild
            if normalized_guild_id in self._active_checks:
                self._active_checks[normalized_guild_id].status = ReadyCheckStatus.CANCELLED
                logger.info(f"Cancelled existing ready check for guild {guild_id}")

            # Initialize ready states
            player_ready_states = {}
            auto_ready_count = 0

            for pid in player_ids:
                # Check voice state for auto-ready
                if self.voice_auto_ready_enabled and guild:
                    member = guild.get_member(pid)
                    if member and self._is_voice_ready(member):
                        player_ready_states[pid] = ReadyStatus.AUTO_READY
                        auto_ready_count += 1
                        logger.debug(f"Auto-marked player {pid} as ready (in voice)")
                    else:
                        player_ready_states[pid] = ReadyStatus.UNCONFIRMED
                else:
                    player_ready_states[pid] = ReadyStatus.UNCONFIRMED

            # Create ready check
            ready_check = ReadyCheck(
                guild_id=normalized_guild_id,
                started_at=datetime.now(),
                timeout_seconds=self.timeout_seconds,
                player_ready_states=player_ready_states,
                voice_auto_ready_enabled=self.voice_auto_ready_enabled,
            )

            self._active_checks[normalized_guild_id] = ready_check
            logger.info(
                f"Started ready check for guild {guild_id}: "
                f"{len(player_ready_states)} players, "
                f"{auto_ready_count} auto-ready via voice"
            )
            return ready_check

    def _is_voice_ready(self, member: discord.Member) -> bool:
        """
        Check if member is in a voice channel and NOT deafened.

        Args:
            member: Discord Member object

        Returns:
            True if in voice and not deafened
        """
        if not member.voice:
            return False
        # self_deaf = user deafened themselves
        # deaf = server deafened
        is_deafened = member.voice.self_deaf or member.voice.deaf
        return not is_deafened

    def get_check(self, guild_id: int | None) -> ReadyCheck | None:
        """Get active ready check for guild."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            return self._active_checks.get(normalized_guild_id)

    def mark_ready(
        self, guild_id: int | None, discord_id: int
    ) -> tuple[bool, ReadyCheck | None]:
        """
        Mark a player as ready (button click).

        Returns:
            (success, ready_check)
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            ready_check = self._active_checks.get(normalized_guild_id)
            if not ready_check or ready_check.status != ReadyCheckStatus.ACTIVE:
                logger.debug(
                    f"Cannot mark player {discord_id} ready: no active check for guild {guild_id}"
                )
                return False, None

            changed = ready_check.mark_ready(discord_id, auto=False)
            if changed:
                logger.info(
                    f"Player {discord_id} marked ready via button (guild {guild_id})"
                )
            return changed, ready_check

    def check_timeout(self, guild_id: int | None) -> tuple[bool, ReadyCheck | None]:
        """
        Check if ready check has timed out.

        Returns:
            (is_timed_out, ready_check)
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            ready_check = self._active_checks.get(normalized_guild_id)
            if not ready_check or ready_check.status != ReadyCheckStatus.ACTIVE:
                return False, None

            is_timeout = ready_check.is_timed_out(datetime.now())
            if is_timeout:
                ready_check.status = ReadyCheckStatus.TIMEOUT
                logger.info(f"Ready check timed out for guild {guild_id}")

            return is_timeout, ready_check

    def complete_check(self, guild_id: int | None) -> ReadyCheck | None:
        """
        Mark ready check as completed and clean up.

        Returns:
            The completed ReadyCheck
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            ready_check = self._active_checks.get(normalized_guild_id)
            if ready_check:
                ready_check.status = ReadyCheckStatus.COMPLETED
                del self._active_checks[normalized_guild_id]
                logger.info(f"Ready check completed for guild {guild_id}")
            return ready_check

    def cancel_check(self, guild_id: int | None) -> None:
        """Cancel and clean up ready check."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            ready_check = self._active_checks.pop(normalized_guild_id, None)
            if ready_check:
                ready_check.status = ReadyCheckStatus.CANCELLED
                logger.info(f"Ready check cancelled for guild {guild_id}")

    def kick_unready_players(self, guild_id: int | None) -> list[int]:
        """
        Remove unready players from lobby after timeout.

        Returns:
            List of kicked player IDs
        """
        ready_check = self.get_check(guild_id)
        if not ready_check:
            return []

        unready_ids = list(ready_check.get_unready_players())
        if not unready_ids:
            return []

        logger.info(
            f"Kicking {len(unready_ids)} unready players from guild {guild_id}: {unready_ids}"
        )

        # Remove from lobby
        for pid in unready_ids:
            self.lobby_service.leave_lobby(pid)

        return unready_ids

    def set_message_id(
        self, guild_id: int | None, message_id: int, channel_id: int
    ) -> None:
        """Store message ID for embed updates."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            self._message_ids[normalized_guild_id] = (message_id, channel_id)

    def get_message_id(self, guild_id: int | None) -> tuple[int, int] | None:
        """Get stored message ID and channel ID."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            return self._message_ids.get(normalized_guild_id)

    def clear_message_id(self, guild_id: int | None) -> None:
        """Clear stored message ID."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self._lock:
            self._message_ids.pop(normalized_guild_id, None)
