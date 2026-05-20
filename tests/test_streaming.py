"""Tests for utils/streaming.py — Go Live (screen-share) detection."""

from unittest.mock import MagicMock

import discord

from utils.streaming import get_streaming_player_ids


def _make_member(
    pid: int,
    *,
    in_voice: bool = False,
    self_stream: bool = False,
    activities: list | None = None,
) -> MagicMock:
    """Create a mock guild member with optional voice/activity state."""
    member = MagicMock(spec=discord.Member)
    member.id = pid

    if in_voice:
        voice = MagicMock()
        voice.self_stream = self_stream
        member.voice = voice
    else:
        member.voice = None

    member.activities = activities or []
    return member


def _game(name: str) -> discord.Game:
    return discord.Game(name=name)


class TestGetStreamingPlayerIds:
    """Go Live (screen-sharing in voice) is the only gate — game activity is ignored."""

    def _guild_with_members(self, members: dict[int, MagicMock]) -> MagicMock:
        guild = MagicMock(spec=discord.Guild)
        guild.get_member = lambda pid: members.get(pid)
        return guild

    def test_go_live_included(self):
        """A player who is Go Live in voice is included."""
        m = _make_member(1, in_voice=True, self_stream=True, activities=[_game("Dota 2")])
        guild = self._guild_with_members({1: m})

        result = get_streaming_player_ids(guild, [1])
        assert result == {1}

    def test_go_live_non_dota_game_included(self):
        """Go Live counts regardless of which game Discord reports."""
        m = _make_member(
            1, in_voice=True, self_stream=True, activities=[_game("Counter-Strike 2")]
        )
        guild = self._guild_with_members({1: m})

        result = get_streaming_player_ids(guild, [1])
        assert result == {1}

    def test_go_live_no_activities_included(self):
        """Go Live counts even when Discord reports no activity at all."""
        m = _make_member(1, in_voice=True, self_stream=True, activities=[])
        guild = self._guild_with_members({1: m})

        result = get_streaming_player_ids(guild, [1])
        assert result == {1}

    def test_in_voice_not_go_live_excluded(self):
        """A player in voice but not screen-sharing is excluded."""
        m = _make_member(1, in_voice=True, self_stream=False, activities=[_game("Dota 2")])
        guild = self._guild_with_members({1: m})

        result = get_streaming_player_ids(guild, [1])
        assert result == set()

    def test_not_in_voice_excluded(self):
        """A player not in any voice channel is excluded."""
        m = _make_member(1, in_voice=False, activities=[_game("Dota 2")])
        guild = self._guild_with_members({1: m})

        result = get_streaming_player_ids(guild, [1])
        assert result == set()

    def test_none_voice_state_excluded(self):
        """A player with no voice state is excluded."""
        m = _make_member(1, in_voice=False)
        guild = self._guild_with_members({1: m})

        result = get_streaming_player_ids(guild, [1])
        assert result == set()

    def test_member_not_found_excluded(self):
        """A player ID not present in the guild is excluded."""
        guild = self._guild_with_members({})

        result = get_streaming_player_ids(guild, [999])
        assert result == set()

    def test_multiple_players_mixed(self):
        """Only the Go Live players are included, whatever they're playing."""
        m1 = _make_member(1, in_voice=True, self_stream=True, activities=[_game("Dota 2")])
        m2 = _make_member(2, in_voice=True, self_stream=False, activities=[_game("Dota 2")])
        m3 = _make_member(3, in_voice=True, self_stream=True, activities=[_game("Deadlock")])
        m4 = _make_member(4, in_voice=True, self_stream=True, activities=[])
        guild = self._guild_with_members({1: m1, 2: m2, 3: m3, 4: m4})

        result = get_streaming_player_ids(guild, [1, 2, 3, 4])
        assert result == {1, 3, 4}

    def test_empty_player_list(self):
        """An empty player list returns an empty set."""
        guild = self._guild_with_members({})

        result = get_streaming_player_ids(guild, [])
        assert result == set()
