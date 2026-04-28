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
    DIG_ENGINE_SYSTEM_PROMPT,
    DIG_ENGINE_TOOL,
    DIG_OUTCOME_TOOL,
    DIG_SYSTEM_PROMPT,
    PLAY_STYLE_DESCRIPTIONS,
    build_dice_results_context,
    build_dig_history_context,
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


@pytest.mark.parametrize(
    "histogram,expected",
    [
        ({"safe": 2, "risky": 1}, "unknown"),
        ({}, "unknown"),
        ({"safe": 80, "risky": 10, "desperate": 1}, "cautious_grinder"),
        ({"safe": 10, "risky": 20, "desperate": 10}, "reckless_degen"),
        ({"safe": 10, "risky": 50, "desperate": 5}, "calculated_risk_taker"),
        ({"safe": 5, "risky": 5, "help": 20}, "social_butterfly"),
        ({"safe": 10, "risky": 8, "desperate": 2}, "balanced_explorer"),
    ],
)
def test_classify_play_style(histogram, expected):
    assert classify_play_style(histogram) == expected


# ---------------------------------------------------------------------------
# DigLLMValidator
# ---------------------------------------------------------------------------


class TestDigLLMValidator:
    def setup_method(self):
        self.validator = DigLLMValidator()

    def test_valid_narrative_keeps_fields(self):
        result = self.validator.validate_narrative({
            "narrative": "You swing your pickaxe.",
            "tone": "dramatic",
        })
        assert result["narrative"] == "You swing your pickaxe."
        assert result["tone"] == "dramatic"
        assert result["event_flavor"] == ""
        assert result["cave_in_flavor"] == ""
        assert result["callback_reference"] == ""

    def test_invalid_tone_defaults_to_dramatic(self):
        result = self.validator.validate_narrative({"narrative": "Hello", "tone": "invalid"})
        assert result["tone"] == "dramatic"

    def test_all_tones_valid(self):
        for tone in ("dramatic", "humorous", "ominous", "triumphant", "melancholy", "absurd"):
            result = self.validator.validate_narrative({"narrative": "test", "tone": tone})
            assert result["tone"] == tone

    @pytest.mark.parametrize(
        "field,limit,padded_size",
        [
            ("narrative", 500, 600),
            ("event_flavor", 300, 400),
            ("cave_in_flavor", 300, 400),
            ("callback_reference", 200, 250),
        ],
    )
    def test_long_text_truncated_with_ellipsis(self, field, limit, padded_size):
        payload = {"narrative": "Hi", "tone": "dramatic", field: "X" * padded_size}
        result = self.validator.validate_narrative(payload)
        assert len(result[field]) == limit
        assert result[field].endswith("...")

    def test_missing_fields_get_defaults(self):
        result = self.validator.validate_narrative({})
        assert result == {
            "narrative": "",
            "tone": "dramatic",
            "event_flavor": "",
            "cave_in_flavor": "",
            "callback_reference": "",
        }


# ---------------------------------------------------------------------------
# Context builders — happy paths + key features wired through
# ---------------------------------------------------------------------------


class TestBuildPlayerStateContext:
    def test_basic_tunnel_includes_depth_tier_and_balance(self):
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

    def test_miner_profile_included(self):
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

    def test_empty_tunnel_uses_defaults(self):
        ctx = build_player_state_context({}, 0)
        assert "Dirt" in ctx
        assert "Wooden" in ctx

    @pytest.mark.parametrize(
        "field,value,must_contain",
        [
            ("equipped_relics", json.dumps([{"name": "Mole Claws"}]), ["Mole Claws"]),
            ("mutations", json.dumps([{"name": "Dark Sight"}]), ["Dark Sight"]),
            (
                "temp_buffs",
                json.dumps([{"name": "Second Wind", "digs_remaining": 2}]),
                ["Second Wind", "2 digs left"],
            ),
        ],
    )
    def test_json_collections_render_into_context(self, field, value, must_contain):
        tunnel = {"depth": 50, field: value}
        ctx = build_player_state_context(tunnel, 0)
        for token in must_contain:
            assert token in ctx


