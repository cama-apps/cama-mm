"""Tests for the dig flavor LLM service + supporting helpers."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_flavor_service import (
    DigFlavorService,
    classify_play_style,
)
from services.dig_flavor_validator import (
    MEMORY_MAX_BYTES,
    NARRATIVE_MAX_CHARS,
    validate_flavor_args,
    validate_splash_narrative,
)
from services.dig_llm_prompts import (
    NARRATE_DIG_TOOL,
    PLAY_STYLE_DESCRIPTIONS,
    TONE_PROFILES,
    build_dig_flavor_system_prompt,
    build_dig_history_context,
    build_dig_outcome_context,
    build_messages,
    build_multiplayer_context,
    build_personality_context,
    build_player_state_context,
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


@dataclass
class _MockToolCallResult:
    tool_name: str | None
    tool_args: dict[str, Any]
    content: str | None = None
    raw_response: Any = None


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
# DigFlavorValidator
# ---------------------------------------------------------------------------


class TestValidateFlavorArgs:
    def _common(self, **overrides):
        base = {
            "eligible_event_ids": {"crystal_golem", "void_seam"},
            "rolled_tone": "industrial_grim",
            "bonus_cap_pct": 5.0,
        }
        base.update(overrides)
        return base

    def test_valid_minimal_args_returns_result(self):
        out = validate_flavor_args(
            {"narrative": "The walls weep dust.", "tone": "industrial_grim"},
            **self._common(),
        )
        assert out is not None
        assert out.narrative == "The walls weep dust."
        assert out.tone == "industrial_grim"
        assert out.flavor_bonus_pct == 0.0
        assert out.npc_appearance is None
        assert out.picked_event_id is None
        assert out.memory_update is None

    def test_empty_narrative_rejected(self):
        out = validate_flavor_args(
            {"narrative": "  ", "tone": "industrial_grim"},
            **self._common(),
        )
        assert out is None

    def test_numbers_in_narrative_rejected(self):
        out = validate_flavor_args(
            {"narrative": "You earned 5 JC.", "tone": "industrial_grim"},
            **self._common(),
        )
        assert out is None

    def test_long_narrative_truncated(self):
        long = "x " * 400
        out = validate_flavor_args(
            {"narrative": long, "tone": "cosmic_dread"},
            **self._common(),
        )
        assert out is not None
        assert len(out.narrative) <= NARRATIVE_MAX_CHARS

    def test_invalid_tone_falls_back_to_rolled(self):
        out = validate_flavor_args(
            {"narrative": "The deep stirs.", "tone": "made_up_tone"},
            **self._common(rolled_tone="cosmic_dread"),
        )
        assert out is not None
        assert out.tone == "cosmic_dread"

    @pytest.mark.parametrize(
        "raw_pct,cap,expected",
        [
            (3.0, 5.0, 3.0),
            (12.0, 5.0, 5.0),
            (-12.0, 5.0, -5.0),
            (8.5, 10.0, 8.5),
            (15.0, 10.0, 10.0),
        ],
    )
    def test_flavor_bonus_clamped_to_cap(self, raw_pct, cap, expected):
        out = validate_flavor_args(
            {
                "narrative": "Pickaxe sings.",
                "tone": "industrial_grim",
                "flavor_bonus_pct": raw_pct,
            },
            **self._common(bonus_cap_pct=cap),
        )
        assert out is not None
        assert out.flavor_bonus_pct == expected

    def test_picked_event_must_be_in_allow_list(self):
        out = validate_flavor_args(
            {
                "narrative": "Crystal hums.",
                "tone": "industrial_grim",
                "picked_event_id": "made_up_event",
            },
            **self._common(),
        )
        assert out is not None
        assert out.picked_event_id is None

    def test_picked_event_passes_when_in_list(self):
        out = validate_flavor_args(
            {
                "narrative": "Crystal hums.",
                "tone": "industrial_grim",
                "picked_event_id": "crystal_golem",
            },
            **self._common(),
        )
        assert out is not None
        assert out.picked_event_id == "crystal_golem"

    def test_npc_appearance_renders(self):
        out = validate_flavor_args(
            {
                "narrative": "Pickaxe sings.",
                "tone": "industrial_grim",
                "npc_appearance": {
                    "id_or_name": "the_old_hand",
                    "line": "Walked away from worse.",
                },
            },
            **self._common(),
        )
        assert out is not None
        assert out.npc_appearance == {
            "id_or_name": "the_old_hand",
            "line": "Walked away from worse.",
        }

    def test_npc_appearance_missing_line_dropped(self):
        out = validate_flavor_args(
            {
                "narrative": "Pickaxe sings.",
                "tone": "industrial_grim",
                "npc_appearance": {"id_or_name": "the_old_hand"},
            },
            **self._common(),
        )
        assert out is not None
        assert out.npc_appearance is None

    def test_memory_update_truncated_to_byte_cap(self):
        big = "y" * (MEMORY_MAX_BYTES * 2)
        out = validate_flavor_args(
            {
                "narrative": "Stone breathes.",
                "tone": "cosmic_dread",
                "memory_update": big,
            },
            **self._common(),
        )
        assert out is not None
        assert out.memory_update is not None
        assert len(out.memory_update.encode("utf-8")) <= MEMORY_MAX_BYTES


class TestValidateSplashNarrative:
    def test_under_cap_passes(self):
        assert validate_splash_narrative("Bob feels something missing.") == "Bob feels something missing."

    def test_truncates_over_cap(self):
        out = validate_splash_narrative("y" * 600)
        assert len(out) <= 200

    def test_blank_returns_empty(self):
        assert validate_splash_narrative("   ") == ""


# ---------------------------------------------------------------------------
# Tool definition + system prompt
# ---------------------------------------------------------------------------


def test_narrate_dig_tool_has_expected_shape():
    assert NARRATE_DIG_TOOL["type"] == "function"
    func = NARRATE_DIG_TOOL["function"]
    assert func["name"] == "narrate_dig"
    props = func["parameters"]["properties"]
    for field in (
        "narrative",
        "tone",
        "callback_reference",
        "picked_event_id",
        "npc_appearance",
        "flavor_bonus_pct",
        "memory_update",
    ):
        assert field in props
    assert set(func["parameters"]["required"]) == {"narrative", "tone"}


def test_play_style_descriptions_complete():
    expected = {
        "cautious_grinder",
        "reckless_degen",
        "calculated_risk_taker",
        "balanced_explorer",
        "social_butterfly",
        "unknown",
    }
    assert set(PLAY_STYLE_DESCRIPTIONS.keys()) == expected


def test_tone_profiles_have_three_voices():
    assert set(TONE_PROFILES.keys()) == {
        "cosmic_dread",
        "industrial_grim",
        "cryptic_folkloric",
    }


@pytest.mark.parametrize("tone", ["cosmic_dread", "industrial_grim", "cryptic_folkloric"])
def test_system_prompt_injects_tone_and_cap(tone):
    prompt = build_dig_flavor_system_prompt(tone, 5.0)
    assert tone in prompt
    assert "±5%" in prompt
    big_prompt = build_dig_flavor_system_prompt(tone, 10.0)
    assert "±10%" in big_prompt


# ---------------------------------------------------------------------------
# Context builders — happy paths
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

    def test_empty_tunnel_uses_defaults(self):
        ctx = build_player_state_context({}, 0)
        assert "Dirt" in ctx
        assert "Wooden" in ctx


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
            ({}, ["+0 blocks"]),
        ],
    )
    def test_dig_outcome_renders_expected_tokens(self, result, must_contain):
        ctx = build_dig_outcome_context(result)
        for token in must_contain:
            assert token in ctx


class TestBuildMessages:
    def test_structure_includes_state_personality_outcome(self):
        msgs = build_messages("system", "state", "personality", "outcome", "")
        assert len(msgs) == 2
        assert msgs[0] == {"role": "system", "content": "system"}
        for label in ("PLAYER STATE", "PERSONALITY", "DIG OUTCOME"):
            assert label in msgs[1]["content"]

    def test_optional_sections_render(self):
        msgs = build_messages(
            "system",
            "state",
            "personality",
            "outcome",
            "sabotage stuff",
            history="some history",
            dm_context="some memory",
            eligible_events="ev_a, ev_b",
        )
        content = msgs[1]["content"]
        for label in (
            "MULTIPLAYER",
            "DIG HISTORY",
            "DM CONTEXT",
            "ELIGIBLE EVENTS",
        ):
            assert label in content


class TestBuildMultiplayerContext:
    def test_blank_actions_returns_empty(self):
        assert build_multiplayer_context([]) == ""

    def test_actions_render_with_serialized_details(self):
        actions = [
            {
                "action_type": "sabotage",
                "actor_id": 111,
                "target_id": 222,
                "detail": json.dumps({"damage": 5, "target_id": 222}),
            },
        ]
        ctx = build_multiplayer_context(actions)
        assert "Sabotage" in ctx


class TestBuildDigHistoryContext:
    def test_blank_history_returns_empty(self):
        assert build_dig_history_context([], {}) == ""

    def test_lifetime_stats_render(self):
        tunnel = {"total_digs": 50, "total_jc_earned": 200, "max_depth": 60}
        ctx = build_dig_history_context([], tunnel)
        assert "50 digs" in ctx
        assert "200 JC" in ctx


# ---------------------------------------------------------------------------
# DigFlavorService.flavor — single LLM call, mocked
# ---------------------------------------------------------------------------


class TestDigFlavorServiceFlavor:
    def _make_service(
        self,
        dig_repo,
        player_repo,
        *,
        tool_result=None,
        side_effect=None,
        memory: str = "",
    ):
        ai_service = MagicMock()
        if side_effect is not None:
            ai_service.call_with_tools = AsyncMock(side_effect=side_effect)
        elif tool_result is not None:
            ai_service.call_with_tools = AsyncMock(return_value=tool_result)
        else:
            ai_service.call_with_tools = AsyncMock(return_value=_MockToolCallResult(
                tool_name="narrate_dig",
                tool_args={
                    "narrative": "The walls listen. They have always listened.",
                    "tone": "cryptic_folkloric",
                },
            ))

        # Stub context builder — sync `.build` is awaited via an AsyncMock.
        from services.dig_dm_context import DigDMContext

        ctx = DigDMContext(
            memory_blob=memory,
            last_match_summary="No recent inhouse matches.",
            economy_state="No outstanding loan.",
            server_activity="Server is quiet.",
            npc_roster=[],
        )
        context_builder = MagicMock()
        context_builder.build = AsyncMock(return_value=ctx)
        return DigFlavorService(ai_service, dig_repo, player_repo, context_builder)

    @pytest.mark.asyncio
    async def test_flavor_adds_narrative_keys(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        result = {"advance": 3, "jc_earned": 5, "depth_after": 10, "success": True}
        await svc.flavor(result, uid, TEST_GUILD_ID)

        assert result["llm_narrative"] == "The walls listen. They have always listened."
        assert result["llm_tone"] == "cryptic_folkloric"

    @pytest.mark.asyncio
    @pytest.mark.parametrize("side_effect", [TimeoutError(), RuntimeError("API down")])
    async def test_flavor_falls_back_silently_on_error(
        self, side_effect, dig_repo, player_repository,
    ):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo, player_repository, side_effect=side_effect,
        )
        result = {"advance": 3, "jc_earned": 5, "depth_after": 10, "success": True}
        await svc.flavor(result, uid, TEST_GUILD_ID)

        assert "llm_narrative" not in result
        assert result["advance"] == 3

    @pytest.mark.asyncio
    async def test_flavor_falls_back_on_wrong_tool(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(tool_name="other_tool", tool_args={}),
        )
        result = {"advance": 1, "jc_earned": 2, "depth_after": 5, "success": True}
        await svc.flavor(result, uid, TEST_GUILD_ID)
        assert "llm_narrative" not in result

    @pytest.mark.asyncio
    async def test_flavor_falls_back_on_numbers_in_narrative(
        self, dig_repo, player_repository,
    ):
        """Numbers in prose are forbidden — validator rejects + caller falls back."""
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(
                tool_name="narrate_dig",
                tool_args={
                    "narrative": "You earned 5 JC.",
                    "tone": "industrial_grim",
                },
            ),
        )
        result = {"advance": 1, "jc_earned": 5, "depth_after": 5, "success": True}
        await svc.flavor(result, uid, TEST_GUILD_ID)
        assert "llm_narrative" not in result

    @pytest.mark.asyncio
    async def test_flavor_bonus_credits_balance_when_within_cap(
        self, dig_repo, player_repository,
    ):
        uid = _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(
                tool_name="narrate_dig",
                tool_args={
                    "narrative": "Pickaxe sings against the seam.",
                    "tone": "industrial_grim",
                    "flavor_bonus_pct": 4.0,
                },
            ),
        )
        result = {"advance": 1, "jc_earned": 100, "depth_after": 5, "success": True}
        # Force the small-cap tier to keep the bonus deterministic at ±5%.
        import random as _r
        rng = _r.Random(0)  # 0 → roll() returns ~0.84, well above the 0.10 big-cap chance
        await svc.flavor(result, uid, TEST_GUILD_ID, rng=rng)

        # 4% of 100 = 4 JC delta added.
        assert result.get("llm_jc_delta") == 4
        assert player_repository.get_balance(uid, TEST_GUILD_ID) == 104

    @pytest.mark.asyncio
    async def test_flavor_bonus_skipped_when_jc_earned_zero(
        self, dig_repo, player_repository,
    ):
        uid = _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(
                tool_name="narrate_dig",
                tool_args={
                    "narrative": "Walls collapse around you.",
                    "tone": "cosmic_dread",
                    "flavor_bonus_pct": 5.0,
                },
            ),
        )
        result = {
            "advance": 0, "jc_earned": 0, "cave_in": True,
            "depth_after": 5, "success": True,
        }
        await svc.flavor(result, uid, TEST_GUILD_ID)
        assert "llm_jc_delta" not in result
        assert player_repository.get_balance(uid, TEST_GUILD_ID) == 100

    @pytest.mark.asyncio
    async def test_flavor_writes_dm_memory(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(
            dig_repo,
            player_repository,
            tool_result=_MockToolCallResult(
                tool_name="narrate_dig",
                tool_args={
                    "narrative": "First descent. The depth notes you.",
                    "tone": "cosmic_dread",
                    "memory_update": "Player crossed first threshold quietly. The dark noticed.",
                },
            ),
        )
        result = {"advance": 1, "jc_earned": 1, "depth_after": 5, "success": True}
        await svc.flavor(result, uid, TEST_GUILD_ID)

        memory = dig_repo.get_dm_memory(uid, TEST_GUILD_ID)
        assert "first threshold" in memory


# ---------------------------------------------------------------------------
# DigFlavorService — update_personality (kept from old service)
# ---------------------------------------------------------------------------


class TestDigFlavorServicePersonality:
    def _make_service(self, dig_repo, player_repository):
        return DigFlavorService(MagicMock(), dig_repo, player_repository, MagicMock())

    def test_update_personality_creates_new(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="safe")

        personality = dig_repo.get_personality(uid, TEST_GUILD_ID)
        assert personality is not None
        assert personality["choice_histogram"].get("safe") == 1

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

    def test_invalid_choice_ignored(self, dig_repo, player_repository):
        uid = _register_player(player_repository)
        dig_repo.create_tunnel(uid, TEST_GUILD_ID, "Test Tunnel")

        svc = self._make_service(dig_repo, player_repository)
        svc.update_personality(uid, TEST_GUILD_ID, "dig", choice="invalid_choice")

        histogram = dig_repo.get_personality(uid, TEST_GUILD_ID).get("choice_histogram", {})
        assert "invalid_choice" not in histogram


# ---------------------------------------------------------------------------
# Repository — DM memory + personality + social actions
# ---------------------------------------------------------------------------


class TestDigRepoDMMemory:
    def test_get_returns_empty_when_missing(self, dig_repo):
        assert dig_repo.get_dm_memory(99999, TEST_GUILD_ID) == ""

    def test_set_then_get_round_trip(self, dig_repo):
        dig_repo.set_dm_memory(99001, TEST_GUILD_ID, "Notable moment recorded.")
        assert dig_repo.get_dm_memory(99001, TEST_GUILD_ID) == "Notable moment recorded."

    def test_overwrite_replaces_text(self, dig_repo):
        dig_repo.set_dm_memory(99001, TEST_GUILD_ID, "First note.")
        dig_repo.set_dm_memory(99001, TEST_GUILD_ID, "Second note.")
        assert dig_repo.get_dm_memory(99001, TEST_GUILD_ID) == "Second note."

    def test_2kb_truncation_byte_safe(self, dig_repo):
        big = "y" * 5000
        dig_repo.set_dm_memory(99001, TEST_GUILD_ID, big)
        stored = dig_repo.get_dm_memory(99001, TEST_GUILD_ID)
        assert len(stored.encode("utf-8")) <= 2048

    def test_per_guild_isolation(self, dig_repo):
        dig_repo.set_dm_memory(99001, TEST_GUILD_ID, "guild A memory")
        dig_repo.set_dm_memory(99001, 999, "guild B memory")
        assert dig_repo.get_dm_memory(99001, TEST_GUILD_ID) == "guild A memory"
        assert dig_repo.get_dm_memory(99001, 999) == "guild B memory"


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


class TestDigRepoSocialActions:
    def test_empty_when_none_logged(self, dig_repo):
        assert dig_repo.get_recent_social_actions(10001, TEST_GUILD_ID) == []

    def test_returns_only_social_actions(self, dig_repo):
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID, actor_id=10001, target_id=10002,
            action_type="sabotage", depth_before=50, depth_after=45, jc_delta=-5,
        )
        dig_repo.log_action(
            guild_id=TEST_GUILD_ID, actor_id=10001,
            action_type="dig", depth_before=48, depth_after=50,
        )

        actions = dig_repo.get_recent_social_actions(10001, TEST_GUILD_ID)
        assert {a["action_type"] for a in actions} == {"sabotage"}


# ---------------------------------------------------------------------------
# DigService — dig_with_preconditions + apply_dig_outcome (still used)
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

    def test_unregistered_returns_terminal_failure(self, dig_service):
        terminal, preconditions = dig_service.dig_with_preconditions(99999, TEST_GUILD_ID)
        assert terminal is not None
        assert terminal["success"] is False


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
