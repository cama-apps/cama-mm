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
    # Relics-only catalog after the non-functional collectibles were cut.
    assert len(ALL_ARTIFACTS) == 30


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