class TestBuildPersonalityContext:
    @pytest.mark.parametrize("personality", [None, {}])
    def test_blank_personality_falls_back_to_default_blurb(self, personality):
        assert build_personality_context(personality) == "New player, no history yet."

    def test_play_style_and_histogram_render(self):
        personality = {
            "play_style": "balanced_explorer",
            "choice_histogram": {"safe": 10, "risky": 8},
            "notable_moments": ["Slew their first boss"],
        }
        ctx = build_personality_context(personality)
        assert "balanced_explorer" in ctx
        assert "safe: 10" in ctx
        assert "risky: 8" in ctx
        assert "first boss" in ctx


class TestBuildDigOutcomeContext:
    @pytest.mark.parametrize(
        "result,must_contain",
        [
            ({"advance": 3, "jc_earned": 5}, ["+3 blocks", "JC earned: 5"]),
            (
                {
                    "advance": 0,
                    "jc_earned": 0,
                    "cave_in": True,
                    "cave_in_detail": {"block_loss": 5, "message": "Rocks tumbled."},
                },
                ["CAVE-IN", "5 blocks"],
            ),
            (
                {
                    "advance": 2,
                    "jc_earned": 3,
                    "event": {"id": "crystal_golem", "name": "Crystal Golem", "description": "!"},
                },
                ["Crystal Golem"],
            ),
            ({}, ["+0 blocks"]),
        ],
    )
    def test_dig_outcome_renders_expected_tokens(self, result, must_contain):
        ctx = build_dig_outcome_context(result)
        for token in must_contain:
            assert token in ctx


class TestBuildMultiplayerContext:
    @pytest.mark.parametrize("actions", [[], None])
    def test_blank_actions_returns_empty(self, actions):
        assert build_multiplayer_context(actions) == ""

    def test_actions_render_with_serialized_details(self):
        actions = [
            {
                "action_type": "sabotage",
                "actor_id": 111,
                "target_id": 222,
                "detail": json.dumps({"damage": 5, "target_id": 222}),
            },
            {
                "action_type": "help",
                "actor_id": 333,
                "target_id": 222,
                "detail": json.dumps({"advance": 3, "target_id": 222}),
            },
        ]
        ctx = build_multiplayer_context(actions)
        assert "Sabotage" in ctx
        assert "Help" in ctx

    def test_active_cheers_visible(self):
        import time as _time
        future = int(_time.time()) + 3600
        tunnel = {
            "cheer_data": json.dumps([
                {"cheerer_id": 111, "expires_at": future},
                {"cheerer_id": 222, "expires_at": future},
            ])
        }
        ctx = build_multiplayer_context([], tunnel=tunnel)
        assert "+10%" in ctx
        assert "+2 advance" in ctx

    def test_expired_state_is_filtered_out(self):
        """Both expired cheers and expired revenge windows must not appear."""
        tunnel_cheers = {"cheer_data": json.dumps([{"cheerer_id": 111, "expires_at": 1000}])}
        assert "Cheer" not in build_multiplayer_context([], tunnel=tunnel_cheers)

        tunnel_revenge = {"revenge_target": 999, "revenge_type": "damage", "revenge_until": 1000}
        assert "Revenge" not in build_multiplayer_context([], tunnel=tunnel_revenge)

    def test_revenge_window_active_renders(self):
        import time as _time
        tunnel = {
            "revenge_target": 999,
            "revenge_type": "damage",
            "revenge_until": int(_time.time()) + 7200,
        }
        ctx = build_multiplayer_context([], tunnel=tunnel)
        assert "Revenge" in ctx
        assert "damage" in ctx

    def test_trap_and_insurance_and_injury_render(self):
        import time as _time
        future = int(_time.time()) + 3600
        assert "armed" in build_multiplayer_context([], tunnel={"trap_active": 1})
        assert "insured" in build_multiplayer_context([], tunnel={"insured_until": future})
        injury = {"injury_state": json.dumps({"type": "reduced_advance", "digs_remaining": 2})}
        ctx = build_multiplayer_context([], tunnel=injury)
        assert "Injury" in ctx
        assert "2 digs" in ctx

    @pytest.mark.parametrize("rank,expected", [(3, "#3"), (0, None)])
    def test_rank_visibility(self, rank, expected):
        ctx = build_multiplayer_context([], rank=rank)
        if expected:
            assert expected in ctx
        else:
            assert "Rank" not in ctx


