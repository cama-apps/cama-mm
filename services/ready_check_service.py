"""
ReadyCheckService: Manages ready check sessions with button confirmations.

Tracks ephemeral state for active ready checks, including:
- Which players have clicked "Ready"
- Designated player for AFK removal permissions
- Admin presence in lobby
"""

import logging
from dataclasses import dataclass, field

logger = logging.getLogger("cama_bot.services.ready_check")


@dataclass
class ReadyCheckState:
    """State for an active ready check session."""

    guild_id: int
    ready_players: set[int] = field(default_factory=set)
    total_players: set[int] = field(default_factory=set)
    designated_player_id: int | None = None  # None if admin present
    admin_in_lobby: bool = False  # Track if admin is present
    status_message_id: int | None = None  # Message to update when players click Ready
    status_channel_id: int | None = None  # Channel containing status message
    online_players: list[int] = field(default_factory=list)  # Players online at check time
    voice_players: list[int] = field(default_factory=list)  # Players in voice at check time


class ReadyCheckService:
    """Manages ready check sessions across guilds."""

    def __init__(self):
        self._active_checks: dict[int, ReadyCheckState] = {}  # guild_id -> state

    def start_check(
        self,
        guild_id: int,
        player_ids: list[int],
        designated_player_id: int | None,
        admin_in_lobby: bool,
    ) -> ReadyCheckState:
        """
        Start a new ready check session.

        Args:
            guild_id: Discord guild ID
            player_ids: List of player Discord IDs in lobby
            designated_player_id: Designated player ID (None if admin present)
            admin_in_lobby: Whether an admin is in the lobby

        Returns:
            ReadyCheckState for this session
        """
        state = ReadyCheckState(
            guild_id=guild_id,
            ready_players=set(),
            total_players=set(player_ids),
            designated_player_id=designated_player_id,
            admin_in_lobby=admin_in_lobby,
        )
        self._active_checks[guild_id] = state
        logger.info(
            f"Started ready check for guild {guild_id}: {len(player_ids)} players, "
            f"designated={designated_player_id}, admin={admin_in_lobby}"
        )
        return state

    def mark_ready(self, guild_id: int, player_id: int) -> bool:
        """
        Mark a player as ready.

        Args:
            guild_id: Discord guild ID
            player_id: Discord user ID

        Returns:
            True if player was marked ready, False if not in active check
        """
        state = self._active_checks.get(guild_id)
        if not state:
            logger.warning(f"No active ready check for guild {guild_id}")
            return False

        if player_id not in state.total_players:
            logger.warning(f"Player {player_id} not in lobby for guild {guild_id}")
            return False

        state.ready_players.add(player_id)
        logger.info(
            f"Player {player_id} marked ready in guild {guild_id} "
            f"({len(state.ready_players)}/{len(state.total_players)} ready)"
        )
        return True

    def get_state(self, guild_id: int) -> ReadyCheckState | None:
        """
        Get active ready check state for a guild.

        Args:
            guild_id: Discord guild ID

        Returns:
            ReadyCheckState if active, None otherwise
        """
        return self._active_checks.get(guild_id)

    def update_designated_player(self, guild_id: int, new_designated_player_id: int) -> None:
        """
        Update designated player during active ready check.

        Used when current designated player becomes AFK and needs to be replaced.

        Args:
            guild_id: Discord guild ID
            new_designated_player_id: New designated player Discord ID
        """
        state = self._active_checks.get(guild_id)
        if not state:
            logger.warning(f"No active ready check for guild {guild_id}")
            return

        old_id = state.designated_player_id
        state.designated_player_id = new_designated_player_id
        logger.info(
            f"Updated designated player in guild {guild_id}: {old_id} -> {new_designated_player_id}"
        )

    def set_status_message(
        self,
        guild_id: int,
        message_id: int,
        channel_id: int,
        online_players: list[int],
        voice_players: list[int],
    ) -> None:
        """
        Store the status message for updates when players click Ready.

        Args:
            guild_id: Discord guild ID
            message_id: Status embed message ID
            channel_id: Channel ID containing the message
            online_players: List of player IDs who were online at check time
            voice_players: List of player IDs who were in voice at check time
        """
        state = self._active_checks.get(guild_id)
        if not state:
            logger.warning(f"No active ready check for guild {guild_id}")
            return

        state.status_message_id = message_id
        state.status_channel_id = channel_id
        state.online_players = online_players
        state.voice_players = voice_players
        logger.info(f"Set status message {message_id} for guild {guild_id}")

    def cancel_check(self, guild_id: int) -> None:
        """
        Cancel active ready check for a guild.

        Args:
            guild_id: Discord guild ID
        """
        if guild_id in self._active_checks:
            del self._active_checks[guild_id]
            logger.info(f"Cancelled ready check for guild {guild_id}")
        else:
            logger.debug(f"No active ready check to cancel for guild {guild_id}")
