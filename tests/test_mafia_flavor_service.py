"""Tests for MafiaFlavorService."""

from __future__ import annotations

import random

import pytest

from domain.models.mafia import (
    MafiaGame,
    MafiaPhase,
    MafiaPlayer,
    MafiaRole,
    MafiaTwist,
    MafiaWinner,
)
from services.mafia_flavor_service import MafiaFlavorService


@pytest.fixture
def flavor():
    return MafiaFlavorService(rng=random.Random(0))


@pytest.fixture
def sample_game():
    return MafiaGame(
        game_id=1,
        guild_id=42,
        game_date="2026-04-24",
        phase=MafiaPhase.NIGHT,
        started_at=1000,
        roster_size=8,
    )


@pytest.fixture
def victim():
    return MafiaPlayer(
        game_id=1,
        discord_id=999,
        guild_id=42,
        role=MafiaRole.TOWNIE,
        hero_name="Pudge",
    )


@pytest.mark.asyncio
async def test_setup_narration_returns_string(flavor, sample_game):
    text = await flavor.setup_narration(sample_game)
    assert isinstance(text, str)
    assert len(text) > 0


@pytest.mark.asyncio
async def test_setup_includes_twist_line(flavor, sample_game):
    sample_game.twist_event = MafiaTwist.BLOOD_MOON
    text = await flavor.setup_narration(sample_game)
    assert "blood moon" in text.lower()


@pytest.mark.asyncio
async def test_death_narration_includes_role_and_hero(flavor, victim):
    text = await flavor.death_narration(victim)
    assert "Pudge" in text
    assert "Townie" in text
    assert f"<@{victim.discord_id}>" in text


@pytest.mark.asyncio
async def test_plague_death_uses_plague_template(flavor, victim):
    text = await flavor.death_narration(victim, by_plague=True)
    assert "plague" in text.lower() or "fever" in text.lower()


@pytest.mark.asyncio
async def test_lynch_narration_includes_role(flavor, victim):
    victim.role = MafiaRole.MAFIA
    text = await flavor.lynch_narration(victim)
    assert "Mafia" in text


@pytest.mark.asyncio
async def test_resolution_narrations_per_winner(flavor):
    town = await flavor.resolution_narration(MafiaWinner.TOWN)
    mafia = await flavor.resolution_narration(MafiaWinner.MAFIA)
    jester = await flavor.resolution_narration(MafiaWinner.JESTER)
    none = await flavor.resolution_narration(MafiaWinner.NONE)
    assert all(isinstance(t, str) and len(t) > 0 for t in (town, mafia, jester, none))


@pytest.mark.asyncio
async def test_no_lynch_narration(flavor):
    text = await flavor.no_lynch_narration()
    assert isinstance(text, str)
    assert len(text) > 0
