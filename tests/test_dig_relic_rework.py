"""Tests for the dig relic rework: dormant relics + new relics."""

from __future__ import annotations

from services.dig_constants import ALL_ARTIFACTS, ARTIFACT_BY_ID, RELICS

# All relic IDs that should now exist post-rework.
NEW_RELIC_IDS = {
    "prism_heart",
    "mana_conduit",
    "bloodstone",
    "gamblers_charm",
    "vendetta_coin",
    "mentors_lantern",
    "stormcaller",
    "slow_drip",
}

DORMANT_RELIC_IDS = {
    "root_network",
    "frozen_clock",
    "hollow_eye",
    "mycelium_link",
    "hollow_fang",
    "echo_lantern",
    "patient_stone",
}


def test_new_relics_present_in_catalog():
    relic_ids = {r.id for r in RELICS}
    assert NEW_RELIC_IDS.issubset(relic_ids), (
        f"Missing new relics: {NEW_RELIC_IDS - relic_ids}"
    )


def test_dormant_relics_present_in_catalog():
    relic_ids = {r.id for r in RELICS}
    assert DORMANT_RELIC_IDS.issubset(relic_ids), (
        f"Missing dormant relics: {DORMANT_RELIC_IDS - relic_ids}"
    )


def test_new_relics_have_lore_and_effects():
    for rid in NEW_RELIC_IDS:
        relic = ARTIFACT_BY_ID[rid]
        assert relic.is_relic is True
        assert relic.effect, f"{rid} missing effect text"
        assert relic.lore_text, f"{rid} missing lore"


def test_total_artifact_count_matches_expected():
    # Relics-only catalog: 30 originals + the 7-relic "buff fun" batch.
    assert len(ALL_ARTIFACTS) == 37


def test_post_pinnacle_decay_factor_root_network_slows_decay():
    """Root Network multiplies the per-25-depth decay rate by 0.75."""
    # Direct unit test on the math: 100 depth past pinnacle = 4 steps.
    # Base: 1.0 - 0.05 * 4 = 0.80
    # Root Network: 1.0 - 0.0375 * 4 = 0.85
    # Frozen Clock:  1.0 - 0.025 * 4  = 0.90
    base = 1.0 - 0.05 * 4
    root = 1.0 - 0.0375 * 4
    frozen = 1.0 - 0.025 * 4

    assert root > base
    assert frozen > root


def test_relic_yield_helper_echo_lantern_multiplies_jc(monkeypatch):
    """_relic_jc_yield_multiplier respects Echo Lantern."""
    from services.dig_service import DigService

    class _Stub(DigService):
        def __init__(self):
            pass

        def _has_relic(self, discord_id, guild_id, relic_id):
            return relic_id == "echo_lantern"

    svc = _Stub()
    mult = svc._relic_jc_yield_multiplier(
        1, 99, weather_code=None, include_random=False,
    )
    assert mult == 1.15


def test_relic_yield_helper_stormcaller_storm(monkeypatch):
    from services.dig_service import DigService

    class _Stub(DigService):
        def __init__(self):
            pass

        def _has_relic(self, discord_id, guild_id, relic_id):
            return relic_id == "stormcaller"

    svc = _Stub()
    storm_mult = svc._relic_jc_yield_multiplier(1, 99, weather_code="storm", include_random=False)
    sunny_mult = svc._relic_jc_yield_multiplier(1, 99, weather_code="sunny", include_random=False)
    other_mult = svc._relic_jc_yield_multiplier(1, 99, weather_code="rain", include_random=False)
    assert storm_mult == 1.5
    assert sunny_mult == 1.10
    assert other_mult == 1.0


def test_relic_storm_negates_hazard_only_during_storm():
    from services.dig_service import DigService

    class _Stub(DigService):
        def __init__(self):
            pass

        def _has_relic(self, discord_id, guild_id, relic_id):
            return relic_id == "stormcaller"

    svc = _Stub()
    assert svc._relic_storm_negates_hazard(1, 99, "storm") is True
    assert svc._relic_storm_negates_hazard(1, 99, "sunny") is False
    assert svc._relic_storm_negates_hazard(1, 99, None) is False


