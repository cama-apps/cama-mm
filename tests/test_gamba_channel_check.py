"""Tests for the require_gamba_channel check."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.checks import require_gamba_channel


def _make_interaction(channel_name: str | None, *, guild_id: int | None = 12345) -> MagicMock:
    interaction = MagicMock()
    interaction.user.id = 99
    if guild_id is not None:
        interaction.guild.id = guild_id
    else:
        interaction.guild = None

    channel = MagicMock()
    if channel_name is not None:
        channel.name = channel_name
    else:
        # DM — no name attribute
        del channel.name
    interaction.channel = channel

    interaction.response.send_message = AsyncMock()
    interaction.client.player_service.adjust_balance = MagicMock(return_value=0)
    return interaction


@pytest.mark.asyncio
class TestRequireGambaChannel:
    async def test_allowed_exact_name(self):
        interaction = _make_interaction("gamba")
        assert await require_gamba_channel(interaction) is True
        interaction.response.send_message.assert_not_called()

    async def test_allowed_substring(self):
        interaction = _make_interaction("gamba-zone")
        assert await require_gamba_channel(interaction) is True

    async def test_allowed_contains_gamba(self):
        interaction = _make_interaction("the-gamba")
        assert await require_gamba_channel(interaction) is True

    async def test_allowed_case_insensitive(self):
        interaction = _make_interaction("Gamba-Zone")
        assert await require_gamba_channel(interaction) is True

    async def test_blocked_wrong_channel(self):
        interaction = _make_interaction("general")
        assert await require_gamba_channel(interaction) is False
        interaction.client.player_service.adjust_balance.assert_called_once_with(99, 12345, -1)
        interaction.response.send_message.assert_awaited_once()
        args, kwargs = interaction.response.send_message.call_args
        assert "ancient spirits" in args[0].lower()
        assert kwargs.get("ephemeral") is True

    async def test_blocked_dm_no_channel_name(self):
        interaction = _make_interaction(None, guild_id=None)
        assert await require_gamba_channel(interaction) is False
        interaction.client.player_service.adjust_balance.assert_called_once_with(99, None, -1)

    async def test_blocked_empty_channel_name(self):
        interaction = _make_interaction("")
        assert await require_gamba_channel(interaction) is False
        interaction.client.player_service.adjust_balance.assert_called_once()
