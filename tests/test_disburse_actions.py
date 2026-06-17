"""Tests for disbursement command action helpers."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands.betting_helpers import disburse_actions as actions
from commands.betting_helpers.disburse_embeds import (
    build_disburse_embed,
    build_disburse_votes_embed,
)
from tests.conftest import TEST_GUILD_ID


class _Proposal:
    def __init__(self, votes: dict[str, int] | None = None):
        self.guild_id = TEST_GUILD_ID
        self.proposal_id = 123
        self.message_id = None
        self.channel_id = None
        self.fund_amount = 500
        self.quorum_required = 10
        self.status = "active"
        self.votes = votes or {
            "even": 0,
            "proportional": 0,
            "neediest": 0,
            "stimulus": 0,
            "lottery": 0,
            "social_security": 0,
            "richest": 0,
            "cancel": 0,
        }

    @property
    def total_votes(self) -> int:
        return sum(self.votes.values())

    @property
    def quorum_progress(self) -> float:
        return self.total_votes / self.quorum_required

    @property
    def quorum_reached(self) -> bool:
        return self.total_votes >= self.quorum_required


class _FakeDisburseService:
    METHODS = (
        "even",
        "proportional",
        "neediest",
        "stimulus",
        "lottery",
        "social_security",
        "richest",
        "cancel",
    )
    METHOD_LABELS = {
        "even": "Even Split",
        "proportional": "Proportional",
        "neediest": "Neediest First",
        "stimulus": "Stimulus",
        "lottery": "Lottery",
        "social_security": "Social Security",
        "richest": "Richest",
        "cancel": "Cancel",
    }

    def __init__(
        self,
        *,
        proposal: _Proposal | None = None,
        individual_votes: list[dict] | None = None,
    ):
        self.proposal = proposal
        self.individual_votes = individual_votes or []
        self.reset_called = False
        self.force_execute_called = False

    def get_proposal(self, guild_id: int | None):
        return self.proposal

    def get_individual_votes(self, guild_id: int | None) -> list[dict]:
        return self.individual_votes

    def reset_proposal(self, guild_id: int | None) -> bool:
        self.reset_called = True
        return True

    def force_execute(self, guild_id: int | None) -> dict:
        self.force_execute_called = True
        return {
            "success": True,
            "method": "even",
            "method_label": "Even Split",
            "total_disbursed": 0,
            "recipient_count": 0,
            "distributions": [],
            "message": "No funds were distributed.",
        }


class _FakeResponse:
    def __init__(self):
        self.messages: list[dict] = []

    async def send_message(self, content=None, embed=None, view=None, ephemeral=None):
        self.messages.append(
            {
                "content": content,
                "embed": embed,
                "view": view,
                "ephemeral": ephemeral,
            }
        )


class _FakeFollowup:
    def __init__(self):
        self.messages: list[dict] = []

    async def send(self, content=None, embed=None, ephemeral=None):
        self.messages.append(
            {
                "content": content,
                "embed": embed,
                "ephemeral": ephemeral,
            }
        )


class _FakeInteraction:
    def __init__(self, user_id: int = 999):
        self.user = SimpleNamespace(id=user_id, display_name="Tax Man")
        self.guild = SimpleNamespace(id=TEST_GUILD_ID)
        self.response = _FakeResponse()
        self.followup = _FakeFollowup()


def _vote_rows(count: int) -> list[dict]:
    return [
        {
            "discord_id": 10_000 + idx,
            "vote_method": "even" if idx % 2 == 0 else "proportional",
            "voted_at": 1_700_000_000 + idx,
        }
        for idx in range(count)
    ]


def _individual_vote_field(embed) -> str:
    return next(field.value for field in embed.fields if "Individual Votes" in field.name)


def test_disburse_embed_uses_jopacoin_reserve_language():
    embed = build_disburse_embed(_Proposal())

    text = "\n".join(
        [embed.title or "", embed.description or ""]
        + [f"{field.name}\n{field.value}" for field in embed.fields]
    )
    assert "Jopacoin Reserve" in text
    assert "server operations budget" in text
    assert "Keep budget in reserve" in text
    assert "Nonprofit" not in text


def test_disburse_votes_embed_pages_show_every_voter():
    individual_votes = _vote_rows(31)
    proposal = _Proposal(votes={"even": 16, "proportional": 15})
    service = _FakeDisburseService()

    pages = [
        build_disburse_votes_embed(
            proposal,
            service,
            individual_votes,
            page=page,
            page_size=15,
        )
        for page in range(3)
    ]

    all_vote_text = "\n".join(_individual_vote_field(page) for page in pages)
    for vote in individual_votes:
        assert f"<@{vote['discord_id']}>" in all_vote_text
    assert "..." not in all_vote_text
    assert pages[0].footer.text.endswith("Page 1/3")
    assert pages[2].footer.text.endswith("Page 3/3")


@pytest.mark.asyncio
async def test_disburse_votes_tax_man_gets_paginated_view(monkeypatch):
    monkeypatch.setattr(actions, "has_tax_man_permission", lambda _: True)
    individual_votes = _vote_rows(16)
    proposal = _Proposal(votes={"even": 8, "proportional": 8})
    service = _FakeDisburseService(
        proposal=proposal,
        individual_votes=individual_votes,
    )
    cog = SimpleNamespace(disburse_service=service)
    interaction = _FakeInteraction()

    await actions.disburse_votes(cog, interaction, TEST_GUILD_ID)

    message = interaction.response.messages[0]
    assert message["ephemeral"] is True
    assert message["view"].total_pages == 2
    assert "<@10000>" in _individual_vote_field(message["embed"])
    assert "<@10015>" not in _individual_vote_field(message["embed"])


@pytest.mark.asyncio
async def test_disburse_votes_blocks_non_tax_man(monkeypatch):
    monkeypatch.setattr(actions, "has_tax_man_permission", lambda _: False)
    service = _FakeDisburseService(proposal=_Proposal())
    cog = SimpleNamespace(disburse_service=service)
    interaction = _FakeInteraction()

    await actions.disburse_votes(cog, interaction, TEST_GUILD_ID)

    assert "Only Tax Men" in interaction.response.messages[0]["content"]


@pytest.mark.asyncio
async def test_disburse_reset_tax_man_can_reset(monkeypatch):
    monkeypatch.setattr(actions, "has_tax_man_permission", lambda _: True)
    service = _FakeDisburseService(proposal=_Proposal())
    cog = SimpleNamespace(disburse_service=service)
    interaction = _FakeInteraction()

    await actions.disburse_reset(cog, interaction, TEST_GUILD_ID)

    assert service.reset_called is True
    assert "reset" in interaction.response.messages[0]["content"].lower()
    assert interaction.response.messages[0]["ephemeral"] is False


@pytest.mark.asyncio
async def test_disburse_execute_tax_man_can_force_execute(monkeypatch):
    monkeypatch.setattr(actions, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(actions, "safe_defer", AsyncMock(return_value=True))
    service = _FakeDisburseService(
        proposal=_Proposal(votes={"even": 1}),
        individual_votes=_vote_rows(1),
    )
    cog = SimpleNamespace(
        disburse_service=service,
        bot=SimpleNamespace(get_channel=lambda _channel_id: None),
    )
    interaction = _FakeInteraction()

    await actions.disburse_execute(cog, interaction, TEST_GUILD_ID)

    assert service.force_execute_called is True
    assert interaction.followup.messages[0]["embed"].title == (
        "💝 Disbursement Complete (Tax Man)"
    )
