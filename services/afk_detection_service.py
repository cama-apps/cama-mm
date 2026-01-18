"""
AFK Detection Service for monitoring player activity signals.

Checks multiple activity indicators to determine if lobby players are present or AFK:
- Discord online/DND status
- Voice channel presence (not deafened)
- Recent messages in lobby thread
- Recent ⚔️ reactions on lobby message
- Recent typing activity
"""

import logging
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone

import discord

from utils.typing_tracker import TypingTracker
from utils.reaction_tracker import ReactionTracker

logger = logging.getLogger("cama_bot.services.afk_detection")


@dataclass
class ActivityStatus:
    """
    Activity status for a single player.

    Attributes:
        discord_id: Player's Discord ID
        is_active: Whether player has any activity signals
        signals: List of activity signal names detected
        last_activity_time: Most recent activity timestamp (or None)
    """

    discord_id: int
    is_active: bool
    signals: list[str]
    last_activity_time: datetime | None


class AFKDetectionService:
    """
    Service for detecting AFK (away from keyboard) players in the lobby.

    Checks multiple activity signals to determine player presence.
    """

    def __init__(
        self,
        typing_tracker: TypingTracker,
        reaction_tracker: ReactionTracker,
    ):
        """
        Initialize AFK detection service.

        Args:
            typing_tracker: Tracker for recent typing events
            reaction_tracker: Tracker for reaction timestamps
        """
        self.typing_tracker = typing_tracker
        self.reaction_tracker = reaction_tracker

    async def check_player_activity(
        self,
        player_id: int,
        guild: discord.Guild,
        lobby_message_id: int | None,
        lobby_thread: discord.Thread | None,
        activity_window_seconds: int = 120,
    ) -> ActivityStatus:
        """
        Check all activity signals for a player.

        Args:
            player_id: Discord user ID
            guild: Discord guild object
            lobby_message_id: Lobby message ID for checking reactions
            lobby_thread: Lobby thread for checking messages
            activity_window_seconds: Time window for "recent" activity (default 2 min)

        Returns:
            ActivityStatus with detected signals and timestamps
        """
        signals = []
        last_activity = None

        # Get member object
        member = guild.get_member(player_id)
        if not member:
            logger.warning(f"Could not find member {player_id} in guild {guild.id}")
            return ActivityStatus(
                discord_id=player_id,
                is_active=False,
                signals=[],
                last_activity_time=None,
            )

        # Signal 1: Discord online/DND status
        if member.status in [discord.Status.online, discord.Status.dnd]:
            signals.append("online")
            last_activity = datetime.now()  # Status is current

        # Signal 2: Voice channel presence (not deafened)
        if member.voice:
            if not (member.voice.self_deaf or member.voice.deaf):
                signals.append("voice")
                last_activity = datetime.now()  # Voice is current

        # Signal 3: Typing indicator (last 10 seconds)
        if self.typing_tracker.is_typing_recently(
            guild.id, player_id, window_seconds=10
        ):
            signals.append("typing")
            last_activity = datetime.now()  # Typing is very recent

        # Signal 4: Recent messages in lobby thread
        if lobby_thread:
            has_msg, msg_time = await self._check_recent_messages(
                lobby_thread, player_id, activity_window_seconds
            )
            if has_msg:
                signals.append("recent_message")
                if not last_activity or msg_time > last_activity:
                    last_activity = msg_time

        # Signal 5: Recent ⚔️ reaction on lobby message
        if lobby_message_id:
            has_reaction, reaction_time = self.reaction_tracker.check_recent_reaction(
                lobby_message_id, player_id, activity_window_seconds
            )
            if has_reaction:
                signals.append("recent_reaction")
                if not last_activity or reaction_time > last_activity:
                    last_activity = reaction_time

        return ActivityStatus(
            discord_id=player_id,
            is_active=len(signals) > 0,
            signals=signals,
            last_activity_time=last_activity,
        )

    async def _check_recent_messages(
        self,
        thread: discord.Thread,
        player_id: int,
        window_seconds: int,
    ) -> tuple[bool, datetime | None]:
        """
        Check if player sent messages recently in the thread.

        Args:
            thread: Discord thread to check
            player_id: Discord user ID
            window_seconds: Time window in seconds

        Returns:
            (has_recent_message, most_recent_timestamp_or_none)
        """
        cutoff = datetime.now(timezone.utc) - timedelta(seconds=window_seconds)

        try:
            # Fetch recent message history
            async for message in thread.history(limit=100, after=cutoff):
                if message.author.id == player_id:
                    return True, message.created_at
        except Exception as exc:
            logger.warning(
                f"Failed to check message history in thread {thread.id}: {exc}"
            )

        return False, None

    def format_activity_status(self, status: ActivityStatus) -> str:
        """
        Format activity status for display.

        Args:
            status: ActivityStatus object

        Returns:
            Formatted string with emoji indicators
        """
        if not status.is_active:
            if status.last_activity_time:
                elapsed = datetime.now() - status.last_activity_time
                minutes = int(elapsed.total_seconds() / 60)
                return f"(last seen: {minutes}m ago)"
            else:
                return "(no recent activity)"

        # Map signal names to emojis
        signal_emojis = {
            "online": "🟢",
            "voice": "🎙️",
            "typing": "⌨️",
            "recent_message": "💬",
            "recent_reaction": "⚔️",
        }

        emoji_list = [signal_emojis.get(sig, sig) for sig in status.signals]
        return ", ".join(emoji_list)
