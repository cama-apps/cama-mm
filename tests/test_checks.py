"""Tests for shared pre-command checks (commands/checks.py)."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands.checks import require_gamba_channel


def _mock_interaction(channel_name: str, parent_name: str | None = None):
    """Build a fake discord.Interaction with the channel/parent we care about.

    Uses SimpleNamespace + AsyncMock to avoid pulling in the discord library's
    full Interaction surface. ``require_gamba_channel`` only touches a small
    set of attributes.
    """
    parent = SimpleNamespace(name=parent_name) if parent_name is not None else None
    channel = SimpleNamespace(name=channel_name, parent=parent)
    interaction = SimpleNamespace(
        channel=channel,
        user=SimpleNamespace(id=42),
        guild=SimpleNamespace(id=99),
        client=SimpleNamespace(
            player_service=SimpleNamespace(adjust_balance=lambda *a, **k: None)
        ),
        response=SimpleNamespace(send_message=AsyncMock()),
    )
    return interaction


@pytest.mark.asyncio
async def test_passes_in_gamba_channel():
    interaction = _mock_interaction(channel_name="gamba-dota")
    assert await require_gamba_channel(interaction) is True
    interaction.response.send_message.assert_not_called()


@pytest.mark.asyncio
async def test_passes_in_thread_under_gamba_channel():
    """A thread under #gamba-something inherits its parent's gamba-status."""
    interaction = _mock_interaction(
        channel_name="Market #5: Will Luke hit immortal?",
        parent_name="gamba",
    )
    assert await require_gamba_channel(interaction) is True
    interaction.response.send_message.assert_not_called()


@pytest.mark.asyncio
async def test_rejects_in_non_gamba_channel():
    interaction = _mock_interaction(channel_name="general")
    assert await require_gamba_channel(interaction) is False
    interaction.response.send_message.assert_awaited_once()


@pytest.mark.asyncio
async def test_rejects_in_thread_under_non_gamba_channel():
    interaction = _mock_interaction(
        channel_name="some-thread", parent_name="general",
    )
    assert await require_gamba_channel(interaction) is False
    interaction.response.send_message.assert_awaited_once()


@pytest.mark.asyncio
async def test_handles_missing_parent_gracefully():
    """Channels without a parent attribute don't crash."""
    interaction = _mock_interaction(channel_name="general", parent_name=None)
    assert await require_gamba_channel(interaction) is False