class TestBuildDigHistoryContext:
    @pytest.mark.parametrize("tunnel", [{}, None])
    def test_blank_history_returns_empty(self, tunnel):
        assert build_dig_history_context([], tunnel) == ""

    def test_lifetime_stats_render(self):
        tunnel = {"total_digs": 50, "total_jc_earned": 200, "max_depth": 60}
        ctx = build_dig_history_context([], tunnel)
        assert "50 digs" in ctx
        assert "200 JC" in ctx

    def test_dig_summary_counts_advances_and_cave_ins(self):
        actions = []
        for i in range(10):
            cave_in = i < 3
            actions.append({
                "action_type": "dig",
                "depth_before": 40 + i,
                "depth_after": 40 + i + (0 if cave_in else 2),
                "detail": json.dumps({"cave_in": cave_in, "block_loss": 5 if cave_in else 0}),
            })
        ctx = build_dig_history_context(actions, {})
        assert "7 advances" in ctx
        assert "3 cave-ins" in ctx
        assert "1 digs ago" in ctx  # most recent cave-in

    def test_depth_trend_shows_range_and_max(self):
        actions = [
            {"action_type": "dig", "depth_before": 50, "depth_after": 52, "detail": "{}"},
            {"action_type": "dig", "depth_before": 38, "depth_after": 40, "detail": "{}"},
        ]
        ctx = build_dig_history_context(actions, {"max_depth": 74})
        assert "38->52" in ctx
        assert "+14" in ctx
        assert "74" in ctx

    def test_boss_progress_and_attempts_render(self):
        tunnel = {
            "boss_progress": json.dumps({"25": "defeated", "50": "defeated", "75": "active"}),
            "boss_attempts": 2,
        }
        ctx = build_dig_history_context([], tunnel)
        assert "✓" in ctx
        assert "2 attempts" in ctx

    @pytest.mark.parametrize(
        "tunnel,must_contain,must_not_contain",
        [
            ({"prestige_level": 2, "best_run_score": 450}, ["Prestige", "450"], []),
            ({"prestige_level": 0}, [], ["Prestige"]),
        ],
    )
    def test_prestige_visibility(self, tunnel, must_contain, must_not_contain):
        ctx = build_dig_history_context([], tunnel)
        for token in must_contain:
            assert token in ctx
        for token in must_not_contain:
            assert token not in ctx

    def test_recent_events_and_boss_fights_render(self):
        actions = [
            {"action_type": "event", "detail": json.dumps({"event_id": "crystal_golem"})},
            {
                "action_type": "boss_fight",
                "detail": json.dumps({"won": True, "risk": "reckless", "boundary": 50}),
            },
            {
                "action_type": "boss_fight",
                "detail": json.dumps({"won": False, "risk": "bold", "boundary": 75}),
            },
        ]
        ctx = build_dig_history_context(actions, {})
        assert "crystal_golem" in ctx
        assert "won" in ctx
        assert "lost" in ctx
        assert "reckless" in ctx

    def test_invalid_detail_json_does_not_crash(self):
        actions = [
            {"action_type": "dig", "depth_before": 10, "depth_after": 12, "detail": "not json"},
        ]
        ctx = build_dig_history_context(actions, {})
        assert "1 advances" in ctx


class TestBuildMessages:
    def test_structure_includes_state_personality_outcome(self):
        msgs = build_messages("system", "state", "personality", "outcome", "")
        assert len(msgs) == 2
        assert msgs[0] == {"role": "system", "content": "system"}
        assert msgs[1]["role"] == "user"
        for label in ("PLAYER STATE", "PERSONALITY", "DIG OUTCOME"):
            assert label in msgs[1]["content"]
        assert "MULTIPLAYER" not in msgs[1]["content"]

    def test_optional_sections_ordering(self):
        msgs = build_messages(
            "system",
            "state",
            "personality",
            "outcome",
            "sabotage stuff",
            history="some history",
        )
        content = msgs[1]["content"]
        assert "MULTIPLAYER" in content
        assert "DIG HISTORY" in content
        # History sits between personality and outcome
        assert content.index("PERSONALITY") < content.index("DIG HISTORY") < content.index("DIG OUTCOME")