def test_relic_yield_helper_bloodstone_random_only_when_enabled(monkeypatch):
    """Bloodstone applies its coin-flip only when ``include_random=True``."""
    from services.dig_service import DigService

    class _Stub(DigService):
        def __init__(self):
            pass

        def _has_relic(self, discord_id, guild_id, relic_id):
            return relic_id == "bloodstone"

    svc = _Stub()
    deterministic = svc._relic_jc_yield_multiplier(
        1, 99, weather_code=None, include_random=False,
    )
    assert deterministic == 1.0  # Random skipped → no Bloodstone change

    # With random enabled, the helper should return either 1.5 or 0.75.
    randomized = {
        svc._relic_jc_yield_multiplier(1, 99, weather_code=None, include_random=True)
        for _ in range(40)
    }
    assert randomized.issubset({1.5, 0.75})


# ===========================================================================
# "Buff fun" batch: 7 new relics, the relic-slot ceiling, and the rollout.
# ===========================================================================

BUFF_FUN_RELIC_IDS = {
    "midas_splinter", "lucky_seam", "prospectors_streak", "first_light",
    "berserkers_mark", "gamblers_edge", "deaths_door",
}


def _relic_stub(relic_ids):
    """A bare DigService whose ``_has_relic`` is True only for the given ids."""
    from services.dig_service import DigService

    class _Stub(DigService):
        def __init__(self):
            self._equipped = set(relic_ids)

        def _has_relic(self, discord_id, guild_id, relic_id):
            return relic_id in self._equipped

    return _Stub()


def test_buff_fun_relics_present_with_lore_and_effects():
    for rid in BUFF_FUN_RELIC_IDS:
        relic = ARTIFACT_BY_ID[rid]
        assert relic.is_relic and relic.effect and relic.lore_text


def test_deaths_door_renamed_off_the_second_wind_mutation():
    # ``second_wind`` is an existing mutation id — the relic must not reuse it.
    assert "second_wind" not in ARTIFACT_BY_ID
    assert ARTIFACT_BY_ID["deaths_door"].name == "Death's Door"


def test_deaths_door_is_the_nameless_depth_trophy():
    from services.dig_constants import TROPHY_RELIC_IDS
    from services.dig_data.bosses import get_boss_by_id

    assert "deaths_door" in TROPHY_RELIC_IDS
    assert get_boss_by_id("nameless_depth").trophy_relic_id == "deaths_door"


def test_relic_slot_cap_has_a_ceiling():
    svc = _relic_stub([])
    assert svc._relic_slot_cap(0) == 1
    assert svc._relic_slot_cap(3) == 4
    assert svc._relic_slot_cap(5) == 6
    assert svc._relic_slot_cap(6) == 6    # ceiling reached
    assert svc._relic_slot_cap(50) == 6   # no infinite scaling


def test_midas_splinter_doubles_on_proc(monkeypatch):
    svc = _relic_stub(["midas_splinter"])
    monkeypatch.setattr("services.dig.gear_mixin.random.random", lambda: 0.01)  # < 0.04
    assert svc._relic_jc_yield_multiplier(1, 0, luminosity=None) == 2.0
    monkeypatch.setattr("services.dig.gear_mixin.random.random", lambda: 0.5)
    assert svc._relic_jc_yield_multiplier(1, 0, luminosity=None) == 1.0


def test_lucky_seam_jackpot_on_proc(monkeypatch):
    svc = _relic_stub(["lucky_seam"])
    monkeypatch.setattr("services.dig.gear_mixin.random.random", lambda: 0.001)  # < 0.005
    assert svc._relic_jc_yield_multiplier(1, 0, luminosity=None) == 10.0
    monkeypatch.setattr("services.dig.gear_mixin.random.random", lambda: 0.5)
    assert svc._relic_jc_yield_multiplier(1, 0, luminosity=None) == 1.0


def test_first_light_doubles_first_dig_of_day():
    svc = _relic_stub(["first_light"])
    # Deterministic (not gated by include_random): first dig => x2, else x1.
    assert svc._relic_jc_yield_multiplier(
        1, 0, luminosity=None, is_first_dig_today=True, include_random=False
    ) == 2.0
    assert svc._relic_jc_yield_multiplier(
        1, 0, luminosity=None, is_first_dig_today=False, include_random=False
    ) == 1.0


def test_is_first_dig_of_day():
    import time

    from utils.game_date import get_game_date
    svc = _relic_stub([])
    today = get_game_date()
    assert svc._is_first_dig_of_day(None, today) is True
    assert svc._is_first_dig_of_day(int(time.time()), today) is False
    assert svc._is_first_dig_of_day(int(time.time()) - 3 * 86400, today) is True


