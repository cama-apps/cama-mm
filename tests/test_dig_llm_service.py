"""Tests for the LLM dig game engine service."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_llm_prompts import (
    DIG_DICE_TOOL,
    DIG_ENGINE_SYSTEM_PROMPT,
    DIG_ENGINE_TOOL,
    DIG_OUTCOME_TOOL,
    DIG_SYSTEM_PROMPT,
    PLAY_STYLE_DESCRIPTIONS,
    build_dice_results_context,
    build_dig_outcome_context,
    build_engine_messages,
    build_messages,
    build_multiplayer_context,
    build_personality_context,
    build_player_state_context,
    build_preconditions_context,
)
from services.dig_llm_service import (
    DigLLMService,
    DigLLMValidator,
    classify_play_style,
)
from services.dig_service import DigService
from tests.conftest import TEST_GUILD_ID

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def player_repository(repo_db_path):
    return PlayerRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository):
    return DigService(dig_repo=dig_repo, player_repo=player_repository)


def _register_player(player_repository, discord_id=10001, guild_id=TEST_GUILD_ID, balance=100):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:
        player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


# ---------------------------------------------------------------------------
# classify_play_style
# ---------------------------------------------------------------------------

class TestClassifyPlayStyle:
    def test_unknown_when_too_few_choices(self):
        assert classify_play_style({"safe": 2, "risky": 1}) == "unknown"

    def test_cautious_grinder(self):
        assert classify_play_style({"safe": 80, "risky": 10, "desperate": 1}) == "cautious_grinder"

    def test_reckless_degen(self):
        assert classify_play_style({"safe": 10, "risky": 20, "desperate": 10}) == "reckless_degen"

    def test_calculated_risk_taker(self):
        assert classify_play_style({"safe": 10, "risky": 50, "desperate": 5}) == "calculated_risk_taker"

    def test_social_butterfly(self):
        assert classify_play_style({"safe": 5, "risky": 5, "help": 20}) == "social_butterfly"

    def test_balanced_explorer(self):
        assert classify_play_style({"safe": 10, "risky": 8, "desperate": 2}) == "balanced_explorer"

    def test_empty_histogram(self):
        assert classify_play_style({}) == "unknown"


# ---------------------------------------------------------------------------
# DigLLMValidator
# ---------------------------------------------------------------------------

class TestDigLLMValidator:
    def setup_method(self):
        self.validator = DigLLMValidator()

    def test_valid_narrative(self):
        result = self.validator.validate_narrative({
            "narrative": "You swing your pickaxe.",
            "tone": "dramatic",
        })
        assert result["narrative"] == "You swing your pickaxe."
        assert result["tone"] == "dramatic"
        assert result["event_flavor"] == ""
        assert result["cave_in_flavor"] == ""
        assert result["callback_reference"] == ""

    def test_narrative_truncation(self):
        long_text = "A" * 600
        result = self.validator.validate_narrative({"narrative": long_text, "tone": "humorous"})
        assert len(result["narrative"]) == 500
        assert result["narrative"].endswith("...")

    def test_invalid_tone_defaults_to_dramatic(self):
        result = self.validator.validate_narrative({"narrative": "Hello", "tone": "invalid"})
        assert result["tone"] == "dramatic"

    def test_all_tones_valid(self):
        for tone in ("dramatic", "humorous", "ominous", "triumphant", "melancholy", "absurd"):
            result = self.validator.validate_narrative({"narrative": "test", "tone": tone})
            assert result["tone"] == tone

    def test_event_flavor_truncation(self):
        result = self.validator.validate_narrative({
            "narrative": "Hi",
            "tone": "dramatic",
            "event_flavor": "B" * 400,
        })
        assert len(result["event_flavor"]) == 300
        assert result["event_flavor"].endswith("...")

    def test_cave_in_flavor_truncation(self):
        result = self.validator.validate_narrative({
            "narrative": "Hi",
            "tone": "dramatic",
            "cave_in_flavor": "C" * 400,
        })
        assert len(result["cave_in_flavor"]) == 300

    def test_callback_truncation(self):
        result = self.validator.validate_narrative({
            "narrative": "Hi",
            "tone": "dramatic",
            "callback_reference": "D" * 250,
        })
        assert len(result["callback_reference"]) == 200

    def test_missing_fields_get_defaults(self):
        result = self.validator.validate_narrative({})
        assert result["narrative"] == ""
        assert result["tone"] == "dramatic"
        assert result["event_flavor"] == ""
        assert result["cave_in_flavor"] == ""
        assert result["callback_reference"] == ""


# ---------------------------------------------------------------------------
# Context builders
# ---------------------------------------------------------------------------

class TestBuildPlayerStateContext:
    def test_basic_tunnel(self):
        tunnel = {
            "tunnel_name": "Test Tunnel",
            "depth": 42,
            "pickaxe_tier": 2,
            "prestige_level": 1,
            "luminosity": 80,
            "streak_days": 5,
            "total_digs": 20,
        }
        ctx = build_player_state_context(tunnel, 50)
        assert "Test Tunnel" in ctx
        assert "42" in ctx
        assert "Stone" in ctx
        assert "Iron" in ctx
        assert "50 JC" in ctx

    def test_miner_profile_context(self):
        tunnel = {
            "tunnel_name": "Burrow",
            "depth": 12,
            "miner_about": "Former cartographer who fears ceilings.",
            "stat_strength": 2,
            "stat_smarts": 3,
            "stat_stamina": 1,
            "stat_points": 7,
        }
        ctx = build_player_state_context(tunnel, 0)
        assert "Former cartographer" in ctx
        assert "Strength 2" in ctx
        assert "Smarts 3" in ctx
        assert "Stamina 1" in ctx

    def test_empty_tunnel(self):
        ctx = build_player_state_context({}, 0)
        assert "Dirt" in ctx
        assert "Wooden" in ctx

    def test_relics_json(self):
        tunnel = {
            "depth": 10,
            "equipped_relics": json.dumps([{"name": "Mole Claws"}, {"name": "Crystal Compass"}]),
        }
        ctx = build_player_state_context(tunnel, 0)
        assert "Mole Claws" in ctx
        assert "Crystal Compass" in ctx

    def test_mutations_json(self):
        tunnel = {
            "depth": 200,
            "mutations": json.dumps([{"name": "Dark Sight"}, {"name": "Thick Skin"}]),
        }
        ctx = build_player_state_context(tunnel, 0)
        assert "Dark Sight" in ctx
        assert "Thick Skin" in ctx

    def test_buffs_json(self):
        tunnel = {
            "depth": 50,
            "temp_buffs": json.dumps([{"name": "Second Wind", "digs_remaining": 2}]),
        }
        ctx = build_player_state_context(tunnel, 0)
        assert "Second Wind" in ctx
        assert "2 digs left" in ctx


class TestBuildPersonalityContext:
    def test_none_personality(self):
        assert build_personality_context(None) == "New player, no history yet."

    def test_empty_personality(self):
        assert build_personality_context({}) == "New player, no history yet."

    def test_with_play_style(self):
        personality = {"play_style": "cautious_grinder"}
        ctx = build_personality_context(personality)
        assert "cautious_grinder" in ctx
        assert "plays it safe" in ctx

    def test_with_histogram(self):
        personality = {
            "play_style": "balanced_explorer",
            "choice_histogram": {"safe": 10, "risky": 8},
        }
        ctx = build_personality_context(personality)
        assert "safe: 10" in ctx
        assert "risky: 8" in ctx

    def test_with_notable_moments(self):
        personality = {
            "play_style": "unknown",
            "notable_moments": ["Slew their first boss", "Discovered a rare artifact"],
        }
        ctx = build_personality_context(personality)
        assert "first boss" in ctx
        assert "artifact" in ctx


class TestBuildDigOutcomeContext:
    def test_basic_outcome(self):
        result = {"advance": 3, "jc_earned": 5}
        ctx = build_dig_outcome_context(result)
        assert "+3 blocks" in ctx
        assert "JC earned: 5" in ctx

    def test_cave_in_outcome(self):
        result = {
            "advance": 0,
            "jc_earned": 0,
            "cave_in": True,
            "cave_in_detail": {"block_loss": 5, "message": "Rocks tumbled."},
        }
        ctx = build_dig_outcome_context(result)
        assert "CAVE-IN" in ctx
        assert "5 blocks" in ctx

    def test_event_outcome(self):
        result = {
            "advance": 2,
            "jc_earned": 3,
            "event": {"id": "crystal_golem", "name": "Crystal Golem", "description": "A golem appears!"},
        }
        ctx = build_dig_outcome_context(result)
        assert "Crystal Golem" in ctx

    def test_empty_result(self):
        ctx = build_dig_outcome_context({})
        assert "+0 blocks" in ctx


class TestBuildMultiplayerContext:
    def test_empty(self):
        assert build_multiplayer_context([]) == ""
        assert build_multiplayer_context(None) == ""

    def test_with_actions(self):
        actions = [
            {"action_type": "sabotage", "actor_id": 111, "target_id": 222, "created_at": 1000},
            {"action_type": "help", "actor_id": 333, "target_id": 222, "created_at": 1001},
        ]
        ctx = build_multiplayer_context(actions)
        assert "SABOTAGE" in ctx
        assert "HELP" in ctx


class TestBuildMessages:
    def test_structure(self):
        msgs = build_messages("system", "state", "personality", "outcome", "")
        assert len(msgs) == 2
        assert msgs[0]["role"] == "system"
        assert msgs[0]["content"] == "system"
        assert msgs[1]["role"] == "user"
        assert "PLAYER STATE" in msgs[1]["content"]
        assert "PERSONALITY" in msgs[1]["content"]
        assert "DIG OUTCOME" in msgs[1]["content"]
        assert "MULTIPLAYER" not in msgs[1]["content"]

    def test_with_multiplayer(self):
        msgs = build_messages("system", "state", "personality", "outcome", "sabotage stuff")
        assert "MULTIPLAYER" in msgs[1]["content"]


# ---------------------------------------------------------------------------
# Prompts constants
# ---------------------------------------------------------------------------

class TestPromptConstants:
    def test_tool_definition_structure(self):
        assert DIG_OUTCOME_TOOL["type"] == "function"
        func = DIG_OUTCOME_TOOL["function"]
        assert func["name"] == "narrate_dig_outcome"
        props = func["parameters"]["properties"]
        assert "narrative" in props
        assert "tone" in props
        assert "event_flavor" in props
        assert "cave_in_flavor" in props
        assert "callback_reference" in props
        assert set(func["parameters"]["required"]) == {"narrative", "tone"}

    def test_system_prompt_not_empty(self):
        assert len(DIG_SYSTEM_PROMPT) > 100

    def test_play_style_descriptions_complete(self):
        expected = {"cautious_grinder", "reckless_degen", "calculated_risk_taker",
                    "balanced_explorer", "social_butterfly", "unknown"}
        assert set(PLAY_STYLE_DESCRIPTIONS.keys()) == expected


# ---------------------------------------------------------------------------
# DigLLMService — enhance
# ---------------------------------------------------------------------------

@dataclass
class _MockToolCallResult:
    tool_name: str | None
    tool_args: dict[str, Any]
    content: str | None = None
    raw_response: Any = None


class TestDigLLMServiceEnhance:
    """Integration test with mocked AI service."""

    def _make_service(self, dig_repo, player_repo, tool_result=None, side_effect=None):
        ai_service = MagicMock()
        if side_effect:
            ai_service.call_with_tools = AsyncMock(side_effect=side_effect)
        elif tool_result:
            ai_service.call_with_tools = AsyncMock(return_value=tool_result)
        else:
            ai_service.call_with_tools = AsyncMock(return_value=_MockToolCallResult(
                tool_name="narrate_dig_outcome",
                tool_args={
                    "narrative": "The earth shudders as your pickaxe bites into stone.",
                    "tone": "dramatic",
                    "event_flavor": "",
                    "cave_in_flavor": "",
                    "callback_reference": "",
                },
            ))
        return DigLLMService(ai_service, dig_repo, player_repo)

    @pytest.mark.asyncio
    async def test_enhance_adds_narrative(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        result = {"advance": 3, "jc_earned": 5, "depth": 10}
        enhanced = await svc.enhance(result, uid, TEST_GUILD_ID)

        assert "llm_narrative" in enhanced
        assert enhanced["llm_narrative"] == "The earth shudders as your pickaxe bites into stone."
        assert enhanced["llm_tone"] == "dramatic"

    @pytest.mark.asyncio
    async def test_enhance_fallback_on_timeout(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo, player_repository,
            side_effect=TimeoutError(),
        )
        result = {"advance": 3, "jc_earned": 5, "depth": 10}
        enhanced = await svc.enhance(result, uid, TEST_GUILD_ID)

        # Original result returned unchanged
        assert "llm_narrative" not in enhanced
        assert enhanced["advance"] == 3

    @pytest.mark.asyncio
    async def test_enhance_fallback_on_api_error(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo, player_repository,
            side_effect=RuntimeError("API down"),
        )
        result = {"advance": 2, "jc_earned": 1}
        enhanced = await svc.enhance(result, uid, TEST_GUILD_ID)

        assert "llm_narrative" not in enhanced
        assert enhanced["advance"] == 2

    @pytest.mark.asyncio
    async def test_enhance_wrong_tool_name(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo, player_repository,
            tool_result=_MockToolCallResult(tool_name="wrong_tool", tool_args={}),
        )
        result = {"advance": 1, "jc_earned": 2}
        enhanced = await svc.enhance(result, uid, TEST_GUILD_ID)

        assert "llm_narrative" not in enhanced

    @pytest.mark.asyncio
    async def test_enhance_no_tunnel(self, dig_repo, player_repository):
        """Enhance should still work even if no tunnel exists (graceful)."""
        uid = _register_player(player_repository)
        svc = self._make_service(dig_repo, player_repository)
        result = {"advance": 1, "jc_earned": 1}
        enhanced = await svc.enhance(result, uid, TEST_GUILD_ID)
        # Should either succeed with narrative or fail gracefully
        assert "advance" in enhanced


# ---------------------------------------------------------------------------
# DigLLMService — update_personality
# ---------------------------------------------------------------------------

class TestDigLLMServicePersonality:
    def test_update_personality_creates_new(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        ai_service = MagicMock()
        svc = DigLLMService(ai_service, dig_repo, player_repository)
        svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="safe")

        personality = dig_repo.get_personality(uid, TEST_GUILD_ID)
        assert personality is not None
        histogram = personality.get("choice_histogram", {})
        assert histogram.get("safe") == 1

    def test_update_personality_increments(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        ai_service = MagicMock()
        svc = DigLLMService(ai_service, dig_repo, player_repository)

        for _ in range(5):
            svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="safe")
        svc.update_personality(uid, TEST_GUILD_ID, "event", choice="risky")

        personality = dig_repo.get_personality(uid, TEST_GUILD_ID)
        histogram = personality["choice_histogram"]
        assert histogram["safe"] == 5
        assert histogram["risky"] == 1

    def test_update_personality_notable_moment(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        ai_service = MagicMock()
        svc = DigLLMService(ai_service, dig_repo, player_repository)
        svc.update_personality(
            uid, TEST_GUILD_ID, "boss_fight",
            details={"first_boss_kill": True},
        )

        personality = dig_repo.get_personality(uid, TEST_GUILD_ID)
        moments = personality.get("notable_moments", [])
        assert len(moments) == 1
        assert "first boss" in moments[0].lower()

    def test_update_personality_classifies_style(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        ai_service = MagicMock()
        svc = DigLLMService(ai_service, dig_repo, player_repository)

        # 8 safe choices should classify as cautious after threshold
        for _ in range(8):
            svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="safe")

        personality = dig_repo.get_personality(uid, TEST_GUILD_ID)
        assert personality["play_style"] == "cautious_grinder"

    def test_invalid_choice_ignored(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        ai_service = MagicMock()
        svc = DigLLMService(ai_service, dig_repo, player_repository)
        svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="invalid_choice")

        personality = dig_repo.get_personality(uid, TEST_GUILD_ID)
        histogram = personality.get("choice_histogram", {})
        assert "invalid_choice" not in histogram


# ---------------------------------------------------------------------------
# Repository — engine mode + personality
# ---------------------------------------------------------------------------

class TestDigRepoEngineMode:
    def test_default_mode_is_legacy(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")
        assert dig_repo.get_engine_mode(uid, TEST_GUILD_ID) == "legacy"

    def test_set_mode_to_llm(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")
        dig_repo.set_engine_mode(uid, TEST_GUILD_ID, "llm")
        assert dig_repo.get_engine_mode(uid, TEST_GUILD_ID) == "llm"

    def test_set_mode_back_to_legacy(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")
        dig_repo.set_engine_mode(uid, TEST_GUILD_ID, "llm")
        dig_repo.set_engine_mode(uid, TEST_GUILD_ID, "legacy")
        assert dig_repo.get_engine_mode(uid, TEST_GUILD_ID) == "legacy"

    def test_no_tunnel_returns_legacy(self, dig_repo):
        assert dig_repo.get_engine_mode(99999, TEST_GUILD_ID) == "legacy"


class TestDigRepoPersonality:
    def test_get_personality_none(self, dig_repo):
        assert dig_repo.get_personality(99999, TEST_GUILD_ID) is None

    def test_upsert_and_get(self, dig_repo):
        data = {
            "play_style": "cautious_grinder",
            "choice_histogram": {"safe": 10, "risky": 3},
            "notable_moments": ["Slew first boss"],
        }
        dig_repo.upsert_personality(10001, TEST_GUILD_ID, data)
        result = dig_repo.get_personality(10001, TEST_GUILD_ID)
        assert result is not None
        assert result["play_style"] == "cautious_grinder"
        assert result["choice_histogram"]["safe"] == 10
        assert len(result["notable_moments"]) == 1

    def test_upsert_updates_existing(self, dig_repo):
        data1 = {"play_style": "unknown", "choice_histogram": {"safe": 1}, "notable_moments": []}
        dig_repo.upsert_personality(10001, TEST_GUILD_ID, data1)

        data2 = {"play_style": "reckless_degen", "choice_histogram": {"safe": 1, "desperate": 5}, "notable_moments": ["Cave-in streak"]}
        dig_repo.upsert_personality(10001, TEST_GUILD_ID, data2)

        result = dig_repo.get_personality(10001, TEST_GUILD_ID)
        assert result["play_style"] == "reckless_degen"
        assert result["choice_histogram"]["desperate"] == 5


class TestDigRepoSocialActions:
    def test_empty_social_actions(self, dig_repo):
        actions = dig_repo.get_recent_social_actions(10001, TEST_GUILD_ID)
        assert actions == []

    def test_returns_social_actions(self, dig_repo):
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID,
            actor_id=10001,
            target_id=10002,
            action_type="sabotage",
            depth_before=50,
            depth_after=45,
            jc_delta=-5,
        )
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID,
            actor_id=10003,
            target_id=10001,
            action_type="help",
            depth_before=45,
            depth_after=48,
        )
        # Non-social action should NOT be returned
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID,
            actor_id=10001,
            action_type="dig",
            depth_before=48,
            depth_after=50,
        )

        actions = dig_repo.get_recent_social_actions(10001, TEST_GUILD_ID)
        assert len(actions) == 2
        types = {a["action_type"] for a in actions}
        assert types == {"sabotage", "help"}


# ---------------------------------------------------------------------------
# Engine tool / prompt constants
# ---------------------------------------------------------------------------

class TestEngineToolDefinition:
    def test_tool_structure(self):
        assert DIG_ENGINE_TOOL["type"] == "function"
        func = DIG_ENGINE_TOOL["function"]
        assert func["name"] == "resolve_dig"
        props = func["parameters"]["properties"]
        assert "advance" in props
        assert "jc_earned" in props
        assert "cave_in" in props
        assert "event_id" in props
        # narrative/tone are NOT in the engine tool — narration is separate
        assert "narrative" not in props
        assert "tone" not in props
        required = set(func["parameters"]["required"])
        assert {"advance", "jc_earned", "cave_in"} <= required

    def test_dice_tool_structure(self):
        assert DIG_DICE_TOOL["type"] == "function"
        func = DIG_DICE_TOOL["function"]
        assert func["name"] == "roll_dice"
        props = func["parameters"]["properties"]
        assert "rolls" in props

    def test_engine_system_prompt_not_empty(self):
        assert len(DIG_ENGINE_SYSTEM_PROMPT) > 100
        assert "Dungeon Master" in DIG_ENGINE_SYSTEM_PROMPT

    def test_engine_messages_structure(self):
        msgs = build_engine_messages("sys", "state", "pers", "precond", "")
        assert len(msgs) == 2
        assert msgs[0]["role"] == "system"
        assert "DIG PRECONDITIONS" in msgs[1]["content"]
        assert "MULTIPLAYER" not in msgs[1]["content"]


# ---------------------------------------------------------------------------
# build_preconditions_context
# ---------------------------------------------------------------------------

class TestBuildPreconditionsContext:
    def test_basic(self):
        p = {
            "depth_before": 42,
            "layer_name": "Stone",
            "advance_min": 2,
            "advance_max": 4,
            "jc_min": 1,
            "jc_max": 5,
            "cave_in_chance": 0.15,
            "event_chance": 0.20,
            "available_events": [{"id": "crystal_golem", "name": "Crystal Golem", "rarity": "common"}],
            "luminosity": 80,
            "miner_stats": {"strength": 2, "smarts": 3, "stamina": 1},
            "stat_effects": {"advance_min_bonus": 0, "advance_max_bonus": 1, "cave_in_reduction": 0.06, "cooldown_multiplier": 0.96},
        }
        ctx = build_preconditions_context(p)
        assert "42" in ctx
        assert "Stone" in ctx
        assert "2-4" in ctx
        assert "15%" in ctx
        assert "crystal_golem" in ctx
        assert "Strength 2" in ctx
        assert "-6% cave-in" in ctx

    def test_items_shown(self):
        p = {
            "depth_before": 10,
            "layer_name": "Dirt",
            "advance_min": 1, "advance_max": 3,
            "jc_min": 0, "jc_max": 1,
            "cave_in_chance": 0.05,
            "event_chance": 0.16,
            "available_events": [],
            "luminosity": 100,
            "has_dynamite": True,
            "has_grappling_hook": True,
        }
        ctx = build_preconditions_context(p)
        assert "Dynamite" in ctx
        assert "Grappling Hook" in ctx

    def test_hard_hat_prevention_shown(self):
        p = {
            "depth_before": 74,
            "layer_name": "Crystal",
            "advance_min": 1,
            "advance_max": 2,
            "jc_min": 1,
            "jc_max": 3,
            "cave_in_chance": 0.18,
            "event_chance": 0.20,
            "available_events": [],
            "luminosity": 80,
            "has_hard_hat": True,
            "hard_hat_prevents": True,
            "hard_hat_charges": 2,
        }
        ctx = build_preconditions_context(p)
        assert "Cave-in chance: 0%" in ctx
        assert "Hard Hat" in ctx
        assert "cannot happen this dig" in ctx


class TestBuildDiceResultsContext:
    def test_formats_dice_results(self):
        ctx = build_dice_results_context([
            {"label": "cave_in", "sides": 100, "rolls": [42], "modifier": 0, "total": 42},
            {"label": "loss", "sides": 6, "rolls": [4], "modifier": 2, "total": 6},
        ])
        assert "cave_in" in ctx
        assert "d100" in ctx
        assert "loss" in ctx
        assert "+ 2 = 6" in ctx


# ---------------------------------------------------------------------------
# DigLLMValidator.validate_engine_outcome
# ---------------------------------------------------------------------------

class TestValidateEngineOutcome:
    def setup_method(self):
        self.validator = DigLLMValidator()

    def _preconditions(self, **overrides):
        base = {
            "advance_min": 1, "advance_max": 3,
            "jc_min": 0, "jc_max": 5,
            "hard_hat_prevents": False,
            "available_events": [
                {"id": "crystal_golem", "name": "Crystal Golem", "rarity": "common"},
            ],
        }
        base.update(overrides)
        return base

    def test_clamps_advance(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 10, "jc_earned": 3, "cave_in": False},
            self._preconditions(),
        )
        assert result["advance"] == 3

    def test_clamps_advance_min(self):
        result = self.validator.validate_engine_outcome(
            {"advance": -5, "jc_earned": 2, "cave_in": False},
            self._preconditions(),
        )
        assert result["advance"] == 1

    def test_clamps_jc(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 2, "jc_earned": 100, "cave_in": False},
            self._preconditions(),
        )
        assert result["jc_earned"] == 5

    def test_cave_in_forces_zero_advance(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 3, "jc_earned": 5, "cave_in": True,
             "cave_in_block_loss": 5, "cave_in_type": "stun"},
            self._preconditions(),
        )
        assert result["cave_in"] is True
        assert result["advance"] == 0
        assert result["jc_earned"] == 0

    def test_cave_in_block_loss_clamped(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 0, "jc_earned": 0, "cave_in": True,
             "cave_in_block_loss": 15, "cave_in_type": "injury"},
            self._preconditions(),
        )
        assert result["cave_in_block_loss"] == 8

    def test_hard_hat_prevents_cave_in(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 2, "jc_earned": 3, "cave_in": True,
             "cave_in_block_loss": 5, "cave_in_type": "stun"},
            self._preconditions(hard_hat_prevents=True),
        )
        assert result["cave_in"] is False
        assert result["cave_in_block_loss"] == 0
        # narrative/tone no longer in engine outcome
        assert "narrative" not in result

    def test_invalid_event_id_cleared(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 2, "jc_earned": 3, "cave_in": False,
             "event_id": "nonexistent_event"},
            self._preconditions(),
        )
        assert result["event_id"] == ""

    def test_valid_event_id_preserved(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 2, "jc_earned": 3, "cave_in": False,
             "event_id": "crystal_golem"},
            self._preconditions(),
        )
        assert result["event_id"] == "crystal_golem"

    def test_no_narrative_in_engine_outcome(self):
        """Engine outcome should not contain narrative fields."""
        result = self.validator.validate_engine_outcome(
            {"advance": 2, "jc_earned": 3, "cave_in": False},
            self._preconditions(),
        )
        assert "narrative" not in result
        assert "tone" not in result

    def test_validate_dice_rolls_clamps(self):
        result = self.validator.validate_dice_rolls(
            {"rolls": [{"label": "cave in!", "sides": 500, "count": 99, "modifier": 500}]},
            self._preconditions(),
        )
        assert result == [{
            "label": "cavein",
            "sides": 100,
            "count": 3,
            "modifier": 100,
        }]

    def test_validate_dice_rolls_defaults(self):
        result = self.validator.validate_dice_rolls({}, self._preconditions())
        labels = {r["label"] for r in result}
        assert {"cave_in", "event", "advance", "jc"} <= labels


# ---------------------------------------------------------------------------
# dig_with_preconditions
# ---------------------------------------------------------------------------

class TestDigWithPreconditions:
    def test_returns_preconditions_for_normal_dig(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        dig_service.dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")
        # First dig via normal dig() to move past first-dig state
        dig_service.dig(uid, TEST_GUILD_ID)
        # Reset cooldown so we can dig again
        dig_service.reset_dig_cooldown(uid, TEST_GUILD_ID)

        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal is None
        assert preconditions is not None
        assert "advance_min" in preconditions
        assert "advance_max" in preconditions
        assert "jc_min" in preconditions
        assert "jc_max" in preconditions
        assert "cave_in_chance" in preconditions
        assert "available_events" in preconditions
        assert preconditions["discord_id"] == uid
        assert preconditions["depth_before"] >= 0

    def test_returns_terminal_for_unregistered(self, dig_service):
        terminal, preconditions = dig_service.dig_with_preconditions(99999, TEST_GUILD_ID)
        assert terminal is not None
        assert terminal["success"] is False
        assert preconditions is None

    def test_returns_terminal_for_first_dig(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal is not None
        assert terminal.get("is_first_dig") is True
        assert preconditions is None

    def test_returns_terminal_for_cooldown(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        # Create tunnel and do first dig
        dig_service.dig(uid, TEST_GUILD_ID)
        # Do another dig to start cooldown
        dig_service.dig(uid, TEST_GUILD_ID)

        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal is not None
        assert terminal.get("paid_dig_available") is True
        assert preconditions is None


# ---------------------------------------------------------------------------
# apply_dig_outcome
# ---------------------------------------------------------------------------

class TestApplyDigOutcome:
    def _get_preconditions(self, dig_service, uid):
        """Helper: compute preconditions for a ready-to-dig player."""
        # Move past first dig
        dig_service.dig(uid, TEST_GUILD_ID)
        # Reset cooldown
        dig_service.reset_dig_cooldown(uid, TEST_GUILD_ID)
        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal is None
        return preconditions

    def test_normal_outcome(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)
        depth_before = p["depth_before"]

        outcome = {
            "advance": 2, "jc_earned": 3, "cave_in": False,
            "event_id": "",
        }
        result = dig_service.apply_dig_outcome(p, outcome)
        assert result["success"] is True
        assert result["advance"] == 2
        assert result["depth_after"] == depth_before + 2
        # Narrative is no longer set by apply_dig_outcome
        assert "llm_narrative" not in result

    def test_cave_in_outcome(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        outcome = {
            "advance": 0, "jc_earned": 0, "cave_in": True,
            "cave_in_block_loss": 4, "cave_in_type": "stun",
        }
        result = dig_service.apply_dig_outcome(p, outcome)
        assert result["success"] is True
        assert result["cave_in"] is True
        assert result["cave_in_detail"]["type"] == "stun"

    def test_boss_boundary_capped(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)
        # Set depth to just below boss boundary (25)
        dig_service.dig_repo.update_tunnel(uid, TEST_GUILD_ID, depth=23)
        p["depth_before"] = 23

        outcome = {
            "advance": 10, "jc_earned": 5, "cave_in": False,
            "event_id": "",
        }
        result = dig_service.apply_dig_outcome(p, outcome)
        # Should be capped to boss boundary - 1
        assert result["depth_after"] == 24
        assert result["boss_encounter"] is True

    def test_jc_added_to_balance(self, dig_service, player_repository):
        uid = _register_player(player_repository, balance=100)
        p = self._get_preconditions(dig_service, uid)

        outcome = {
            "advance": 1, "jc_earned": 5, "cave_in": False,
            "event_id": "",
        }
        dig_service.apply_dig_outcome(p, outcome)
        balance = player_repository.get_balance(uid, TEST_GUILD_ID)
        assert balance >= 100


# ---------------------------------------------------------------------------
# DigLLMService.run_dig
# ---------------------------------------------------------------------------

class TestDigLLMServiceRunDig:
    def _make_service(self, dig_repo, player_repo, dig_service, tool_result=None, side_effect=None):
        ai_service = MagicMock()
        if side_effect:
            ai_service.call_with_tools = AsyncMock(side_effect=side_effect)
        elif tool_result:
            ai_service.call_with_tools = AsyncMock(return_value=tool_result)
        else:
            # Default: 3-call flow — dice → resolve (mechanics) → narrate
            ai_service.call_with_tools = AsyncMock(side_effect=[
                _MockToolCallResult(
                    tool_name="roll_dice",
                    tool_args={"rolls": [{"label": "cave_in", "sides": 100}]},
                ),
                _MockToolCallResult(
                    tool_name="resolve_dig",
                    tool_args={
                        "advance": 2, "jc_earned": 3, "cave_in": False,
                        "event_id": "",
                    },
                ),
                _MockToolCallResult(
                    tool_name="narrate_dig_outcome",
                    tool_args={
                        "narrative": "DM narrates the dig.",
                        "tone": "dramatic",
                    },
                ),
            ])
        return DigLLMService(ai_service, dig_repo, player_repo, dig_service=dig_service)

    def _get_preconditions(self, dig_service, uid):
        dig_service.dig(uid, TEST_GUILD_ID)
        dig_service.reset_dig_cooldown(uid, TEST_GUILD_ID)
        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal is None
        return preconditions

    @pytest.mark.asyncio
    async def test_run_dig_applies_dm_outcome(self, dig_repo, player_repository, dig_service):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        svc = self._make_service(dig_repo, player_repository, dig_service)
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)

        assert result["success"] is True
        assert result["advance"] == 2
        assert result["llm_narrative"] == "DM narrates the dig."

    @pytest.mark.asyncio
    async def test_run_dig_rolls_dice_before_resolving(self, dig_repo, player_repository, dig_service):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        ai_service = MagicMock()
        ai_service.call_with_tools = AsyncMock(side_effect=[
            _MockToolCallResult(
                tool_name="roll_dice",
                tool_args={"rolls": [{"label": "cave_in", "sides": 100}]},
            ),
            _MockToolCallResult(
                tool_name="resolve_dig",
                tool_args={
                    "advance": 1, "jc_earned": 0, "cave_in": False, "event_id": "",
                },
            ),
            _MockToolCallResult(
                tool_name="narrate_dig_outcome",
                tool_args={
                    "narrative": "The dice clatter in the dark.",
                    "tone": "ominous",
                },
            ),
        ])
        svc = DigLLMService(ai_service, dig_repo, player_repository, dig_service=dig_service)
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)

        assert result["success"] is True
        assert result["llm_narrative"] == "The dice clatter in the dark."
        assert ai_service.call_with_tools.call_count == 3
        # Second call (resolve) should see dice results
        resolve_messages = ai_service.call_with_tools.call_args_list[1].args[0]
        assert "DICE RESULTS" in resolve_messages[1]["content"]

    @pytest.mark.asyncio
    async def test_run_dig_fallback_on_timeout(self, dig_repo, player_repository, dig_service):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        svc = self._make_service(
            dig_repo, player_repository, dig_service,
            side_effect=TimeoutError(),
        )
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)

        # Should still succeed via deterministic fallback
        assert result["success"] is True
        assert result.get("advance", 0) >= 0

    @pytest.mark.asyncio
    async def test_run_dig_fallback_on_api_error(self, dig_repo, player_repository, dig_service):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        svc = self._make_service(
            dig_repo, player_repository, dig_service,
            side_effect=RuntimeError("API down"),
        )
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)

        assert result["success"] is True

    @pytest.mark.asyncio
    async def test_run_dig_wrong_tool_falls_back(self, dig_repo, player_repository, dig_service):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        svc = self._make_service(
            dig_repo, player_repository, dig_service,
            tool_result=_MockToolCallResult(tool_name="wrong_tool", tool_args={}),
        )
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)

        # Falls back to deterministic
        assert result["success"] is True