# ---------------------------------------------------------------------------
# Prompt constants and engine tool definition
# ---------------------------------------------------------------------------


def test_outcome_tool_has_expected_shape():
    """DIG_OUTCOME_TOOL covers narration only; engine tool covers mechanics only."""
    assert DIG_OUTCOME_TOOL["type"] == "function"
    func = DIG_OUTCOME_TOOL["function"]
    assert func["name"] == "narrate_dig_outcome"
    props = func["parameters"]["properties"]
    for field in ("narrative", "tone", "event_flavor", "cave_in_flavor", "callback_reference"):
        assert field in props
    assert set(func["parameters"]["required"]) == {"narrative", "tone"}
    assert len(DIG_SYSTEM_PROMPT) > 100
    expected = {
        "cautious_grinder",
        "reckless_degen",
        "calculated_risk_taker",
        "balanced_explorer",
        "social_butterfly",
        "unknown",
    }
    assert set(PLAY_STYLE_DESCRIPTIONS.keys()) == expected


def test_engine_tool_separates_mechanics_from_narrative():
    assert DIG_ENGINE_TOOL["type"] == "function"
    func = DIG_ENGINE_TOOL["function"]
    assert func["name"] == "resolve_dig"
    props = func["parameters"]["properties"]
    for field in ("advance", "jc_earned", "cave_in", "event_id"):
        assert field in props
    assert "narrative" not in props
    assert "tone" not in props
    assert {"advance", "jc_earned", "cave_in"} <= set(func["parameters"]["required"])
    assert len(DIG_ENGINE_SYSTEM_PROMPT) > 100
    assert "Dungeon Master" in DIG_ENGINE_SYSTEM_PROMPT

    msgs = build_engine_messages("sys", "state", "pers", "precond", "")
    assert msgs[0]["role"] == "system"
    assert "DIG PRECONDITIONS" in msgs[1]["content"]
    assert "MULTIPLAYER" not in msgs[1]["content"]


# ---------------------------------------------------------------------------
# build_preconditions_context
# ---------------------------------------------------------------------------


class TestBuildPreconditionsContext:
    def test_basic_renders_layer_ranges_and_stats(self):
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
            "stat_effects": {
                "advance_min_bonus": 0,
                "advance_max_bonus": 1,
                "cave_in_reduction": 0.06,
                "cooldown_multiplier": 0.96,
            },
        }
        ctx = build_preconditions_context(p)
        assert "42" in ctx
        assert "Stone" in ctx
        assert "2-4" in ctx
        assert "15%" in ctx
        assert "crystal_golem" in ctx
        assert "Strength 2" in ctx
        assert "-6% cave-in" in ctx

    @pytest.mark.parametrize(
        "extras,must_contain",
        [
            ({"has_dynamite": True, "has_grappling_hook": True}, ["Dynamite", "Grappling Hook"]),
            (
                {"has_hard_hat": True, "hard_hat_prevents": True, "hard_hat_charges": 2},
                ["Cave-in chance: 0%", "Hard Hat", "cannot happen this dig"],
            ),
        ],
    )
    def test_item_state_renders(self, extras, must_contain):
        p = {
            "depth_before": 10,
            "layer_name": "Dirt",
            "advance_min": 1,
            "advance_max": 3,
            "jc_min": 0,
            "jc_max": 1,
            "cave_in_chance": 0.05,
            "event_chance": 0.16,
            "available_events": [],
            "luminosity": 100,
            **extras,
        }
        ctx = build_preconditions_context(p)
        for token in must_contain:
            assert token in ctx


def test_build_dice_results_context_formats_rolls():
    ctx = build_dice_results_context([
        {"label": "cave_in", "sides": 100, "rolls": [42], "modifier": 0, "total": 42},
        {"label": "loss", "sides": 6, "rolls": [4], "modifier": 2, "total": 6},
    ])
    assert "cave_in" in ctx
    assert "d100" in ctx
    assert "loss" in ctx
    assert "+ 2 = 6" in ctx


