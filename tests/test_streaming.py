"""Tests for utils/streaming.py — Go Live + Dota 2 detection."""

from unittest.mock import MagicMock

import discord

from utils.streaming import get_streaming_dota_player_ids


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


def _dota_game() -> discord.Game:
    return discord.Game(name="Dota 2")


def _other_game() -> discord.Game:
    return discord.Game(name="Counter-Strike 2")


def _dota_streaming() -> MagicMock:
    act = MagicMock(spec=discord.Streaming)
    act.name = "Dota 2"
    return act


class TestGetStreamingDotaPlayerIds:
    """Tests for get_streaming_dota_player_ids."""

    def _guild_with_members(self, members: dict[int, MagicMock]) -> MagicMock:
        guild = MagicMock(spec=discord.Guild)
        guild.get_member = lambda pid: members.get(pid)
        return guild

    def test_go_live_with_dota_activity(self):
        """Player who is Go Live + Dota 2 game should be included."""
        m = _make_member(1, in_voice=True, self_stream=True, activities=[_dota_game()])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == {1}

    def test_go_live_with_other_game(self):
        """Player who is Go Live but playing a different game should be excluded."""
        m = _make_member(1, in_voice=True, self_stream=True, activities=[_other_game()])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == set()

    def test_not_go_live_with_dota(self):
        """Player in voice but not Go Live should be excluded."""
        m = _make_member(1, in_voice=True, self_stream=False, activities=[_dota_game()])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == set()

    def test_not_in_voice(self):
        """Player not in any voice channel should be excluded."""
        m = _make_member(1, in_voice=False, activities=[_dota_game()])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == set()

    def test_none_voice_state(self):
        """Player with None voice state should be excluded."""
        m = _make_member(1, in_voice=False)
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == set()

    def test_member_not_found(self):
        """Player ID not found in guild should be excluded."""
        guild = self._guild_with_members({})

        result = get_streaming_dota_player_ids(guild, [999])
        assert result == set()

    def test_streaming_activity_type(self):
        """Player Go Live with a Streaming activity (e.g. Twitch) showing Dota 2."""
        m = _make_member(1, in_voice=True, self_stream=True, activities=[_dota_streaming()])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == {1}

    def test_multiple_players_mixed(self):
        """Multiple players: only those Go Live + Dota 2 are included."""
        m1 = _make_member(1, in_voice=True, self_stream=True, activities=[_dota_game()])
        m2 = _make_member(2, in_voice=True, self_stream=False, activities=[_dota_game()])
        m3 = _make_member(3, in_voice=True, self_stream=True, activities=[_other_game()])
        m4 = _make_member(4, in_voice=True, self_stream=True, activities=[_dota_game()])
        guild = self._guild_with_members({1: m1, 2: m2, 3: m3, 4: m4})

        result = get_streaming_dota_player_ids(guild, [1, 2, 3, 4])
        assert result == {1, 4}

    def test_empty_player_list(self):
        """Empty player list returns empty set."""
        guild = self._guild_with_members({})

        result = get_streaming_dota_player_ids(guild, [])
        assert result == set()

    def test_no_activities(self):
        """Player Go Live but with no activities should be excluded."""
        m = _make_member(1, in_voice=True, self_stream=True, activities=[])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == set()

    def test_case_insensitive_dota_match(self):
        """Activity name matching should be case-insensitive."""
        game = discord.Game(name="DOTA 2")
        m = _make_member(1, in_voice=True, self_stream=True, activities=[game])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == {1}

    def test_activity_with_none_name(self):
        """Activity with None name should not cause an error."""
        act = MagicMock(spec=discord.Game)
        act.name = None
        m = _make_member(1, in_voice=True, self_stream=True, activities=[act])
        guild = self._guild_with_members({1: m})

        result = get_streaming_dota_player_ids(guild, [1])
        assert result == set()
