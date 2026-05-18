"""Streaming detection helper — Go Live (screen-share) in voice."""

import discord


def get_streaming_player_ids(
    guild: discord.Guild, player_ids: list[int]
) -> set[int]:
    """Return player IDs that are Go Live (screen-sharing) in a voice channel."""
    streaming = set()
    for pid in player_ids:
        member = guild.get_member(pid)
        if member and member.voice and member.voice.self_stream:
            streaming.add(pid)
    return streaming