# ---------------------------------------------------------------------------
# DigLLMValidator.validate_engine_outcome — clamping logic
# ---------------------------------------------------------------------------


class TestValidateEngineOutcome:
    def setup_method(self):
        self.validator = DigLLMValidator()

    def _preconditions(self, **overrides):
        base = {
            "advance_min": 1,
            "advance_max": 3,
            "jc_min": 0,
            "jc_max": 5,
            "hard_hat_prevents": False,
            "available_events": [
                {"id": "crystal_golem", "name": "Crystal Golem", "rarity": "common"},
            ],
        }
        base.update(overrides)
        return base

    @pytest.mark.parametrize(
        "outcome,field,expected",
        [
            ({"advance": 10, "jc_earned": 3, "cave_in": False}, "advance", 3),
            ({"advance": -5, "jc_earned": 2, "cave_in": False}, "advance", 1),
            ({"advance": 2, "jc_earned": 100, "cave_in": False}, "jc_earned", 5),
        ],
    )
    def test_clamps_advance_and_jc(self, outcome, field, expected):
        result = self.validator.validate_engine_outcome(outcome, self._preconditions())
        assert result[field] == expected

    def test_cave_in_forces_zero_advance_and_jc(self):
        result = self.validator.validate_engine_outcome(
            {
                "advance": 3,
                "jc_earned": 5,
                "cave_in": True,
                "cave_in_block_loss": 5,
                "cave_in_type": "stun",
            },
            self._preconditions(),
        )
        assert result["cave_in"] is True
        assert result["advance"] == 0
        assert result["jc_earned"] == 0

    def test_cave_in_block_loss_clamped(self):
        result = self.validator.validate_engine_outcome(
            {
                "advance": 0,
                "jc_earned": 0,
                "cave_in": True,
                "cave_in_block_loss": 15,
                "cave_in_type": "injury",
            },
            self._preconditions(),
        )
        assert result["cave_in_block_loss"] == 8

    def test_hard_hat_prevents_cave_in(self):
        result = self.validator.validate_engine_outcome(
            {
                "advance": 2,
                "jc_earned": 3,
                "cave_in": True,
                "cave_in_block_loss": 5,
                "cave_in_type": "stun",
            },
            self._preconditions(hard_hat_prevents=True),
        )
        assert result["cave_in"] is False
        assert result["cave_in_block_loss"] == 0
        assert "narrative" not in result

    @pytest.mark.parametrize(
        "event_id,expected",
        [("nonexistent_event", ""), ("crystal_golem", "crystal_golem")],
    )
    def test_event_id_validation(self, event_id, expected):
        result = self.validator.validate_engine_outcome(
            {"advance": 2, "jc_earned": 3, "cave_in": False, "event_id": event_id},
            self._preconditions(),
        )
        assert result["event_id"] == expected

    def test_engine_outcome_carries_no_narrative_fields(self):
        result = self.validator.validate_engine_outcome(
            {"advance": 2, "jc_earned": 3, "cave_in": False},
            self._preconditions(),
        )
        assert "narrative" not in result
        assert "tone" not in result


# ---------------------------------------------------------------------------
# DigLLMService — enhance (legacy single-call narration path)
# ---------------------------------------------------------------------------


@dataclass
class _MockToolCallResult:
    tool_name: str | None
    tool_args: dict[str, Any]
    content: str | None = None
    raw_response: Any = None


