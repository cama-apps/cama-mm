"""Relic rarity, ordinary-pool selection, and atomic recycling tests."""

from __future__ import annotations

import pytest

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_data import artifacts
from services.dig_service import DigService

EXPECTED_EXISTING_RARITIES = {
    "Common": {
        "crystal_compass", "obsidian_shield", "mycelium_link",
        "midas_splinter", "lucky_seam",
    },
    "Uncommon": {
        "mole_claws", "magma_heart", "root_network", "mana_conduit",
        "mentors_lantern", "prospectors_streak",
    },
    "Rare": {
        "echo_stone", "spore_cloak", "frozen_clock", "gamblers_charm",
        "stormcaller", "slow_drip",
    },
    "Legendary": {"hollow_eye", "prism_heart", "bloodstone", "vendetta_coin"},
}

EXPECTED_NEW_RELICS = {
    "chipped_compass": "Common",
    "lantern_stub": "Common",
    "paper_crane": "Uncommon",
    "bone_abacus": "Uncommon",
    "bottled_quake": "Rare",
    "black_wax_seal": "Rare",
    "burning_ledger": "Legendary",
    "shifting_idol": "Legendary",
}


@pytest.fixture
def relic_service(repo_db_path):
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=501, discord_username="Recycler", guild_id=9)
    dig_repo.create_tunnel(501, 9, "Recycle Run")
    return DigService(dig_repo, player_repo)


def _add_relics(service: DigService, *artifact_ids: str) -> list[int]:
    return [
        service.dig_repo.add_artifact(501, 9, artifact_id, is_relic=True)
        for artifact_id in artifact_ids
    ]


def test_existing_ordinary_relics_are_reclassified_across_four_tiers():
    by_id = {relic.id: relic for relic in artifacts.RELICS}

    assert artifacts.RELIC_RARITY_ORDER == (
        "Common", "Uncommon", "Rare", "Legendary",
    )
    for rarity, relic_ids in EXPECTED_EXISTING_RARITIES.items():
        assert {relic_id for relic_id in relic_ids if by_id[relic_id].rarity == rarity} == relic_ids


def test_new_relic_catalog_has_two_relics_per_rarity():
    by_id = {relic.id: relic for relic in artifacts.RELICS}

    assert {relic_id: by_id[relic_id].rarity for relic_id in EXPECTED_NEW_RELICS} == (
        EXPECTED_NEW_RELICS
    )
    assert all(by_id[relic_id].is_relic for relic_id in EXPECTED_NEW_RELICS)


@pytest.mark.parametrize(
    ("roll", "expected"),
    [
        (0.0, "Common"),
        (0.599999, "Common"),
        (0.60, "Uncommon"),
        (0.899999, "Uncommon"),
        (0.90, "Rare"),
        (0.989999, "Rare"),
        (0.99, "Legendary"),
        (0.999999, "Legendary"),
    ],
)
def test_relic_rarity_roll_uses_60_30_9_1_weights(roll, expected):
    assert artifacts.relic_rarity_for_roll(roll) == expected


@pytest.mark.parametrize(
    ("rarity_roll", "expected"),
    [
        (0.10, "Common"),
        (0.70, "Uncommon"),
        (0.95, "Rare"),
        (0.995, "Legendary"),
    ],
)
def test_raw_relic_find_uses_weighted_ordinary_rarity_pool(
    relic_service, monkeypatch, rarity_roll, expected,
):
    rolls = iter((0.0, rarity_roll))
    monkeypatch.setattr("services.dig.gear_mixin.random.random", lambda: next(rolls))
    monkeypatch.setattr("services.dig.gear_mixin.random.choice", lambda pool: pool[0])

    result = relic_service.roll_artifact(501, 9, 160)

    assert result is not None
    assert result["rarity"] == expected.lower()
    assert artifacts.ARTIFACT_BY_ID[result["id"]].rarity == expected
    assert artifacts.is_ordinary_relic(artifacts.ARTIFACT_BY_ID[result["id"]])


