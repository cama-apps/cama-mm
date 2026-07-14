"""Shop curses bias dig hazards and event outcomes while active."""

from __future__ import annotations

from repositories.curse_repository import CurseRepository
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_service import DigService


def _cursed_service(repo_db_path, monkeypatch):
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    curse_repo = CurseRepository(repo_db_path)
    player_repo.add(discord_id=801, discord_username="Cursed Digger", guild_id=18)
    dig_repo.create_tunnel(801, 18, "Bad Luck Shaft")
    dig_repo.update_tunnel(
        801,
        18,
        total_digs=1,
        depth=10,
        luminosity=100,
        last_dig_at=0,
    )
    curse_repo.cast_or_extend(18, 999, 801, 7)
    service = DigService(dig_repo, player_repo, curse_repo=curse_repo)
    monkeypatch.setattr(service, "_get_weather_effects", lambda guild_id, layer: {})
    return service


def test_shop_curse_weights_events_with_negative_branches_more_heavily(
    repo_db_path, monkeypatch,
):
    service = _cursed_service(repo_db_path, monkeypatch)
    safe_event = {
        "id": "safe_only",
        "name": "Safe Only",
        "description": "Safe.",
        "rarity": "common",
        "min_depth": 0,
        "safe_option": {
            "success": {"jc": 1},
        },
    }
    risky_event = {
        "id": "negative_branch",
        "name": "Negative Branch",
        "description": "Risky.",
        "rarity": "common",
        "min_depth": 0,
        "risky_option": {
            "success_chance": 0.5,
            "success": {"jc": 1},
            "failure": {"jc": -1},
        },
    }
    curse_only_event = {
        "id": "curse_only_branch",
        "name": "Curse Only Branch",
        "description": "Risky.",
        "rarity": "common",
        "risky_option": {
            "success_chance": 0.5,
            "success": {"jc": 1},
            "failure": {
                "jc": 0,
                "curse": {
                    "id": "test_curse",
                    "name": "Test Curse",
                    "duration_digs": 2,
                    "effect": {"advance_bonus": -1},
                },
            },
        },
    }
    monkeypatch.setattr(
        "services.dig_service.EVENT_POOL",
        [safe_event, risky_event, curse_only_event],
    )
    captured = {}

    def choose(events, *, weights, k):
        captured["events"] = events
        captured["weights"] = weights
        return [events[0]]

    monkeypatch.setattr("services.dig.events_mixin.random.choices", choose)

    service.roll_event(10, discord_id=801, guild_id=18)

    weights_by_id = dict(zip((event["id"] for event in captured["events"]), captured["weights"]))
    assert weights_by_id["safe_only"] == 70
    assert weights_by_id["negative_branch"] == 105
    assert weights_by_id["curse_only_branch"] == 105


def test_shop_curse_reduces_risky_event_success(
    repo_db_path, monkeypatch,
):
    service = _cursed_service(repo_db_path, monkeypatch)
    event = {
        "id": "curse_failure",
        "name": "Curse Failure",
        "rarity": "common",
        "risky_option": {
            "success_chance": 0.50,
            "success": {"advance": 0, "jc": 1, "description": "Won."},
            "failure": {"advance": 0, "jc": -1, "description": "Lost."},
        },
    }
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [event])
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.47)
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)

    result = service.resolve_event(801, 18, "curse_failure", "risky")

    assert result["succeeded"] is False


def test_shop_curse_adds_cave_in_risk_to_dig_preconditions(
    repo_db_path, monkeypatch,
):
    service = _cursed_service(repo_db_path, monkeypatch)

    terminal, preconditions = service._compute_preconditions(801, 18)

    assert terminal is None
    assert preconditions["shop_curse_stacks"] == 1
    assert preconditions["shop_curse_cave_in_bonus"] == 0.02
    assert preconditions["cave_in_chance"] >= 0.03
