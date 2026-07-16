"""
Channel-activity tracking via periodic sweeps.

Instead of listening to every message/voice event, background loops periodically
sweep the channels the bot can see and bump ``players.last_active_at`` for any
registered player who is currently present. Voice occupancy is read from the
gateway cache (free); text presence is read from recent message history (one small
REST fetch per active channel, bounded by a per-channel watermark).

A player counts as "active in voice" only if they are not a bot, not sitting in the
server's AFK channel, and not deafened (server-deaf or self-deaf). Mute/self-mute
still counts — someone can listen and hang out without talking.
"""

import asyncio
import logging
from datetime import UTC, datetime, timedelta

import discord

logger = logging.getLogger("cama_bot.activity_service")


class ActivityService:
    def __init__(self, player_repo):
        self._player_repo = player_repo
        # Per-channel watermark for text sweeps: (guild_id, channel_id) -> UTC datetime.
        # In-memory only; resets to a one-interval lookback on restart.
        self._text_watermarks: dict[tuple[int, int], datetime] = {}

    async def sweep_voice(self, guild: discord.Guild) -> int:
        """
        Bump every registered member currently sitting in a viewable, non-AFK
        voice channel (excluding bots and deafened members). Returns the number
        of rows the repo reported as updated (best-effort).
        """
        me = guild.me
        if me is None:
            return 0
        afk_channel = guild.afk_channel
        active_ids: set[int] = set()
        for channel in guild.voice_channels:
            if channel == afk_channel:
                continue
            if not channel.permissions_for(me).view_channel:
                continue
            for member in channel.members:
                if self._counts_as_voice_active(member):
                    active_ids.add(member.id)
        if not active_ids:
            return 0
        return await asyncio.to_thread(
            self._player_repo.bump_last_active_many, list(active_ids), guild.id
        )

    async def sweep_text(self, guild: discord.Guild, lookback_seconds: int) -> int:
        """
        Bump registered members who authored a message in a viewable text
        channel since the last text sweep (per-channel watermark). Returns the
        number of rows the repo reported as updated (best-effort).
        """
        me = guild.me
        if me is None:
            return 0
        now = datetime.now(UTC)
        active_ids: set[int] = set()
        for channel in guild.text_channels:
            perms = channel.permissions_for(me)
            if not (perms.view_channel and perms.read_message_history):
                continue
            key = (guild.id, channel.id)
            after = self._text_watermarks.get(key, now - timedelta(seconds=lookback_seconds))
            try:
                async for message in channel.history(after=after, limit=None, oldest_first=False):
                    if not message.author.bot:
                        active_ids.add(message.author.id)
            except (discord.Forbidden, discord.HTTPException):
                # Perms changed mid-sweep or a transient API error: skip this
                # channel, but still advance the watermark below so we don't keep
                # re-scanning the same window forever.
                pass
            self._text_watermarks[key] = now
        if not active_ids:
            return 0
        return await asyncio.to_thread(
            self._player_repo.bump_last_active_many, list(active_ids), guild.id
        )

    @staticmethod
    def _counts_as_voice_active(member: discord.Member) -> bool:
        """A voice member counts as active unless bot or deafened."""
        if member.bot:
            return False
        voice = member.voice
        if voice is None:
            return False
        return not (voice.deaf or voice.self_deaf)
