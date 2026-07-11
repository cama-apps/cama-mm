"""Balance-constant snapshot — an intentional-change guard.

These constants directly tune gameplay/economy balance. Pinning their in-code
default values here means an *accidental* change (a typo, an unrelated refactor,
a bad merge) fails CI loudly and forces a conscious update to this file. This is
deliberately a value-pin, not a behavioral test: when you change a balance knob
on purpose, update the matching assertion in the same commit.

Config values are env-overridable in production; these pins guard the in-code
defaults, which is what CI runs against.
"""
from config import (
    BANKRUPTCY_PENALTY_GAMES,
    BANKRUPTCY_PENALTY_RATE,
    REBELLION_ATTACKER_FLAT_REWARD,
    REBELLION_DEFENDER_STAKE,
    REBELLION_INCITER_FLAT_REWARD,
    WHEEL_TARGET_EV,
)
from services.dig_data.layers import PAID_DIG_COST_CAP, PAID_DIG_COSTS_PER_DAY
from services.prediction_service import PredictionService


def test_paid_dig_cost_ladder_pinned():
    assert PAID_DIG_COSTS_PER_DAY == [3, 5, 10, 20, 40]
    assert PAID_DIG_COST_CAP == 40


def test_gamba_wheel_target_ev_pinned():
    assert WHEEL_TARGET_EV == -27.5


def test_bankruptcy_penalty_pinned():
    assert BANKRUPTCY_PENALTY_GAMES == 3
    assert BANKRUPTCY_PENALTY_RATE == 0.75


def test_prediction_resolution_threshold_pinned():
    assert PredictionService.MIN_RESOLUTION_VOTES == 3


def test_rebellion_reward_constants_pinned():
    assert REBELLION_INCITER_FLAT_REWARD == 30
    assert REBELLION_ATTACKER_FLAT_REWARD == 15
    assert REBELLION_DEFENDER_STAKE == 10
