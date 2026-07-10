"""Integration tests for the quest hooks in DigService.

Covers:
- roll_event filters quest events by player eligibility
- roll_event excludes quest events during boss-fight digs
- resolve_event advances the quest on desperate-success
- resolve_event does NOT advance on safe / risky / desperate-failure
- The final-stage desperate-success surfaces a finale on the result
"""

from __future__ import annotations

import random

import pytest

from repositories.bet_repository import BetRepository
from repositories.dig_guild_modifier_repository import DigGuildModifierRepository
from repositories.dig_quest_repository import DigQuestRepository
from repositories.dig_repository import DigRepository
from services import dig_constants
from services.dig_constants import (
    EventChoice,
    EventOutcome,
    QuestDef,
    QuestStarterPrereq,
    RandomEvent,
)
from services.dig_quest_service import DigQuestService
from services.dig_service import DigService
from tests.conftest import TEST_GUILD_ID

# ── Fake quest fixtures ──────────────────────────────────────────────────────


def _make_event_dict(
    eid: str,
    *,
    quest_id: str | None = None,
    quest_step: int | None = None,
    rarity: str = "common",
) -> dict:
    """Build an EVENT_POOL-shaped dict for a fake quest event with all 3 options."""
    safe = {"label": "safe", "success": {
        "description": "ok", "advance_delta": 0, "jc_delta": 1, "cave_in": False,
    }, "failure": None, "success_chance": 1.0}
    risky = {"label": "risky", "success": {
        "description": "good", "advance_delta": 0, "jc_delta": 5, "cave_in": False,
    }, "failure": {
        "description": "bad", "advance_delta": -2, "jc_delta": 0, "cave_in": False,
    }, "success_chance": 0.5}
    desperate = {"label": "desperate", "success": {
        "description": "great", "advance_delta": 0, "jc_delta": 10, "cave_in": False,
    }, "failure": {
        "description": "terrible", "advance_delta": -5, "jc_delta": 0, "cave_in": False,
    }, "success_chance": 0.25}
    return {
        "id": eid, "name": eid, "description": ("flavor",),
        "min_depth": None, "max_depth": None,
        "safe_option": safe, "risky_option": risky, "desperate_option": desperate,
        "complexity": "choice", "layer": None, "rarity": rarity,
        "requires_dark": False, "social": False, "ascii_art": None,
        "buff_on_success": None, "boon_options": None,
        "min_prestige": 0, "next_event_id": None, "chain_only": False,
        "splash": None, "guild_modifier_on_success": None,
        "quest_id": quest_id, "quest_step": quest_step,
    }


def _make_test_quest(
    quest_id="qtest", *, starter=None, finale_kind="relic_grant",
    finale_payload=None,
):
    """Build a 5-stage QuestDef plus matching RandomEvent objects for the
    service-level validator and EVENT_POOL dicts for the service-level path."""
    events = [
        RandomEvent(
            id=f"{quest_id}_s{i}", name=f"{quest_id}_s{i}", description=("flavor",),
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
            rarity="common", quest_id=quest_id, quest_step=i,
        )
        for i in range(1, 6)
    ]
    quest = QuestDef(
        quest_id=quest_id,
        name=quest_id,
        starter_prereq=starter or QuestStarterPrereq(),
        step_event_ids=tuple(e.id for e in events),
        finale_kind=finale_kind,
        finale_payload=finale_payload if finale_payload is not None else {
            "relic_base": "Test Cloak",
            "relic_suffix": "Trials",
            "stat_pool": ("hp_plus_1", "hit_plus_002", "boss_hit_minus"),
            "roll_count": 2,
        },
    )
    return quest, events


# ── Shared fixtures ──────────────────────────────────────────────────────────


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
def quest_repo(repo_db_path):
    return DigQuestRepository(repo_db_path)




def _register_player(player_repository, discord_id=10001, guild_id=TEST_GUILD_ID, balance=100):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
    )
    player_repository.update_balance(discord_id, guild_id, balance)


