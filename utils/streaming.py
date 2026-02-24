"""Streaming detection helper for Go Live + Dota 2 activity."""

import discord

DOTA2_KEYWORDS = ("dota",)


def get_streaming_dota_player_ids(
    guild: discord.Guild, player_ids: list[int]
) -> set[int]:
    """Return player IDs that are Go Live + playing Dota 2 in a voice channel."""
    streaming = set()
    for pid in player_ids:
        member = guild.get_member(pid)
        if not member:
            continue
        # Check Go Live (screen sharing in a voice channel)
        if not (member.voice and member.voice.self_stream):
            continue
        # Check Dota 2 activity (Game is typical, but Activity with playing type
        # can also appear on some Discord client versions)
        if any(
            isinstance(a, (discord.Game, discord.Streaming, discord.Activity))
            and getattr(a, "name", None)
            and any(kw in a.name.lower() for kw in DOTA2_KEYWORDS)
            for a in member.activities
        ):
            streaming.add(pid)
    return streaming
