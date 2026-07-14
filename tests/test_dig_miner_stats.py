"""Focused sanity coverage for the miner S-stat formulas.

Starting budget, allocation rejection, and boss-point awards already have
integration coverage in ``test_dig_service`` and ``test_boss_duel``.  These
tests pin the exact formula boundaries that were not previously covered.
"""

from __future__ import annotations

import pytest

from services.dig_service import DigService


@pytest.fixture
def stat_service() -> DigService:
    """Return an uninitialized service for the pure stat helpers."""
    return object.__new__(DigService)


@pytest.mark.parametrize(
    ("strength", "expected_min_bonus", "expected_max_bonus"),
    [
        (0, 0, 0),
        (1, 0, 0),
        (2, 0, 1),
        (4, 0, 2),
        (5, 1, 2),
        (10, 2, 5),
    ],
)
def test_strength_bonuses_change_at_exact_thresholds(
    stat_service: DigService,
    strength: int,
    expected_min_bonus: int,
    expected_max_bonus: int,
) -> None:
    effects = stat_service._get_stat_effects({"strength": strength})

    assert effects["advance_min_bonus"] == expected_min_bonus
    assert effects["advance_max_bonus"] == expected_max_bonus


@pytest.mark.parametrize(("smarts", "expected_reduction"), [(0, 0.0), (1, 0.02), (5, 0.10)])
def test_smarts_reduces_cave_in_chance_by_two_percent_per_point(
    stat_service: DigService,
    smarts: int,
    expected_reduction: float,
) -> None:
    effects = stat_service._get_stat_effects({"smarts": smarts})

    assert effects["cave_in_reduction"] == pytest.approx(expected_reduction)


@pytest.mark.parametrize(
    ("stamina", "expected_multiplier"),
    [(0, 1.0), (1, 0.96), (12, 0.52), (13, 0.50), (100, 0.50)],
)
def test_stamina_reduces_costs_four_percent_per_point_with_half_cap(
    stat_service: DigService,
    stamina: int,
    expected_multiplier: float,
) -> None:
    effects = stat_service._get_stat_effects({"stamina": stamina})

    assert effects["cooldown_multiplier"] == pytest.approx(expected_multiplier)
    assert effects["paid_cost_multiplier"] == pytest.approx(expected_multiplier)