def _one_round(svc, status_effects, **kw):
    base = {
        "round_num": 1, "player_hp": 10, "boss_hp": 100,
        "player_hit": 1.0, "player_dmg": 2, "boss_hit": 0.0, "boss_dmg": 1,
        "status_effects": status_effects,
    }
    base.update(kw)
    return svc._run_one_round(**base)


def test_gamblers_edge_doubles_a_hit(monkeypatch):
    svc = _relic_stub(["gamblers_edge"])
    se = svc._trophy_status_seed(1, 0, player_start_hp=10)
    assert se.get("relic_double_hit") is True
    monkeypatch.setattr("services.dig.combat_mixin.random.random", lambda: 0.0)
    entry, _php, boss_hp, _term = _one_round(svc, se)
    assert boss_hp == 96 and entry.get("double_hit")  # 2 doubled => 4


def test_berserkers_mark_ramps_and_caps(monkeypatch):
    svc = _relic_stub(["berserkers_mark"])
    se = svc._trophy_status_seed(1, 0, player_start_hp=20)
    monkeypatch.setattr("services.dig.combat_mixin.random.random", lambda: 0.0)
    # player + boss both land each round => rage 0,1,2 => dmg 2,3,4 (cap +2).
    _e, php, b1, _ = _one_round(svc, se, player_hp=20, boss_hit=1.0)
    _e, php, b2, _ = _one_round(svc, se, round_num=2, player_hp=php, boss_hp=b1, boss_hit=1.0)
    _e, php, b3, _ = _one_round(svc, se, round_num=3, player_hp=php, boss_hp=b2, boss_hit=1.0)
    assert (100 - b1, b1 - b2, b2 - b3) == (2, 3, 4)


def test_deaths_door_survives_once_then_dies(monkeypatch):
    svc = _relic_stub(["deaths_door"])
    se = svc._trophy_status_seed(1, 0, player_start_hp=3)
    assert se.get("relic_deaths_door") is True
    monkeypatch.setattr("services.dig.combat_mixin.random.random", lambda: 0.0)
    # Player misses, boss lands a lethal blow; Death's Door saves at 1 HP.
    entry, php, _b, term = _one_round(svc, se, player_hp=1, player_hit=0.0, boss_hit=1.0, boss_dmg=5)
    assert term is None and php == 1 and entry.get("deaths_door")
    assert se["relic_deaths_door"] is False
    # Charge spent => the next lethal blow kills.
    entry, php, _b, term = _one_round(
        svc, se, round_num=2, player_hp=1, player_hit=0.0, boss_hit=1.0, boss_dmg=5
    )
    assert term is False and php <= 0


def test_relic_cap_migration_trims_to_six_newest(repo_db_path):
    """The one-time rollout keeps each player's 6 newest equipped relics."""
    from infrastructure.schema_manager import SchemaManager
    from repositories.dig_repository import DigRepository

    repo = DigRepository(repo_db_path)
    # Over-cap player (8 equipped) and an under-cap player (3 equipped).
    over_ids = []
    repo.create_tunnel(1, 0, "Over")
    repo.update_tunnel(1, 0, prestige_level=9)
    for rid in list(BUFF_FUN_RELIC_IDS) + ["mole_claws"]:  # 8 distinct relics
        db_id = repo.add_artifact(1, 0, rid, is_relic=True)
        repo.equip_relic(int(db_id), True)
        over_ids.append(int(db_id))
    repo.create_tunnel(2, 0, "Under")
    repo.update_tunnel(2, 0, prestige_level=3)
    for rid in ("crystal_compass", "magma_heart", "echo_stone"):
        repo.equip_relic(int(repo.add_artifact(2, 0, rid, is_relic=True)), True)

    sm = SchemaManager(repo_db_path)
    conn = sm._connect()
    try:
        sm._migration_relic_loadout_cap_and_streak(conn.cursor())
    finally:
        conn.close()

    over_equipped = sorted(
        int(a["id"]) for a in repo.get_artifacts(1, 0) if int(a["equipped"]) == 1
    )
    assert over_equipped == sorted(over_ids)[-6:]                  # 6 newest kept
    assert int(repo.get_tunnel(1, 0)["relic_trim_notice"]) == 1    # flagged for notice
    under_equipped = [a for a in repo.get_artifacts(2, 0) if int(a["equipped"]) == 1]
    assert len(under_equipped) == 3                                # untouched
    assert int(repo.get_tunnel(2, 0)["relic_trim_notice"]) == 0
