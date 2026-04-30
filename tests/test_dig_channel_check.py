"""Tests for the require_dig_channel check."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.checks import require_dig_channel

DIG_ID = 12345
GUILD_ID = 99999


def _make_interaction(
    *,
    channel_id: int | None,
    parent_id: int | None = None,
    guild_id: int | None = GUILD_ID,
    guild_resolves_dig: bool = True,
) -> MagicMock:
    interaction = MagicMock()
    interaction.user.id = 77

    if guild_id is None:
        interaction.guild = None
    else:
        interaction.guild.id = guild_id
        interaction.guild.get_channel = MagicMock(
            return_value=MagicMock() if guild_resolves_dig else None
        )

    channel = MagicMock()
    channel.id = channel_id
    if parent_id is not None:
        parent = MagicMock()
        parent.id = parent_id
        channel.parent = parent
    else:
        channel.parent = None
    interaction.channel = channel

    interaction.response.send_message = AsyncMock()
    interaction.client.player_service.adjust_balance = MagicMock(return_value=0)
    return interaction


@pytest.mark.asyncio
class TestRequireDigChannel:
    async def test_falls_back_to_gamba_when_unset(self, monkeypatch):
        monkeypatch.setattr("config.DIG_CHANNEL_ID", None)
        called = {}

        async def fake_gamba(_interaction):
            called["yes"] = True
            return True

        monkeypatch.setattr("commands.checks.require_gamba_channel", fake_gamba)
        interaction = _make_interaction(channel_id=DIG_ID)
        assert await require_dig_channel(interaction) is True
        assert called == {"yes": True}

    async def test_falls_back_when_channel_unresolvable(self, monkeypatch):
        monkeypatch.setattr("config.DIG_CHANNEL_ID", DIG_ID)
        called = {}

        async def fake_gamba(_interaction):
            called["yes"] = True
            return False

        monkeypatch.setattr("commands.checks.require_gamba_channel", fake_gamba)
        interaction = _make_interaction(channel_id=DIG_ID, guild_resolves_dig=False)
        assert await require_dig_channel(interaction) is False
        assert called == {"yes": True}

    async def test_falls_back_in_dm(self, monkeypatch):
        monkeypatch.setattr("config.DIG_CHANNEL_ID", DIG_ID)
        called = {}

        async def fake_gamba(_interaction):
            called["yes"] = True
            return False

        monkeypatch.setattr("commands.checks.require_gamba_channel", fake_gamba)
        interaction = _make_interaction(channel_id=DIG_ID, guild_id=None)
        assert await require_dig_channel(interaction) is False
        assert called == {"yes": True}

    async def test_allowed_in_dig_channel(self, monkeypatch):
        monkeypatch.setattr("config.DIG_CHANNEL_ID", DIG_ID)
        interaction = _make_interaction(channel_id=DIG_ID)
        assert await require_dig_channel(interaction) is True
        interaction.client.player_service.adjust_balance.assert_not_called()
        interaction.response.send_message.assert_not_called()

    async def test_allowed_in_thread_under_dig(self, monkeypatch):
        monkeypatch.setattr("config.DIG_CHANNEL_ID", DIG_ID)
        interaction = _make_interaction(channel_id=555, parent_id=DIG_ID)
        assert await require_dig_channel(interaction) is True
        interaction.client.player_service.adjust_balance.assert_not_called()

    async def test_blocked_wrong_channel(self, monkeypatch):
        monkeypatch.setattr("config.DIG_CHANNEL_ID", DIG_ID)
        interaction = _make_interaction(channel_id=42)
        assert await require_dig_channel(interaction) is False
        interaction.client.player_service.adjust_balance.assert_called_once_with(
            77, GUILD_ID, -1
        )
        interaction.response.send_message.assert_awaited_once()
        args, kwargs = interaction.response.send_message.call_args
        assert f"<#{DIG_ID}>" in args[0]
        assert kwargs.get("ephemeral") is True

    async def test_blocked_thread_under_wrong_parent(self, monkeypatch):
        monkeypatch.setattr("config.DIG_CHANNEL_ID", DIG_ID)
        interaction = _make_interaction(channel_id=555, parent_id=9999)
        assert await require_dig_channel(interaction) is False
        interaction.client.player_service.adjust_balance.assert_called_once_with(
            77, GUILD_ID, -1
        )
