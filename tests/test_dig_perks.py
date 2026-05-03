"""Prestige perk effects: aggregator + the three new perks
(patient_step, steady_hands, reading_the_stone)."""

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import PRESTIGE_PERK_VALUES, PRESTIGE_PERKS
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


class TestPerkEffectAggregation:
    def test_empty_perks_returns_empty_dict(self, dig_service):
        assert dig_service._aggregate_perk_effects([]) == {}

    def test_single_perk_returns_its_effects(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["advance_boost"])
        assert agg == {"advance_min_bonus": 1.0}

    def test_multiple_perks_sum_independently(self, dig_service):
        agg = dig_service._aggregate_perk_effects(
            ["advance_boost", "cave_in_resistance", "loot_multiplier"]
        )
        assert agg["advance_min_bonus"] == 1.0
        assert agg["cave_in_reduction"] == 0.05
        assert agg["jc_bonus"] == 1.0

    def test_overlapping_perks_sum_same_key(self, dig_service):
        # mixed_bonus + advance_boost both contribute advance_min_bonus
        agg = dig_service._aggregate_perk_effects(["advance_boost", "mixed_bonus"])
        assert agg["advance_min_bonus"] == pytest.approx(1.5)
        assert agg["cave_in_reduction"] == pytest.approx(0.02)
        assert agg["jc_bonus"] == pytest.approx(0.5)

    def test_unknown_perk_contributes_nothing(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["does_not_exist"])
        assert agg == {}

    def test_all_new_perks_have_values(self):
        for new_perk in ("patient_step", "steady_hands", "reading_the_stone"):
            assert new_perk in PRESTIGE_PERKS
            assert new_perk in PRESTIGE_PERK_VALUES
            assert len(PRESTIGE_PERK_VALUES[new_perk]) > 0


class TestPatientStep:
    def test_patient_step_value(self):
        assert PRESTIGE_PERK_VALUES["patient_step"] == {"streak_bonus_multiplier": 0.5}

    def test_aggregator_surfaces_streak_multiplier(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["patient_step"])
        assert agg["streak_bonus_multiplier"] == 0.5


class TestSteadyHands:
    def test_steady_hands_value(self):
        assert PRESTIGE_PERK_VALUES["steady_hands"] == {"cave_in_loss_reduction": 0.25}

    def test_aggregator_surfaces_loss_reduction(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["steady_hands"])
        assert agg["cave_in_loss_reduction"] == 0.25


class TestReadingTheStone:
    def test_reading_the_stone_value(self):
        assert PRESTIGE_PERK_VALUES["reading_the_stone"] == {"event_choice_reveal": 1.0}

    def test_aggregator_surfaces_reveal_flag(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["reading_the_stone"])
        assert agg["event_choice_reveal"] >= 1.0


class TestPerkFractionalRoundsHalfUp:
    """``mixed_bonus`` contributes 0.5 to both ``advance_min_bonus`` and
    ``jc_bonus``. Truncating with int() would silently swallow the
    contribution when the perk is picked alone — half-up rounding keeps
    the perk meaningful."""

    def test_mixed_bonus_alone_gives_one_advance(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["mixed_bonus"])
        assert agg["advance_min_bonus"] == 0.5
        # The dig flow does ``int(perk_advance_flat + 0.5)`` which yields 1
        assert int(agg["advance_min_bonus"] + 0.5) == 1

    def test_mixed_bonus_alone_gives_one_jc(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["mixed_bonus"])
        assert agg["jc_bonus"] == 0.5
        assert int(agg["jc_bonus"] + 0.5) == 1

    def test_advance_boost_plus_mixed_stacks_to_two(self, dig_service):
        agg = dig_service._aggregate_perk_effects(["advance_boost", "mixed_bonus"])
        assert agg["advance_min_bonus"] == 1.5
        assert int(agg["advance_min_bonus"] + 0.5) == 2


class TestHasPerk:
    def test_returns_false_for_no_tunnel(self, dig_service):
        assert dig_service.has_perk(99999, 12345, "advance_boost") is False
