"""
Tests for command-layer validation in commands/betting_helpers/economy_actions.py.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.betting_helpers.economy_actions import paydebt_action


class FakeResponse:
    def __init__(self):
        self.messages = []
        self.defer_calls = 0

    async def send_message(self, content=None, ephemeral=None, embed=None):
        self.messages.append({"content": content, "ephemeral": ephemeral})

    async def defer(self, ephemeral=False):
        self.defer_calls += 1


class FakeInteraction:
    def __init__(self, user_id=1, guild_id=123):
        self.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = SimpleNamespace(id=guild_id)
        self.response = FakeResponse()
        self.followup = SimpleNamespace(send=AsyncMock())


def make_member(user_id):
    return SimpleNamespace(id=user_id, mention=f"<@{user_id}>")


@pytest.fixture
def cog():
    """Minimal cog stub: pay_debt_atomic records calls so tests can assert it was never reached."""
    return SimpleNamespace(
        player_service=SimpleNamespace(pay_debt_atomic=MagicMock()),
    )


@pytest.fixture(autouse=True)
def allow_rate_limit(monkeypatch):
    monkeypatch.setattr(
        "commands.betting_helpers.economy_actions.GLOBAL_RATE_LIMITER.check",
        lambda **kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )


@pytest.mark.asyncio
async def test_paydebt_rejects_zero_amount(cog):
    """A zero amount must be rejected ephemerally before any defer or DB work."""
    interaction = FakeInteraction(user_id=1)

    await paydebt_action(cog, interaction, make_member(2), 0)

    assert len(interaction.response.messages) == 1
    assert interaction.response.messages[0]["content"] == "Amount must be positive."
    assert interaction.response.messages[0]["ephemeral"] is True
    assert interaction.response.defer_calls == 0
    cog.player_service.pay_debt_atomic.assert_not_called()


@pytest.mark.asyncio
async def test_paydebt_rejects_negative_amount(cog):
    """A negative amount must be rejected ephemerally before any defer or DB work."""
    interaction = FakeInteraction(user_id=1)

    await paydebt_action(cog, interaction, make_member(2), -5)

    assert len(interaction.response.messages) == 1
    assert interaction.response.messages[0]["content"] == "Amount must be positive."
    assert interaction.response.messages[0]["ephemeral"] is True
    assert interaction.response.defer_calls == 0
    cog.player_service.pay_debt_atomic.assert_not_called()


@pytest.mark.asyncio
async def test_paydebt_rejects_self_target(cog):
    """Paying your own debt must be rejected ephemerally before any defer or DB work."""
    interaction = FakeInteraction(user_id=1)

    await paydebt_action(cog, interaction, make_member(1), 10)

    assert len(interaction.response.messages) == 1
    assert interaction.response.messages[0]["content"] == "You cannot pay your own debt."
    assert interaction.response.messages[0]["ephemeral"] is True
    assert interaction.response.defer_calls == 0
    cog.player_service.pay_debt_atomic.assert_not_called()