def _make_dig_service(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
    *, quests=(), monkeypatch=None,
):
    quest_svc = DigQuestService(
        quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=quests,
    )
    svc = DigService(
        dig_repo, player_repository,
        dig_guild_modifier_repo=guild_modifier_repo,
        quest_service=quest_svc,
    )
    if monkeypatch is not None:
        monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc, quest_svc


def _patch_event_pool(monkeypatch, extra_events: list[dict]):
    """Append extra quest events to the EVENT_POOL the service module reads."""
    from services import dig_service as ds_module
    patched = list(ds_module.EVENT_POOL) + extra_events
    monkeypatch.setattr(ds_module, "EVENT_POOL", patched)
    # Also patch on dig_constants so paths that read from there see the same.
    monkeypatch.setattr(dig_constants, "EVENT_POOL", patched)


# ── roll_event filter ────────────────────────────────────────────────────────


def test_roll_event_excludes_quest_events_without_player_context(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    """When no discord_id is supplied, all quest events are filtered out."""
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    _patch_event_pool(monkeypatch, [_make_event_dict(
        "qtest_s1", quest_id="qtest", quest_step=1,
    )])
    # No player context -> quest events filtered
    for _ in range(200):
        random.seed(_)
        result = svc.roll_event(depth=10)
        if result is not None:
            assert not result["id"].startswith("qtest")


def test_roll_event_includes_quest_starter_when_eligible(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    """With a quest eligible to start, its stage-1 event can appear in the roll."""
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    # Patch in JUST our quest event — so the only "common" event in the pool
    # at the player's depth is the quest starter; the roll must return it.
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [
        _make_event_dict("qtest_s1", quest_id="qtest", quest_step=1),
    ])
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    random.seed(0)
    result = svc.roll_event(
        depth=10, discord_id=10001, guild_id=TEST_GUILD_ID,
    )
    assert result is not None
    assert result["id"] == "qtest_s1"


def test_roll_event_excludes_quest_event_when_player_on_different_step(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    """Player is on stage 3; stage 1 of the same quest must NOT appear."""
    quest, _ = _make_test_quest()
    svc, qs = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [
        _make_event_dict("qtest_s1", quest_id="qtest", quest_step=1),
    ])
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    quest_repo.set_active(10001, TEST_GUILD_ID, "qtest", 3)
    for seed in range(50):
        random.seed(seed)
        result = svc.roll_event(
            depth=10, discord_id=10001, guild_id=TEST_GUILD_ID,
        )
        assert result is None or result["id"] != "qtest_s1"


def test_roll_event_excludes_quest_event_during_boss_combat(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    """In a boss-fight dig, quest events are skipped even if otherwise eligible."""
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [
        _make_event_dict("qtest_s1", quest_id="qtest", quest_step=1),
    ])
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    for seed in range(50):
        random.seed(seed)
        result = svc.roll_event(
            depth=10, discord_id=10001, guild_id=TEST_GUILD_ID, in_boss=True,
        )
        assert result is None  # only event in pool is quest -> filtered -> empty pool


# ── resolve_event advancement ────────────────────────────────────────────────


def _patch_pool_with_event(monkeypatch, event_id: str, quest_id: str, step: int):
    """Replace EVENT_POOL with a single quest event for resolve_event tests."""
    from services import dig_service as ds_module
    patched = [_make_event_dict(event_id, quest_id=quest_id, quest_step=step)]
    monkeypatch.setattr(ds_module, "EVENT_POOL", patched)
    monkeypatch.setattr(dig_constants, "EVENT_POOL", patched)


def test_resolve_event_advances_on_desperate_success(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    _patch_pool_with_event(monkeypatch, "qtest_s1", "qtest", 1)
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    # Force the desperate success roll
    monkeypatch.setattr(random, "random", lambda: 0.0)
    result = svc.resolve_event(10001, TEST_GUILD_ID, "qtest_s1", "desperate")
    assert result["success"]
    assert result["succeeded"]
    assert result["quest_finale"] is None  # only stage 1 — not finale yet
    state = quest_repo.get_state(10001, TEST_GUILD_ID)
    assert state.active_quest_id == "qtest"
    assert state.active_quest_step == 2


def test_resolve_event_no_advance_on_safe(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    _patch_pool_with_event(monkeypatch, "qtest_s1", "qtest", 1)
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    result = svc.resolve_event(10001, TEST_GUILD_ID, "qtest_s1", "safe")
    assert result["success"]
    assert result["quest_finale"] is None
    state = quest_repo.get_state(10001, TEST_GUILD_ID)
    assert state.active_quest_id is None


def test_resolve_event_no_advance_on_risky_success(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    _patch_pool_with_event(monkeypatch, "qtest_s1", "qtest", 1)
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    monkeypatch.setattr(random, "random", lambda: 0.0)  # risky succeeds
    result = svc.resolve_event(10001, TEST_GUILD_ID, "qtest_s1", "risky")
    assert result["succeeded"]
    state = quest_repo.get_state(10001, TEST_GUILD_ID)
    assert state.active_quest_id is None


def test_resolve_event_no_advance_on_desperate_failure(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    _patch_pool_with_event(monkeypatch, "qtest_s1", "qtest", 1)
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    monkeypatch.setattr(random, "random", lambda: 0.99)  # desperate fails
    result = svc.resolve_event(10001, TEST_GUILD_ID, "qtest_s1", "desperate")
    assert not result["succeeded"]
    state = quest_repo.get_state(10001, TEST_GUILD_ID)
    assert state.active_quest_id is None


def test_resolve_event_final_stage_returns_finale(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    _patch_pool_with_event(monkeypatch, "qtest_s5", "qtest", 5)
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    # Pre-place player on stage 5 of the quest
    quest_repo.set_active(10001, TEST_GUILD_ID, "qtest", 5)
    monkeypatch.setattr(random, "random", lambda: 0.0)  # desperate succeeds
    # Stub random.sample (used in relic stat roll) for determinism
    monkeypatch.setattr(
        random, "sample",
        lambda population, k: list(population)[:k],
    )
    result = svc.resolve_event(10001, TEST_GUILD_ID, "qtest_s5", "desperate")
    assert result["success"]
    finale = result["quest_finale"]
    assert finale is not None
    assert finale["quest_id"] == "qtest"
    assert finale["finale_kind"] == "relic_grant"
    assert finale["relic_name"] == "Test Cloak of Trials"
    # State cleared, quest in completed list
    state = quest_repo.get_state(10001, TEST_GUILD_ID)
    assert state.active_quest_id is None
    assert "qtest" in state.completed_quests


# ── Regression tests for fixes ───────────────────────────────────────────────


def test_chain_event_filter_excludes_quest_events(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    """The P7+ chained-event selector must not surface quest events."""
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    # Make the only event in the pool a quest event. _chain_event must
    # return None rather than return that event.
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [
        _make_event_dict("qtest_s1", quest_id="qtest", quest_step=1),
    ])
    # Force the chain probability roll to succeed
    monkeypatch.setattr(random, "random", lambda: 0.0)
    chained = svc._chain_event(
        depth=50, prestige_level=7,
        trigger_rarity="common", luminosity=100,
        trigger_event_id="some_other_event",
    )
    assert chained is None, "quest event leaked into _chain_event pool"


def test_available_events_filter_excludes_quest_events(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    """The available_events list (LLM context) must not contain quest events."""
    quest, _ = _make_test_quest()
    svc, _ = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [
        _make_event_dict("qtest_s1", quest_id="qtest", quest_step=1),
        _make_event_dict("plain_event"),
    ])
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    # Park the player past first-dig cooldown so _compute_preconditions
    # returns a precondition dict (not an early-exit terminal result).
    import time as _t
    dig_repo.update_tunnel(
        10001, TEST_GUILD_ID,
        depth=10, last_dig_at=int(_t.time()) - 7200, total_digs=5,
    )
    terminal, pre = svc._compute_preconditions(10001, TEST_GUILD_ID, paid=False)
    assert pre is not None, f"expected preconditions, got terminal={terminal!r}"
    ids = {e["id"] for e in pre.get("available_events", [])}
    assert "qtest_s1" not in ids
    assert "plain_event" in ids


def test_finale_jc_applies_tax_fn(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
):
    """The Aghanim-style finale runs gross JC through tax_fn before crediting."""
    quest, _ = _make_test_quest(
        quest_id="agh",
        finale_kind="jc_plus_guild_modifier",
        finale_payload={
            "personal_jc": 100,
            "modifier_id": "test_window",
            "duration_seconds": 1800,
            "modifier_payload": {},
        },
    )
    from services.dig_quest_service import DigQuestService
    qs = DigQuestService(
        quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,),
        tax_fn=lambda did, gid, jc: jc - 5,  # flat 5 JC tax stub
    )
    _register_player(player_repository, balance=100)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    quest_repo.set_active(10001, TEST_GUILD_ID, "agh", 5)
    result = qs.advance_on_desperate_success(10001, TEST_GUILD_ID, "agh_s5")
    assert result is not None
    assert result["personal_jc"] == 95  # 100 gross - 5 tax
    assert result["personal_jc_gross"] == 100
    # Balance reflects post-tax credit
    assert player_repository.get_balance(10001, TEST_GUILD_ID) == 100 + 95


def test_finale_completes_quest_before_dispatch(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
):
    """A dispatch failure leaves the quest completed (recoverable) — not
    pinned on stage 5 (re-fireable)."""
    quest, _ = _make_test_quest(finale_kind="relic_grant")
    from services.dig_quest_service import DigQuestService
    qs = DigQuestService(
        quest_repo, dig_repo, bet_repo, guild_modifier_repo, quests=(quest,),
    )
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")
    quest_repo.set_active(10001, TEST_GUILD_ID, "qtest", 5)

    # Stub add_artifact to raise — simulates a DB error during reward grant.
    def _boom(*a, **kw):
        raise RuntimeError("simulated relic grant failure")
    original = dig_repo.add_artifact
    dig_repo.add_artifact = _boom
    try:
        with pytest.raises(RuntimeError):
            qs.advance_on_desperate_success(10001, TEST_GUILD_ID, "qtest_s5")
    finally:
        dig_repo.add_artifact = original

    # Quest is marked complete — ordering puts complete_quest first so a
    # dispatch failure can't double-grant on the next stage-5 desperate.
    state = quest_repo.get_state(10001, TEST_GUILD_ID)
    assert state.active_quest_id is None
    assert "qtest" in state.completed_quests


def test_roll_event_passes_tunnel_through_to_quest_filter(
    dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo, monkeypatch,
):
    """roll_event must thread the in-scope tunnel through to the quest
    eligibility filter, avoiding a second DB fetch."""
    quest, _ = _make_test_quest()
    svc, qs = _make_dig_service(
        dig_repo, player_repository, quest_repo, bet_repo, guild_modifier_repo,
        quests=(quest,), monkeypatch=monkeypatch,
    )
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [
        _make_event_dict("qtest_s1", quest_id="qtest", quest_step=1),
    ])
    _register_player(player_repository)
    dig_repo.create_tunnel(10001, TEST_GUILD_ID, tunnel_name="t")

    calls: list[tuple] = []
    real_get_tunnel = dig_repo.get_tunnel
    def _counted(did, gid):
        calls.append((did, gid))
        return real_get_tunnel(did, gid)
    monkeypatch.setattr(dig_repo, "get_tunnel", _counted)

    # Pass tunnel through — quest filter must use it instead of fetching again.
    tunnel = real_get_tunnel(10001, TEST_GUILD_ID)
    random.seed(0)
    svc.roll_event(
        depth=10, discord_id=10001, guild_id=TEST_GUILD_ID, tunnel=tunnel,
    )
    assert calls == [], "roll_event re-fetched tunnel despite the caller passing it"
