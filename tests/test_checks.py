"""Tests for shared pre-command checks (commands/checks.py)."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands.checks import require_dig_channel, require_gamba_channel


def _mock_interaction(
    channel_name: str,
    parent_name: str | None = None,
    *,
    channel_id: int = 100,
    parent_id: int | None = None,
):
    """Build a fake discord.Interaction with the channel/parent we care about.

    Uses SimpleNamespace + AsyncMock to avoid pulling in the discord library's
    full Interaction surface. ``require_gamba_channel`` only touches a small
    set of attributes.
    """
    if parent_name is not None:
        parent = SimpleNamespace(name=parent_name, id=parent_id if parent_id is not None else 200)
    else:
        parent = None
    channel = SimpleNamespace(name=channel_name, parent=parent, id=channel_id)
    interaction = SimpleNamespace(
        channel=channel,
        user=SimpleNamespace(id=42),
        guild=SimpleNamespace(id=99, get_channel=lambda _id: SimpleNamespace()),
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


@pytest.mark.asyncio
async def test_passes_when_channel_id_in_allowed_list():
    """A non-gamba channel passes if its id is in extra_allowed_channel_ids."""
    interaction = _mock_interaction(channel_name="the-pit", channel_id=12345)
    result = await require_gamba_channel(
        interaction, extra_allowed_channel_ids=(12345,)
    )
    assert result is True
    interaction.response.send_message.assert_not_called()


@pytest.mark.asyncio
async def test_passes_when_thread_parent_id_in_allowed_list():
    """A thread under an allowed channel passes via the parent.id check."""
    interaction = _mock_interaction(
        channel_name="random-thread",
        parent_name="the-pit",
        channel_id=999,
        parent_id=12345,
    )
    result = await require_gamba_channel(
        interaction, extra_allowed_channel_ids=(12345,)
    )
    assert result is True
    interaction.response.send_message.assert_not_called()


@pytest.mark.asyncio
async def test_extra_ids_not_matching_falls_through_to_reject():
    """If extra ids don't match, a non-gamba channel still gets rejected."""
    interaction = _mock_interaction(channel_name="general", channel_id=999)
    result = await require_gamba_channel(
        interaction, extra_allowed_channel_ids=(12345,)
    )
    assert result is False
    interaction.response.send_message.assert_awaited_once()


@pytest.mark.asyncio
async def test_require_dig_channel_passes_in_dig_channel(monkeypatch):
    """require_dig_channel reads DIG_CHANNEL_ID and lets matching channels pass."""
    import config

    monkeypatch.setattr(config, "DIG_CHANNEL_ID", 55555)
    interaction = _mock_interaction(channel_name="the-pit", channel_id=55555)
    assert await require_dig_channel(interaction) is True
    interaction.response.send_message.assert_not_called()


@pytest.mark.asyncio
async def test_require_dig_channel_rejects_unrelated_channel(monkeypatch):
    """Without a name match and a different id, require_dig_channel rejects."""
    import config

    monkeypatch.setattr(config, "DIG_CHANNEL_ID", 55555)
    interaction = _mock_interaction(channel_name="general", channel_id=12345)
    assert await require_dig_channel(interaction) is False
    interaction.response.send_message.assert_awaited_once()


@pytest.mark.asyncio
async def test_require_dig_channel_with_no_env_falls_back_to_gamba(monkeypatch):
    """If DIG_CHANNEL_ID is unset, require_dig_channel matches require_gamba_channel."""
    import config

    monkeypatch.setattr(config, "DIG_CHANNEL_ID", None)
    interaction = _mock_interaction(channel_name="gamba-dota")
    assert await require_dig_channel(interaction) is True
    interaction.response.send_message.assert_not_called()