def test_ordinary_relic_excludes_progression_and_boss_exclusives():
    by_id = {relic.id: relic for relic in artifacts.RELICS}

    assert artifacts.is_ordinary_relic(by_id["crystal_compass"])
    assert not artifacts.is_ordinary_relic(by_id["first_light"])
    assert not artifacts.is_ordinary_relic(by_id["weeping_fang"])
    assert not artifacts.is_ordinary_relic(by_id["aegis_fragment"])


def test_recycle_three_common_relics_into_random_uncommon(relic_service, monkeypatch):
    row_ids = _add_relics(
        relic_service, "crystal_compass", "obsidian_shield", "mycelium_link",
    )
    monkeypatch.setattr(
        "services.dig.gear_mixin.random.choice",
        lambda pool: next(relic for relic in pool if relic.id == "mole_claws"),
    )

    result = relic_service.recycle_relics(501, 9, row_ids)

    assert result["success"] is True
    assert result["source_rarity"] == "Common"
    assert result["output_rarity"] == "Uncommon"
    assert result["relic_id"] == "mole_claws"
    owned = relic_service.dig_repo.get_artifacts(501, 9)
    assert [row["artifact_id"] for row in owned] == ["mole_claws"]


def test_recycle_allows_duplicate_random_output(relic_service, monkeypatch):
    _add_relics(relic_service, "mole_claws")
    row_ids = _add_relics(
        relic_service, "crystal_compass", "obsidian_shield", "mycelium_link",
    )
    monkeypatch.setattr(
        "services.dig.gear_mixin.random.choice",
        lambda pool: next(relic for relic in pool if relic.id == "mole_claws"),
    )

    result = relic_service.recycle_relics(501, 9, row_ids)

    assert result["success"] is True
    assert sum(
        row["artifact_id"] == "mole_claws"
        for row in relic_service.dig_repo.get_artifacts(501, 9)
    ) == 2


@pytest.mark.parametrize(
    ("artifact_ids", "error_fragment"),
    [
        (("crystal_compass", "obsidian_shield"), "exactly three"),
        (("crystal_compass", "obsidian_shield", "mole_claws"), "same rarity"),
        (("hollow_eye", "prism_heart", "bloodstone"), "Legendary"),
        (("first_light", "berserkers_mark", "gamblers_edge"), "progression"),
    ],
)
def test_recycle_rejects_invalid_sets(relic_service, artifact_ids, error_fragment):
    row_ids = _add_relics(relic_service, *artifact_ids)

    result = relic_service.recycle_relics(501, 9, row_ids)

    assert result["success"] is False
    assert error_fragment.lower() in result["error"].lower()
    assert len(relic_service.dig_repo.get_artifacts(501, 9)) == len(artifact_ids)


def test_recycle_rejects_equipped_input_without_consuming_anything(relic_service):
    row_ids = _add_relics(
        relic_service, "crystal_compass", "obsidian_shield", "mycelium_link",
    )
    relic_service.dig_repo.equip_relic(row_ids[0], 501, 9, True)

    result = relic_service.recycle_relics(501, 9, row_ids)

    assert result["success"] is False
    assert "unequipped" in result["error"].lower()
    assert len(relic_service.dig_repo.get_artifacts(501, 9)) == 3


def test_repository_recycle_rolls_back_when_a_selected_row_is_stale(relic_service):
    row_ids = _add_relics(
        relic_service, "crystal_compass", "obsidian_shield", "mycelium_link",
    )

    with pytest.raises(ValueError, match="eligible relic rows"):
        relic_service.dig_repo.atomic_recycle_relics(
            501,
            9,
            [row_ids[0], row_ids[1], 999999],
            "mole_claws",
        )

    assert len(relic_service.dig_repo.get_artifacts(501, 9)) == 3
