"""Wager payout multiplier tapers toward break-even at high win chance.

Boss betting was strongly +EV at every win chance, so players softened a boss
to a near-certain win, then bet big for near-risk-free profit. The payout
multiplier now tapers from the authored BOSS_PAYOUTS value (left untouched at
or below the knee win chance) down to fair odds (1 / win_chance) at the
win-chance cap, so a near-certain wager is roughly EV-neutral. Normal and
genuinely-risky betting is unaffected.
"""
import pytest

from repositories.dig_repository import DigRepository
from services.dig_service import WIN_CHANCE_CAP, DigService

# At or below this win chance the authored multiplier is returned unchanged.
_KNEE = 0.65


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository):
    return DigService(dig_repo, player_repository)


@pytest.mark.parametrize("win_chance", [0.05, 0.40, 0.55, _KNEE])
def test_multiplier_unchanged_at_or_below_knee(dig_service, win_chance):
    # Normal and genuinely-risky betting must be left exactly as authored.
    assert dig_service._effective_wager_multiplier(4.8, win_chance) == 4.8


def test_multiplier_is_fair_odds_at_win_chance_cap(dig_service):
    # A near-certain wager pays break-even: EV = c*M - 1 == 0  ->  M == 1/c.
    eff = dig_service._effective_wager_multiplier(4.8, WIN_CHANCE_CAP)
    assert eff == pytest.approx(1.0 / WIN_CHANCE_CAP)
    # EV-neutral: winning returns roughly the stake, no profit.
    assert WIN_CHANCE_CAP * eff == pytest.approx(1.0)


def test_multiplier_tapers_monotonically_above_knee(dig_service):
    above_knee = [0.70, 0.80, 0.88, 0.95]
    effs = [dig_service._effective_wager_multiplier(9.7, c) for c in above_knee]
    assert effs == sorted(effs, reverse=True)  # strictly decreasing
    for c, eff in zip(above_knee, effs):
        # Tapered: below the authored 9.7x, but no worse than fair odds.
        assert 1.0 / c <= eff < 9.7


def test_taper_never_raises_a_low_base_multiplier(dig_service):
    # A boss whose authored multiplier is already below fair odds must not be
    # tapered UPWARD — the taper only ever reduces a payout.
    assert dig_service._effective_wager_multiplier(1.5, 0.95) <= 1.5
