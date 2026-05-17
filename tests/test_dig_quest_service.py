"""Tests for DigQuestService: eligibility, progression, finale dispatch."""

from __future__ import annotations

import random
import time

import pytest

from repositories.bet_repository import BetRepository
from repositories.dig_guild_modifier_repository import DigGuildModifierRepository
from repositories.dig_quest_repository import DigQuestRepository
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_constants import (
    EventChoice,
    EventOutcome,
    QuestDef,
    QuestStarterPrereq,
    RandomEvent,
)
from services.dig_quest_service import DigQuestService
from tests.conftest import TEST_GUILD_ID

# ── Fake-quest builder helpers ───────────────────────────────────────────────


def _make_event(eid: str, *, quest_id: str, step: int, rarity: str = "common") -> RandomEvent:
    return RandomEvent(
        id=eid, name=eid, description=("flavor",),
        min_depth=None, max_depth=None,
        safe_option=EventChoice("safe", EventOutcome("ok", 0, 1, False), None, 1.0),
        risky_option=EventChoice(
            "risky", EventOutcome("good", 0, 5, False),
            EventOutcome("bad", -2, 0, False), 0.5,
        ),
        desperate_option=EventChoice(
            "desperate", EventOutcome("great", 0, 10, False),
            EventOutcome("terrible", -5, 0, False), 0.25,
        ),
        rarity=rarity,
        quest_id=quest_id, quest_step=step,
    )


def _make_5_stage_quest(
    quest_id: str = "qtest",
    *,
    starter: QuestStarterPrereq | None = None,
    finale_kind: str = "relic_grant",
    finale_payload: dict | None = None,
) -> tuple[QuestDef, list[RandomEvent]]:
    events = [
        _make_event(f"{quest_id}_s{i}", quest_id=quest_id, step=i)
        for i in range(1, 6)
    ]
    quest = QuestDef(
        quest_id=quest_id,
        name=quest_id,
        starter_prereq=starter or QuestStarterPrereq(),
        step_event_ids=tuple(e.id for e in events),
        finale_kind=finale_kind,
        finale_payload=finale_payload or {
            "relic_base": "Test Relic",
            "relic_suffix": "Trials",
            "stat_pool": ("hp_plus_1", "hit_plus_002", "boss_hit_minus"),
            "roll_count": 2,
        },
    )
    return quest, events


# ── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture
def quest_repo(repo_db_path):
    return DigQuestRepository(repo_db_path)


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def bet_repo(repo_db_path):
    return BetRepository(repo_db_path)


@pytest.fixture
def guild_modifier_repo(repo_db_path):
    return DigGuildModifierRepository(repo_db_path)


@pytest.fixture
def player_repository(repo_db_path):
    return PlayerRepository(repo_db_path)


def _make_tunnel(dig_repo, discord_id, guild_id, *, depth=0, prestige_level=0):
    dig_repo.create_tunnel(discord_id, guild_id, tunnel_name="Test")
    dig_repo.update_tunnel(
        discord_id, guild_id, depth=depth, prestige_level=prestige_level,
    )


def _seed_player(player_repository, discord_id, guild_id, *, balance=1000):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"player{discord_id}",
        guild_id=guild_id,
    )
    player_repository.update_balance(discord_id, guild_id, balance)


# ── Eligibility ──────────────────────────────────────────────────────────────


def test_no_quests_means_no_eligible_events(quest_repo, dig_repo, bet_repo, guild_modifier_repo):
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=())
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == set()


def test_starter_eligible_with_default_prereqs(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=0)
    eligible = svc.eligible_quest_event_ids(1, TEST_GUILD_ID)
    assert eligible == {"qtest_s1"}


def test_depth_gate_filters_starter(quest_repo, dig_repo, bet_repo, guild_modifier_repo):
    quest, _ = _make_5_stage_quest(starter=QuestStarterPrereq(min_depth=25))
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))

    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == set()

    dig_repo.update_tunnel(1, TEST_GUILD_ID, depth=30)
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == {"qtest_s1"}


def test_prestige_gate_filters_starter(quest_repo, dig_repo, bet_repo, guild_modifier_repo):
    quest, _ = _make_5_stage_quest(starter=QuestStarterPrereq(min_prestige=2))
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))

    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, prestige_level=1)
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == set()

    dig_repo.update_tunnel(1, TEST_GUILD_ID, prestige_level=2)
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == {"qtest_s1"}


def test_system_predicate_bet_within_7d_negative(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo, player_repository,
):
    quest, _ = _make_5_stage_quest(
        starter=QuestStarterPrereq(system_predicate="bet_within_7d"),
    )
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _seed_player(player_repository, 1, TEST_GUILD_ID)
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)

    # No bet history → starter ineligible
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == set()


def test_system_predicate_bet_within_7d_positive(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo, player_repository,
):
    quest, _ = _make_5_stage_quest(
        starter=QuestStarterPrereq(system_predicate="bet_within_7d"),
    )
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _seed_player(player_repository, 1, TEST_GUILD_ID)
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)

    # Insert a bet placed within the last 7 days
    now = int(time.time())
    bet_repo.create_bet(TEST_GUILD_ID, 1, "radiant", 10, now)

    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == {"qtest_s1"}


def test_system_predicate_bet_outside_window_filters_out(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo, player_repository,
):
    quest, _ = _make_5_stage_quest(
        starter=QuestStarterPrereq(system_predicate="bet_within_7d"),
    )
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _seed_player(player_repository, 1, TEST_GUILD_ID)
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)

    # Bet placed 8 days ago — outside the 7-day window
    old = int(time.time()) - (8 * 86400)
    bet_repo.create_bet(TEST_GUILD_ID, 1, "radiant", 10, old)

    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == set()


