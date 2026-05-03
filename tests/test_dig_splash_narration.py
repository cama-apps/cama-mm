"""Tests for LLM splash narration: validator, prompt builder, narrate_splash."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_flavor_service import DigFlavorService
from services.dig_flavor_validator import validate_splash_narrative
from services.dig_llm_prompts import (
    SPLASH_NARRATION_SYSTEM_PROMPT,
    SPLASH_NARRATION_TOOL,
    build_splash_narration_messages,
)
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def player_repository(repo_db_path):
    return PlayerRepository(repo_db_path)


@dataclass
class _MockToolCallResult:
    tool_name: str | None
    tool_args: dict[str, Any]
    content: str | None = None
    raw_response: Any = None


def _register(player_repo, discord_id: int, name: str | None = None) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=name or f"User{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )


# ---------------------------------------------------------------------------
# Validator
# ---------------------------------------------------------------------------


class TestValidateSplashNarrative:
    def test_returns_string_under_cap(self):
        assert validate_splash_narrative("Short and sweet.") == "Short and sweet."

    def test_clamps_to_200_chars(self):
        big = "x" * 500
        out = validate_splash_narrative(big)
        assert len(out) <= 200
        assert out.endswith("...")

    def test_blank_string_returns_empty(self):
        assert validate_splash_narrative("   ") == ""

    def test_strips_whitespace(self):
        assert validate_splash_narrative("  hi  ") == "hi"


# ---------------------------------------------------------------------------
# Prompt builder
# ---------------------------------------------------------------------------


class TestBuildSplashNarrationMessages:
    def test_messages_have_system_and_user(self):
        msgs = build_splash_narration_messages(
            digger_name="Alice",
            digger_layer="Crystal",
            event_name="Wisp's Tether",
            event_description="A drifting mote of light recognizes you.",
            splash_mode="grant",
            victims=[{"name": "Bob", "amount": 4}],
        )
        assert len(msgs) == 2
        assert msgs[0]["role"] == "system"
        assert msgs[0]["content"] == SPLASH_NARRATION_SYSTEM_PROMPT
        assert msgs[1]["role"] == "user"

    def test_user_content_includes_victim_names(self):
        msgs = build_splash_narration_messages(
            digger_name="Alice",
            digger_layer="Abyss",
            event_name="The Eye Opens",
            event_description="A seam in the rock parts.",
            splash_mode="steal",
            victims=[{"name": "Bob", "amount": 28}, {"name": "Carol", "amount": 4}],
        )
        user = msgs[1]["content"]
        assert "Alice" in user
        assert "Bob" in user
        assert "Carol" in user
        assert "steal" in user
        assert "Abyss" in user

    def test_handles_empty_victims_gracefully(self):
        msgs = build_splash_narration_messages(
            digger_name="Alice",
            digger_layer="Dirt",
            event_name="Test",
            event_description="Test",
            splash_mode="burn",
            victims=[],
        )
        assert "(no resolvable names)" in msgs[1]["content"]


# ---------------------------------------------------------------------------
# narrate_splash (DigFlavorService)
# ---------------------------------------------------------------------------


class TestNarrateSplash:
    def _make_service(
        self, dig_repo, player_repo, *, tool_result=None, side_effect=None,
    ):
        ai_service = MagicMock()
        if side_effect is not None:
            ai_service.call_with_tools = AsyncMock(side_effect=side_effect)
        elif tool_result is not None:
            ai_service.call_with_tools = AsyncMock(return_value=tool_result)
        else:
            ai_service.call_with_tools = AsyncMock(return_value=_MockToolCallResult(
                tool_name="narrate_splash",
                tool_args={"narrative": "Something gentle and luminous happens."},
            ))
        context_builder = MagicMock()
        return DigFlavorService(ai_service, dig_repo, player_repo, context_builder)

    @pytest.mark.asyncio
    async def test_returns_narrative_on_success(self, dig_repo, player_repository):
        _register(player_repository, 30001, name="Alice")
        _register(player_repository, 30002, name="Bob")
        svc = self._make_service(dig_repo, player_repository)
        out = await svc.narrate_splash(
            digger_id=30001,
            guild_id=TEST_GUILD_ID,
            event_name="Wisp's Tether",
            event_description="A drifting mote of light.",
            splash_mode="grant",
            victims=[{"discord_id": 30002, "amount": 4}],
        )
        assert out == "Something gentle and luminous happens."

    @pytest.mark.asyncio
    async def test_empty_victims_returns_empty_without_calling_llm(
        self, dig_repo, player_repository,
    ):
        svc = self._make_service(dig_repo, player_repository)
        out = await svc.narrate_splash(
            digger_id=30001,
            guild_id=TEST_GUILD_ID,
            event_name="Test",
            event_description="Test",
            splash_mode="burn",
            victims=[],
        )
        assert out == ""
        svc.ai_service.call_with_tools.assert_not_called()

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        "side_effect",
        [TimeoutError(), RuntimeError("API down"), ValueError("bad json")],
    )
    async def test_returns_empty_on_any_error(
        self, side_effect, dig_repo, player_repository,
    ):
        _register(player_repository, 30001)
        _register(player_repository, 30002)
        svc = self._make_service(
            dig_repo, player_repository, side_effect=side_effect,
        )
        out = await svc.narrate_splash(
            digger_id=30001,
            guild_id=TEST_GUILD_ID,
            event_name="Test",
            event_description="Test",
            splash_mode="steal",
            victims=[{"discord_id": 30002, "amount": 10}],
        )
        assert out == ""

    @pytest.mark.asyncio
    async def test_returns_empty_on_wrong_tool(self, dig_repo, player_repository):
        _register(player_repository, 30001)
        _register(player_repository, 30002)
        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(tool_name="other_tool", tool_args={}),
        )
        out = await svc.narrate_splash(
            digger_id=30001,
            guild_id=TEST_GUILD_ID,
            event_name="Test",
            event_description="Test",
            splash_mode="burn",
            victims=[{"discord_id": 30002, "amount": 5}],
        )
        assert out == ""

    @pytest.mark.asyncio
    async def test_clamps_oversize_narrative(self, dig_repo, player_repository):
        _register(player_repository, 30001)
        _register(player_repository, 30002)
        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(
                tool_name="narrate_splash",
                tool_args={"narrative": "y" * 600},
            ),
        )
        out = await svc.narrate_splash(
            digger_id=30001,
            guild_id=TEST_GUILD_ID,
            event_name="Test",
            event_description="Test",
            splash_mode="grant",
            victims=[{"discord_id": 30002, "amount": 10}],
        )
        assert len(out) <= 200


# ---------------------------------------------------------------------------
# Aftermath line rendering
# ---------------------------------------------------------------------------


class TestSplashAftermathLines:
    def test_renders_narrative_above_victim_lines(self):
        from commands.dig import _splash_aftermath_lines

        splash = {
            "mode": "steal",
            "victims": [{"discord_id": 99001, "amount": 10}],
            "llm_narrative": "Bob feels something missing.",
        }
        lines = _splash_aftermath_lines(splash)
        assert lines[0] == "*Bob feels something missing.*"
        assert "<@99001>" in lines[1]
        assert "-10" in lines[1]

    def test_renders_without_narrative_when_missing(self):
        from commands.dig import _splash_aftermath_lines

        splash = {
            "mode": "grant",
            "victims": [{"discord_id": 99002, "amount": 4}],
        }
        lines = _splash_aftermath_lines(splash)
        assert len(lines) == 1
        assert "<@99002>" in lines[0]
        assert "+4" in lines[0]

    def test_blank_narrative_treated_as_missing(self):
        from commands.dig import _splash_aftermath_lines

        splash = {
            "mode": "burn",
            "victims": [{"discord_id": 99003, "amount": 5}],
            "llm_narrative": "   ",
        }
        lines = _splash_aftermath_lines(splash)
        assert len(lines) == 1
        assert "*" not in lines[0]


# ---------------------------------------------------------------------------
# Tool schema sanity
# ---------------------------------------------------------------------------


class TestSplashNarrationToolSchema:
    def test_tool_has_required_shape(self):
        assert SPLASH_NARRATION_TOOL["type"] == "function"
        fn = SPLASH_NARRATION_TOOL["function"]
        assert fn["name"] == "narrate_splash"
        assert "narrative" in fn["parameters"]["properties"]
        assert "narrative" in fn["parameters"]["required"]
