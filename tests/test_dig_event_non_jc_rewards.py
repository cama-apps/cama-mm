"""Non-JC event rewards serialize and settle in the event transaction."""

from unittest.mock import MagicMock

import services.dig_service as dig_service_module
from repositories.dig_repository import DigRepository
from services.dig_constants import MAX_INVENTORY_SIZE
from services.dig_data.aliases import (
    EVENT_POOL,
    _validate_event_reward_pools,
)
from services.dig_data.artifacts import ARTIFACT_BY_ID
from services.dig_data.event_types import EventOutcome, _outcome_to_dict
from services.dig_service import DigService


def test_event_outcome_serializes_all_non_jc_reward_pools():
    outcome = EventOutcome(
        "Found",
        0,
        0,
        False,
        gear_reward_pool=("glassbreaker_pick",),
        consumable_reward_pool=("rescue_line",),
        artifact_reward_pool=("miner_s_lullaby",),
    )

    payload = _outcome_to_dict(outcome)

    assert payload["gear_reward_pool"] == ["glassbreaker_pick"]
    assert payload["consumable_reward_pool"] == ["rescue_line"]
    assert payload["artifact_reward_pool"] == ["miner_s_lullaby"]


NEW_NON_JC_EVENT_IDS = {
    "abandoned_forge",
    "salted_threshold",
    "frayed_lifeline",
    "quartermaster_s_niche",
    "collapsed_armory_cache",
    "relic_bearing_strata",
    "prospector_s_last_pack",
    "song_below_stone",
    "first_descent_cartography",
}


def test_new_encounter_catalog_has_nine_non_jc_events():
    events = {event["id"]: event for event in EVENT_POOL}

    assert set(events) >= NEW_NON_JC_EVENT_IDS
    for event_id in NEW_NON_JC_EVENT_IDS:
        event = events[event_id]
        for option_key in ("safe_option", "risky_option", "desperate_option"):
            option = event.get(option_key)
            if not option:
                continue
            for outcome_key in ("success", "failure"):
                outcome = option.get(outcome_key)
                if outcome:
                    assert outcome.get("jc", 0) <= 0


def test_event_reward_pool_validation_rejects_unknown_ids():
    event = _reward_event(consumable_reward_pool=["missing_supply"])

    try:
        _validate_event_reward_pools([event])
    except ValueError as exc:
        assert "missing_supply" in str(exc)
    else:
        raise AssertionError("invalid event reward pool was accepted")


def test_lore_curios_are_unique_and_statless():
    for artifact_id in ("miner_s_lullaby", "map_of_the_first_descent"):
        curio = ARTIFACT_BY_ID[artifact_id]
        assert curio.rarity == "Legendary"
        assert curio.is_relic is False
        assert curio.effect is None


def test_atomic_event_can_grant_statless_artifact(
    repo_db_path,
    player_repository,
    guild_id,
):
    player_id = 92001
    player_repository.add(
        discord_id=player_id,
        discord_username="artifact_event_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = DigRepository(repo_db_path)
    repo.create_tunnel(player_id, guild_id, "Artifact Tunnel")

    artifact_id = repo.atomic_tunnel_balance_update(
        player_id,
        guild_id,
        add_artifact_id="miner_s_lullaby",
        add_artifact_is_relic=False,
        log_action_type="event",
        log_detail={"event_id": "lullaby_probe"},
    )

    assert isinstance(artifact_id, int)
    assert repo.has_artifact(
        player_id,
        guild_id,
        "miner_s_lullaby",
    )


def _event_service(repo_db_path, player_repository, guild_id):
    player_id = 92002
    player_repository.add(
        discord_id=player_id,
        discord_username="non_jc_event_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = DigRepository(repo_db_path)
    repo.create_tunnel(player_id, guild_id, "Reward Tunnel")
    repo.update_tunnel(player_id, guild_id, depth=100, luminosity=100)
    return DigService(repo, player_repository), player_id


def _reward_event(**outcome):
    return {
        "id": "non_jc_reward_probe",
        "name": "Reward probe",
        "description": "Reward probe",
        "rarity": "rare",
        "safe_option": {
            "label": "Take it",
            "success_chance": 1.0,
            "success": {
                "description": "Found",
                "advance": 0,
                "jc": 0,
                "cave_in": False,
                **outcome,
            },
            "failure": None,
        },
    }


def test_resolve_event_grants_consumable_without_minting_jc(
    repo_db_path,
    player_repository,
    guild_id,
    monkeypatch,
):
    service, player_id = _event_service(
        repo_db_path,
        player_repository,
        guild_id,
    )
    monkeypatch.setattr(
        dig_service_module,
        "EVENT_POOL",
        [_reward_event(consumable_reward_pool=["rescue_line"])],
    )
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)

    result = service.resolve_event(
        player_id,
        guild_id,
        "non_jc_reward_probe",
        "safe",
    )

    assert result["jc_delta"] == 0
    assert result["consumable_drop"]["item_id"] == "rescue_line"
    assert service.dig_repo.get_inventory(player_id, guild_id)[0][
        "item_type"
    ] == "rescue_line"


def test_owned_unique_reward_falls_back_to_unowned_artifact(
    repo_db_path,
    player_repository,
    guild_id,
    monkeypatch,
):
    service, player_id = _event_service(
        repo_db_path,
        player_repository,
        guild_id,
    )
    service.dig_repo.add_gear(
        player_id,
        guild_id,
        "weapon",
        3,
        item_id="glassbreaker_pick",
    )
    monkeypatch.setattr(
        dig_service_module,
        "EVENT_POOL",
        [_reward_event(
            gear_reward_pool=["glassbreaker_pick"],
            artifact_reward_pool=["miner_s_lullaby"],
        )],
    )
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)

    result = service.resolve_event(
        player_id,
        guild_id,
        "non_jc_reward_probe",
        "safe",
    )

    assert result["gear_drop"] is None
    assert result["artifact_drop"]["artifact_id"] == "miner_s_lullaby"