def test_active_quest_only_exposes_current_stage(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    quest_repo.set_active(1, TEST_GUILD_ID, "qtest", 3)
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == {"qtest_s3"}


def test_completed_quest_starter_not_eligible(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    quest_repo.complete_quest(1, TEST_GUILD_ID, "qtest")
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == set()


def test_one_at_a_time_concurrency_blocks_other_starters(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    qA, _ = _make_5_stage_quest(quest_id="qA")
    qB, _ = _make_5_stage_quest(quest_id="qB")
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(qA, qB))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    quest_repo.set_active(1, TEST_GUILD_ID, "qA", 2)
    # Only qA stage 2; qB starter should NOT be eligible
    eligible = svc.eligible_quest_event_ids(1, TEST_GUILD_ID)
    assert eligible == {"qA_s2"}


# ── Progression ──────────────────────────────────────────────────────────────


def test_advance_on_desperate_success_starts_quest_at_stage_2(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)

    result = svc.advance_on_desperate_success(1, TEST_GUILD_ID, "qtest_s1")
    assert result is None  # not finale yet
    state = quest_repo.get_state(1, TEST_GUILD_ID)
    assert state.active_quest_id == "qtest"
    assert state.active_quest_step == 2


def test_advance_ignores_event_mismatched_to_active_stage(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    quest_repo.set_active(1, TEST_GUILD_ID, "qtest", 2)
    # Player on stage 2 reports a stage-4 desperate success — defense in depth
    result = svc.advance_on_desperate_success(1, TEST_GUILD_ID, "qtest_s4")
    assert result is None
    state = quest_repo.get_state(1, TEST_GUILD_ID)
    assert state.active_quest_step == 2


def test_advance_through_to_finale_dispatches_relic(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    # Deterministic relic roll picks
    random.seed(1234)

    # Walk through all 5 stages
    for step in range(1, 5):
        result = svc.advance_on_desperate_success(1, TEST_GUILD_ID, f"qtest_s{step}")
        assert result is None
    result = svc.advance_on_desperate_success(1, TEST_GUILD_ID, "qtest_s5")
    assert result is not None
    assert result["quest_id"] == "qtest"
    assert result["finale_kind"] == "relic_grant"
    assert result["relic_name"] == "Test Relic of Trials"
    assert result["artifact_id"].startswith("pinnacle:Test Relic:Trials:")
    parts = result["artifact_id"].split(":")
    # pinnacle:Test Relic:Trials:<stat1>:<stat2>
    assert len(parts) == 5
    assert len(set(parts[3:])) == 2  # two distinct stats

    # State: quest marked complete, no active
    state = quest_repo.get_state(1, TEST_GUILD_ID)
    assert state.active_quest_id is None
    assert "qtest" in state.completed_quests

    # The relic landed in dig_artifacts as a relic
    relics = dig_repo.get_artifacts(1, TEST_GUILD_ID)
    assert any(r["artifact_id"] == result["artifact_id"] for r in relics)


def test_finale_jc_plus_modifier_grants_jc_and_sets_modifier(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo, player_repository,
):
    quest, _ = _make_5_stage_quest(
        quest_id="agh",
        finale_kind="jc_plus_guild_modifier",
        finale_payload={
            "personal_jc": 75,
            "modifier_id": "reagent_spill",
            "duration_seconds": 1800,
            "modifier_payload": {"jc_event_bonus_pct": 25},
        },
    )
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _seed_player(player_repository, 1, TEST_GUILD_ID, balance=100)
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)

    # Walk all 5 stages
    for step in range(1, 5):
        svc.advance_on_desperate_success(1, TEST_GUILD_ID, f"agh_s{step}")
    result = svc.advance_on_desperate_success(1, TEST_GUILD_ID, "agh_s5")
    assert result is not None
    assert result["finale_kind"] == "jc_plus_guild_modifier"
    assert result["personal_jc"] == 75
    assert result["modifier_id"] == "reagent_spill"

    # Balance credited
    assert player_repository.get_balance(1, TEST_GUILD_ID) == 100 + 75

    # Guild modifier active
    assert guild_modifier_repo.is_active(TEST_GUILD_ID, "reagent_spill")


def test_completed_quest_not_re_eligible_after_finale(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10)
    for step in range(1, 6):
        svc.advance_on_desperate_success(1, TEST_GUILD_ID, f"qtest_s{step}")
    assert svc.eligible_quest_event_ids(1, TEST_GUILD_ID) == set()


def test_progression_persists_across_prestige_reset(
    quest_repo, dig_repo, bet_repo, guild_modifier_repo,
):
    """Active quest state survives a prestige_level bump back to 0."""
    DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=())
    _make_tunnel(dig_repo, 1, TEST_GUILD_ID, depth=10, prestige_level=1)
    quest_repo.set_active(1, TEST_GUILD_ID, "qtest", 3)
    # Simulate prestige reset: depth -> 0, prestige_level += 1 (or whatever).
    # Quest state row should not be touched.
    dig_repo.update_tunnel(1, TEST_GUILD_ID, depth=0, prestige_level=2)
    state = quest_repo.get_state(1, TEST_GUILD_ID)
    assert state.active_quest_id == "qtest"
    assert state.active_quest_step == 3


def test_quest_for_event_lookup(quest_repo, dig_repo, bet_repo, guild_modifier_repo):
    quest, _ = _make_5_stage_quest()
    svc = DigQuestService(quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,))
    assert svc.quest_for_event("qtest_s3") == ("qtest", 3)
    assert svc.quest_for_event("not_a_quest_event") is None