class TestDigLLMServiceEnhance:
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
        enhanced = await svc.enhance({"advance": 3, "jc_earned": 5, "depth": 10}, uid, TEST_GUILD_ID)

        assert enhanced["llm_narrative"] == "The earth shudders as your pickaxe bites into stone."
        assert enhanced["llm_tone"] == "dramatic"

    @pytest.mark.asyncio
    @pytest.mark.parametrize("side_effect", [TimeoutError(), RuntimeError("API down")])
    async def test_enhance_falls_back_silently_on_error(
        self, side_effect, dig_repo, player_repository
    ):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository, side_effect=side_effect)
        enhanced = await svc.enhance({"advance": 3, "jc_earned": 5}, uid, TEST_GUILD_ID)

        assert "llm_narrative" not in enhanced
        assert enhanced["advance"] == 3

    @pytest.mark.asyncio
    async def test_enhance_falls_back_on_wrong_tool(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(tool_name="wrong_tool", tool_args={}),
        )
        enhanced = await svc.enhance({"advance": 1, "jc_earned": 2}, uid, TEST_GUILD_ID)
        assert "llm_narrative" not in enhanced

    @pytest.mark.asyncio
    async def test_enhance_works_without_tunnel(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        svc = self._make_service(dig_repo, player_repository)
        enhanced = await svc.enhance({"advance": 1, "jc_earned": 1}, uid, TEST_GUILD_ID)
        assert "advance" in enhanced


# ---------------------------------------------------------------------------
# DigLLMService — update_personality
# ---------------------------------------------------------------------------


class TestDigLLMServicePersonality:
    def _make_service(self, dig_repo, player_repository):
        return DigLLMService(MagicMock(), dig_repo, player_repository)

    def test_update_personality_creates_new(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="safe")

        personality = dig_repo.get_personality(uid, TEST_GUILD_ID)
        assert personality is not None
        assert personality["choice_histogram"].get("safe") == 1

    def test_update_personality_increments(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        for _ in range(5):
            svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="safe")
        svc.update_personality(uid, TEST_GUILD_ID, "event", choice="risky")

        histogram = dig_repo.get_personality(uid, TEST_GUILD_ID)["choice_histogram"]
        assert histogram["safe"] == 5
        assert histogram["risky"] == 1

    def test_update_personality_records_notable_moment(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        svc.update_personality(
            uid, TEST_GUILD_ID, "boss_fight", details={"first_boss_kill": True}
        )

        moments = dig_repo.get_personality(uid, TEST_GUILD_ID).get("notable_moments", [])
        assert len(moments) == 1
        assert "first boss" in moments[0].lower()

    def test_update_personality_classifies_after_threshold(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        for _ in range(8):
            svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="safe")

        assert dig_repo.get_personality(uid, TEST_GUILD_ID)["play_style"] == "cautious_grinder"

    def test_invalid_choice_ignored(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="invalid_choice")

        histogram = dig_repo.get_personality(uid, TEST_GUILD_ID).get("choice_histogram", {})
        assert "invalid_choice" not in histogram


# ---------------------------------------------------------------------------
# Repository — engine mode + personality + social actions
# ---------------------------------------------------------------------------


class TestDigRepoEngineMode:
    def test_default_mode_is_legacy_with_or_without_tunnel(self, dig_repo, player_repository):
        # No tunnel
        assert dig_repo.get_engine_mode(99999, TEST_GUILD_ID) == "legacy"
        # With tunnel
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")
        assert dig_repo.get_engine_mode(uid, TEST_GUILD_ID) == "legacy"

    def test_mode_can_round_trip_through_llm(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")
        dig_repo.set_engine_mode(uid, TEST_GUILD_ID, "llm")
        assert dig_repo.get_engine_mode(uid, TEST_GUILD_ID) == "llm"
        dig_repo.set_engine_mode(uid, TEST_GUILD_ID, "legacy")
        assert dig_repo.get_engine_mode(uid, TEST_GUILD_ID) == "legacy"


class TestDigRepoPersonality:
    def test_get_personality_none_when_missing(self, dig_repo):
        assert dig_repo.get_personality(99999, TEST_GUILD_ID) is None

    def test_upsert_persists_and_round_trips(self, dig_repo):
        data = {
            "play_style": "cautious_grinder",
            "choice_histogram": {"safe": 10, "risky": 3},
            "notable_moments": ["Slew first boss"],
        }
        dig_repo.upsert_personality(10001, TEST_GUILD_ID, data)
        result = dig_repo.get_personality(10001, TEST_GUILD_ID)
        assert result["play_style"] == "cautious_grinder"
        assert result["choice_histogram"]["safe"] == 10
        assert len(result["notable_moments"]) == 1

    def test_upsert_overwrites_existing(self, dig_repo):
        dig_repo.upsert_personality(
            10001, TEST_GUILD_ID,
            {"play_style": "unknown", "choice_histogram": {"safe": 1}, "notable_moments": []},
        )
        dig_repo.upsert_personality(
            10001, TEST_GUILD_ID,
            {
                "play_style": "reckless_degen",
                "choice_histogram": {"safe": 1, "desperate": 5},
                "notable_moments": ["Cave-in streak"],
            },
        )
        result = dig_repo.get_personality(10001, TEST_GUILD_ID)
        assert result["play_style"] == "reckless_degen"
        assert result["choice_histogram"]["desperate"] == 5


class TestDigRepoSocialActions:
    def test_empty_when_none_logged(self, dig_repo):
        assert dig_repo.get_recent_social_actions(10001, TEST_GUILD_ID) == []

    def test_returns_only_social_actions(self, dig_repo):
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID, actor_id=10001, target_id=10002,
            action_type="sabotage", depth_before=50, depth_after=45, jc_delta=-5,
        )
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID, actor_id=10003, target_id=10001,
            action_type="help", depth_before=45, depth_after=48,
        )
        # Non-social action should NOT be returned
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID, actor_id=10001,
            action_type="dig", depth_before=48, depth_after=50,
        )

        actions = dig_repo.get_recent_social_actions(10001, TEST_GUILD_ID)
        assert {a["action_type"] for a in actions} == {"sabotage", "help"}


