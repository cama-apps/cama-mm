"""Tests for FlavorTextService.generate_betting_last_call (the 1-minute hype line)."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from services.betting_personas import BETTING_PERSONAS
from services.flavor_text_service import (
    EVENT_EXAMPLES,
    FlavorEvent,
    FlavorTextService,
    PlayerContext,
)
from tests.conftest import TEST_GUILD_ID

LAST_CALL_EXAMPLES = EVENT_EXAMPLES[FlavorEvent.BET_LAST_CALL]


def _service(ai_enabled: bool, ai_result=None):
    ai_service = MagicMock()
    ai_service.generate_flavor = AsyncMock(return_value=ai_result)
    guild_config_repo = MagicMock()
    guild_config_repo.get_ai_enabled = MagicMock(return_value=ai_enabled)
    svc = FlavorTextService(
        ai_service=ai_service,
        player_repo=MagicMock(),
        guild_config_repo=guild_config_repo,
    )
    return svc, ai_service


@pytest.mark.asyncio
async def test_disabled_returns_static_without_leader():
    svc, ai_service = _service(ai_enabled=False)
    result = await svc.generate_betting_last_call(
        TEST_GUILD_ID, {"standings": "R 100 | D 200", "seconds_left": 60}
    )
    assert result in LAST_CALL_EXAMPLES
    ai_service.generate_flavor.assert_not_called()


@pytest.mark.asyncio
async def test_disabled_returns_static_even_with_leader():
    svc, ai_service = _service(ai_enabled=False)
    result = await svc.generate_betting_last_call(
        TEST_GUILD_ID,
        {"standings": "R 100 | D 200", "seconds_left": 60, "leader_amount": 500, "leader_team": "dire"},
        leader_discord_id=999,
    )
    assert result in LAST_CALL_EXAMPLES
    ai_service.generate_flavor.assert_not_called()


@pytest.mark.asyncio
async def test_enabled_empty_pool_calls_ai_player_agnostic():
    svc, ai_service = _service(ai_enabled=True, ai_result="EMPTY POOL TAUNT")
    result = await svc.generate_betting_last_call(
        TEST_GUILD_ID, {"standings": "no bets yet", "seconds_left": 60}, leader_discord_id=None
    )
    assert result == "EMPTY POOL TAUNT"
    ai_service.generate_flavor.assert_awaited_once()
    kwargs = ai_service.generate_flavor.call_args.kwargs
    assert kwargs["event_type"] == "bet_last_call"
    assert kwargs["event_details"]["has_bettor"] is False
    assert kwargs["event_details"]["angle"] == "taunt_crowd"
    assert kwargs["persona"].key in BETTING_PERSONAS
    # Player-agnostic: no player context built for an empty pool
    assert kwargs["player_context"] == {}


@pytest.mark.asyncio
async def test_enabled_with_leader_uses_player_context(monkeypatch):
    svc, ai_service = _service(ai_enabled=True, ai_result="ROAST THE LEADER")
    stub_ctx = SimpleNamespace(
        username="BigBettor",
        balance=1234,
        bet_win_rate=0.0,  # a 0% win rate must still render, not be dropped as falsy
        degen_score=88,
        bankruptcy_count=3,
    )
    monkeypatch.setattr(PlayerContext, "from_services", lambda *a, **k: stub_ctx)

    result = await svc.generate_betting_last_call(
        TEST_GUILD_ID,
        {"standings": "R 100 | D 800", "seconds_left": 60, "leader_amount": 800, "leader_team": "dire"},
        leader_discord_id=777,
    )
    assert result == "ROAST THE LEADER"
    kwargs = ai_service.generate_flavor.call_args.kwargs
    assert kwargs["event_details"]["has_bettor"] is True
    assert kwargs["event_details"]["leader_name"] == "BigBettor"
    assert kwargs["event_details"]["angle"] in {"taunt_crowd", "roast_leader", "hype_leader"}
    assert kwargs["player_context"]["username"] == "BigBettor"
    assert kwargs["player_context"]["bet_win_rate"] == "0%"  # 0% renders, not dropped


@pytest.mark.asyncio
async def test_enabled_ai_none_falls_back_to_static():
    svc, ai_service = _service(ai_enabled=True, ai_result=None)
    result = await svc.generate_betting_last_call(
        TEST_GUILD_ID, {"standings": "x", "seconds_left": 60}, leader_discord_id=None
    )
    assert result in LAST_CALL_EXAMPLES


# --- generate_betting_warning (the 5-minute warning tier) ---


@pytest.mark.asyncio
async def test_warning_disabled_returns_static():
    svc, ai_service = _service(ai_enabled=False)
    result = await svc.generate_betting_warning(
        TEST_GUILD_ID,
        {"standings": "R 100 | D 500", "seconds_left": 300},
        underdog_side="radiant",
    )
    assert result in LAST_CALL_EXAMPLES
    ai_service.generate_flavor.assert_not_called()


@pytest.mark.asyncio
async def test_warning_routes_to_bet_warning_event_with_underdog():
    svc, ai_service = _service(ai_enabled=True, ai_result="UNDERDOG ROAST")
    result = await svc.generate_betting_warning(
        TEST_GUILD_ID,
        {"standings": "R 100 | D 500", "seconds_left": 300},
        leader_discord_id=None,
        underdog_side="radiant",
    )
    assert result == "UNDERDOG ROAST"
    kwargs = ai_service.generate_flavor.call_args.kwargs
    # Distinct event type from the last call so prompts can diverge.
    assert kwargs["event_type"] == "bet_warning"
    assert kwargs["event_details"]["underdog_side"] == "radiant"
    assert kwargs["event_details"]["angle"] in {"roast_underdog", "hype_underdog"}
    assert kwargs["persona"].key in BETTING_PERSONAS


@pytest.mark.asyncio
async def test_warning_without_underdog_uses_crowd_angle():
    svc, ai_service = _service(ai_enabled=True, ai_result="CROWD TAUNT")
    result = await svc.generate_betting_warning(
        TEST_GUILD_ID, {"standings": "R 200 | D 200", "seconds_left": 300}, leader_discord_id=None
    )
    assert result == "CROWD TAUNT"
    kwargs = ai_service.generate_flavor.call_args.kwargs
    assert kwargs["event_details"]["underdog_side"] is None
    assert kwargs["event_details"]["angle"] == "taunt_crowd"
    assert kwargs["event_details"]["has_bettor"] is False


@pytest.mark.asyncio
async def test_warning_ai_none_falls_back_to_static():
    svc, ai_service = _service(ai_enabled=True, ai_result=None)
    result = await svc.generate_betting_warning(
        TEST_GUILD_ID, {"standings": "x", "seconds_left": 300}, underdog_side="dire"
    )
    assert result in LAST_CALL_EXAMPLES
