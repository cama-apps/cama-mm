"""Catalog and reward behavior for the event-only Dig rings."""

from __future__ import annotations

import pytest

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_data.aliases import EVENT_POOL
from services.dig_data.items import UNIQUE_GEAR
from services.dig_service import DigService

PLAYER_ID = 811
GUILD_ID = 55

RING_EVENTS = {
    "surveyors_cairn": ("surveyors_loop", 25),
    "ruinwager_vault": ("ruinwager_signet", 75),
    "red_thread_shrine": ("red_thread_band", 125),
}


@pytest.fixture
def event_service(repo_db_path):
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(
        discord_id=PLAYER_ID,
        discord_username="EventRinger",
        guild_id=GUILD_ID,
    )
    player_repo.update_balance(PLAYER_ID, GUILD_ID, 10_000)
    dig_repo.create_tunnel(PLAYER_ID, GUILD_ID, "Ring Road")
    dig_repo.update_tunnel(
        PLAYER_ID,
        GUILD_ID,
        depth=10,
        max_depth=200,
        luminosity=100,
    )
    return DigService(dig_repo, player_repo)


def _pin_success(monkeypatch) -> None:
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda _a, _b: 1.0)
    monkeypatch.setattr("services.dig.events_mixin.random.randint", lambda _a, _b: 0)
    monkeypatch.setattr(
        "services.dig.events_mixin.random.choice",
        lambda candidates: candidates[0],
    )


def test_ring_encounters_are_depth_staggered_genuine_choices():
    by_id = {event["id"]: event for event in EVENT_POOL}

    for event_id, (item_id, min_depth) in RING_EVENTS.items():
        event = by_id[event_id]
        safe = event["safe_option"]
        risky = event["risky_option"]

        assert event["min_depth"] == min_depth
        assert safe["success_chance"] == 1.0
        assert safe["success"]["advance"] > 0
        assert risky["success_chance"] < 1.0
        assert risky["success"]["gear_reward_pool"] == [item_id]
        assert (
            risky["failure"]["advance"] < 0
            or risky["failure"]["jc"] < 0
            or risky["failure"]["cave_in"]
        )


@pytest.mark.parametrize(("event_id", "item_id"), [
    ("surveyors_cairn", "surveyors_loop"),
    ("ruinwager_vault", "ruinwager_signet"),
    ("red_thread_shrine", "red_thread_band"),
])
def test_ring_encounter_risky_success_grants_its_unique_ring(
    event_service,
    monkeypatch,
    event_id,
    item_id,
):
    _pin_success(monkeypatch)

    result = event_service.resolve_event(
        PLAYER_ID,
        GUILD_ID,
        event_id,
        "risky",
    )

    definition = UNIQUE_GEAR[item_id]
    row = event_service.dig_repo.get_gear_by_id(result["gear_drop"]["gear_id"])
    assert result["success"] is True
    assert result["succeeded"] is True
    assert result["gear_drop"]["item_id"] == item_id
    assert result["gear_drop"]["slot"] == "ring"
    assert row["item_id"] == item_id
    assert row["slot"] == "ring"
    assert row["durability"] == definition.max_durability


def test_repeat_ring_encounter_keeps_outcome_without_duplicate_gear(
    event_service,
    monkeypatch,
):
    definition = UNIQUE_GEAR["surveyors_loop"]
    event_service.dig_repo.add_gear(
        PLAYER_ID,
        GUILD_ID,
        "ring",
        definition.reference_tier,
        source="event:surveyors_cairn",
        durability=definition.max_durability,
        item_id=definition.item_id,
    )
    _pin_success(monkeypatch)

    result = event_service.resolve_event(
        PLAYER_ID,
        GUILD_ID,
        "surveyors_cairn",
        "risky",
    )

    owned = [
        row
        for row in event_service.dig_repo.get_gear(PLAYER_ID, GUILD_ID)
        if row.get("item_id") == "surveyors_loop"
    ]
    assert result["success"] is True
    assert result["succeeded"] is True
    assert result["depth_delta"] > 0
    assert result["gear_drop"] is None
    assert "already own this gear" in result["message"].lower()
    assert "leave the duplicate behind" in result["message"].lower()
    assert len(owned) == 1