# ---------------------------------------------------------------------------
# dig_with_preconditions — terminal states + happy path
# ---------------------------------------------------------------------------


class TestDigWithPreconditions:
    def test_returns_preconditions_for_normal_dig(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        dig_service.dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")
        dig_service.dig(uid, TEST_GUILD_ID)
        dig_service.reset_dig_cooldown(uid, TEST_GUILD_ID)

        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal is None
        assert preconditions is not None
        for k in ("advance_min", "advance_max", "jc_min", "jc_max", "cave_in_chance", "available_events"):
            assert k in preconditions
        assert preconditions["discord_id"] == uid
        assert preconditions["depth_before"] >= 0

    def test_unregistered_returns_terminal_failure(self, dig_service):
        terminal, preconditions = dig_service.dig_with_preconditions(99999, TEST_GUILD_ID)
        assert terminal is not None
        assert terminal["success"] is False
        assert preconditions is None

    def test_first_dig_returns_terminal(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal.get("is_first_dig") is True
        assert preconditions is None

    def test_cooldown_returns_terminal(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        dig_service.dig(uid, TEST_GUILD_ID)
        dig_service.dig(uid, TEST_GUILD_ID)
        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal.get("paid_dig_available") is True
        assert preconditions is None


# ---------------------------------------------------------------------------
# apply_dig_outcome
# ---------------------------------------------------------------------------


class TestApplyDigOutcome:
    def _get_preconditions(self, dig_service, uid):
        dig_service.dig(uid, TEST_GUILD_ID)
        dig_service.reset_dig_cooldown(uid, TEST_GUILD_ID)
        terminal, preconditions = dig_service.dig_with_preconditions(uid, TEST_GUILD_ID)
        assert terminal is None
        return preconditions

    def test_normal_outcome_advances_depth(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)
        depth_before = p["depth_before"]

        result = dig_service.apply_dig_outcome(
            p, {"advance": 2, "jc_earned": 3, "cave_in": False, "event_id": ""}
        )
        assert result["success"] is True
        assert result["advance"] == 2
        assert result["depth_after"] == depth_before + 2
        assert "llm_narrative" not in result

    def test_cave_in_outcome_records_detail(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        result = dig_service.apply_dig_outcome(
            p,
            {
                "advance": 0,
                "jc_earned": 0,
                "cave_in": True,
                "cave_in_block_loss": 4,
                "cave_in_type": "stun",
            },
        )
        assert result["success"] is True
        assert result["cave_in"] is True
        assert result["cave_in_detail"]["type"] == "stun"

    def test_advance_capped_at_boss_boundary(self, dig_service, player_repository):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)
        dig_service.dig_repo.update_tunnel(uid, TEST_GUILD_ID, depth=23)
        p["depth_before"] = 23

        result = dig_service.apply_dig_outcome(
            p, {"advance": 10, "jc_earned": 5, "cave_in": False, "event_id": ""}
        )
        # Boss boundary at 25 — advance is capped to boundary - 1
        assert result["depth_after"] == 24
        assert result["boss_encounter"] is True

    def test_jc_added_to_balance(self, dig_service, player_repository):
        uid = _register_player(player_repository, balance=100)
        p = self._get_preconditions(dig_service, uid)

        dig_service.apply_dig_outcome(
            p, {"advance": 1, "jc_earned": 5, "cave_in": False, "event_id": ""}
        )
        assert player_repository.get_balance(uid, TEST_GUILD_ID) >= 100


# ---------------------------------------------------------------------------
# DigLLMService.run_dig — two-call orchestration + fallbacks
# ---------------------------------------------------------------------------


class TestDigLLMServiceRunDig:
    def _make_service(self, dig_repo, player_repo, dig_service, tool_result=None, side_effect=None):
        ai_service = MagicMock()
        if side_effect:
            ai_service.call_with_tools = AsyncMock(side_effect=side_effect)
        elif tool_result:
            ai_service.call_with_tools = AsyncMock(return_value=tool_result)
        else:
            ai_service.call_with_tools = AsyncMock(side_effect=[
                _MockToolCallResult(
                    tool_name="resolve_dig",
                    tool_args={"advance": 2, "jc_earned": 3, "cave_in": False, "event_id": ""},
                ),
                _MockToolCallResult(
                    tool_name="narrate_dig_outcome",
                    tool_args={"narrative": "DM narrates the dig.", "tone": "dramatic"},
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
    async def test_run_dig_applies_outcome_with_narrative(
        self, dig_repo, player_repository, dig_service
    ):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        svc = self._make_service(dig_repo, player_repository, dig_service)
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)

        assert result["success"] is True
        assert result["advance"] == 2
        assert result["llm_narrative"] == "DM narrates the dig."

    @pytest.mark.asyncio
    async def test_run_dig_passes_dice_results_to_resolve_call(
        self, dig_repo, player_repository, dig_service
    ):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        ai_service = MagicMock()
        ai_service.call_with_tools = AsyncMock(side_effect=[
            _MockToolCallResult(
                tool_name="resolve_dig",
                tool_args={"advance": 1, "jc_earned": 0, "cave_in": False, "event_id": ""},
            ),
            _MockToolCallResult(
                tool_name="narrate_dig_outcome",
                tool_args={"narrative": "The dice clatter in the dark.", "tone": "ominous"},
            ),
        ])
        svc = DigLLMService(ai_service, dig_repo, player_repository, dig_service=dig_service)
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)

        assert result["llm_narrative"] == "The dice clatter in the dark."
        assert ai_service.call_with_tools.call_count == 2
        resolve_messages = ai_service.call_with_tools.call_args_list[0].args[0]
        assert "DICE RESULTS" in resolve_messages[1]["content"]

    @pytest.mark.asyncio
    @pytest.mark.parametrize("side_effect", [TimeoutError(), RuntimeError("API down")])
    async def test_run_dig_falls_back_on_error(
        self, side_effect, dig_repo, player_repository, dig_service
    ):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        svc = self._make_service(
            dig_repo, player_repository, dig_service, side_effect=side_effect
        )
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)
        assert result["success"] is True
        assert result.get("advance", 0) >= 0

    @pytest.mark.asyncio
    async def test_run_dig_falls_back_on_wrong_tool(
        self, dig_repo, player_repository, dig_service
    ):
        uid = _register_player(player_repository)
        p = self._get_preconditions(dig_service, uid)

        svc = self._make_service(
            dig_repo, player_repository, dig_service,
            tool_result=_MockToolCallResult(tool_name="wrong_tool", tool_args={}),
        )
        result = await svc.run_dig(uid, TEST_GUILD_ID, p)
        assert result["success"] is True