def test_full_inventory_falls_back_from_consumable_to_artifact(
    repo_db_path,
    player_repository,
    guild_id,
    monkeypatch,
):
    service, player_id = _event_service(
        repo_db_path,
        player_repository,
        guild_id,
    )
    for _ in range(MAX_INVENTORY_SIZE):
        service.dig_repo.add_inventory_item(player_id, guild_id, "lantern")
    monkeypatch.setattr(
        dig_service_module,
        "EVENT_POOL",
        [_reward_event(
            consumable_reward_pool=["rescue_line"],
            artifact_reward_pool=["miner_s_lullaby"],
        )],
    )
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)

    result = service.resolve_event(
        player_id,
        guild_id,
        "non_jc_reward_probe",
        "safe",
    )

    assert result["consumable_drop"] is None
    assert result["artifact_drop"]["artifact_id"] == "miner_s_lullaby"


def test_plain_event_skips_unneeded_reward_inventory_queries(
    repo_db_path,
    player_repository,
    guild_id,
    monkeypatch,
):
    service, player_id = _event_service(
        repo_db_path,
        player_repository,
        guild_id,
    )
    monkeypatch.setattr(
        dig_service_module,
        "EVENT_POOL",
        [_reward_event()],
    )
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)
    get_gear = MagicMock(wraps=service.dig_repo.get_gear)
    get_inventory = MagicMock(wraps=service.dig_repo.get_inventory)
    get_artifacts = MagicMock(wraps=service.dig_repo.get_artifacts)
    monkeypatch.setattr(service.dig_repo, "get_gear", get_gear)
    monkeypatch.setattr(service.dig_repo, "get_inventory", get_inventory)
    monkeypatch.setattr(service.dig_repo, "get_artifacts", get_artifacts)

    service.resolve_event(
        player_id,
        guild_id,
        "non_jc_reward_probe",
        "safe",
    )

    get_gear.assert_not_called()
    get_inventory.assert_not_called()
    get_artifacts.assert_not_called()


def test_artifact_reward_pool_uses_one_owned_artifact_query(
    repo_db_path,
    player_repository,
    guild_id,
    monkeypatch,
):
    service, player_id = _event_service(
        repo_db_path,
        player_repository,
        guild_id,
    )
    monkeypatch.setattr(
        dig_service_module,
        "EVENT_POOL",
        [_reward_event(artifact_reward_pool=[
            "miner_s_lullaby",
            "map_of_the_first_descent",
        ])],
    )
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)
    get_artifacts = MagicMock(wraps=service.dig_repo.get_artifacts)
    has_artifact = MagicMock(wraps=service.dig_repo.has_artifact)
    monkeypatch.setattr(service.dig_repo, "get_artifacts", get_artifacts)
    monkeypatch.setattr(service.dig_repo, "has_artifact", has_artifact)

    result = service.resolve_event(
        player_id,
        guild_id,
        "non_jc_reward_probe",
        "safe",
    )

    assert result["artifact_drop"] is not None
    get_artifacts.assert_called_once_with(player_id, guild_id)
    has_artifact.assert_not_called()
