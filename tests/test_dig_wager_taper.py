"""Wager payout multiplier tapers toward break-even at high win chance.

Boss betting was strongly +EV at every win chance, so players softened a boss
to a near-certain win, then bet big for near-risk-free profit. The payout
multiplier now tapers from the authored BOSS_PAYOUTS value (left untouched at
or below the knee win chance) down to fair odds (1 / win_chance) at the
win-chance cap, so a near-certain wager is roughly EV-neutral. Normal and
genuinely-risky betting is unaffected.
"""
import math

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


class TestLogCappedMultiplier:
    """Wager profit compresses logarithmically and hard-caps at 5x total.

    Top-end boss wagers were printing 10x+ of the stake (table multiplier x
    ascension payout bonus). The settlement multiplier now scales as
    ``1 + ln(m)``, with profit hard-capped at 4x the stake — a winning
    wager returns at most 5x total, no matter what stacked on top.
    """

    def test_at_or_below_break_even_is_untouched(self, dig_service):
        assert dig_service._log_capped_multiplier(1.0) == 1.0
        assert dig_service._log_capped_multiplier(0.8) == 0.8

    def test_low_multipliers_compress_mildly(self, dig_service):
        # ln is near-linear just above 1: a 1.5x total return only drops to
        # ~1.41x, so low-tier wagers keep roughly their authored payout.
        eff = dig_service._log_capped_multiplier(1.5)
        assert eff == pytest.approx(1.405, abs=0.005)

    @pytest.mark.parametrize(
        "raw,expected",
        [(4.3, 2.459), (8.4, 3.128), (10.3, 3.332), (10.3 * 1.5, 3.738)],
    )
    def test_top_end_compresses_to_ln(self, dig_service, raw, expected):
        # Old top-end multipliers — including with the ascension payout
        # bonus stacked on — compress to 1 + ln(m), well under 5x total.
        eff = dig_service._log_capped_multiplier(raw)
        assert eff == pytest.approx(expected, abs=0.005)
        assert eff <= 5.0

    def test_profit_hard_caps_at_four_times_the_stake(self, dig_service):
        # Beyond m = e^4 the ceiling binds: total return never passes 5x.
        assert dig_service._log_capped_multiplier(60.0) == pytest.approx(5.0)
        assert dig_service._log_capped_multiplier(1000.0) == pytest.approx(5.0)

    def test_monotonic_below_the_cap(self, dig_service):
        # Ordering between tiers is preserved until the cap flattens them.
        raws = [1.2, 1.5, 1.8, 2.1, 2.4]
        effs = [dig_service._log_capped_multiplier(m) for m in raws]
        assert effs == sorted(effs)
        assert all(1.0 < e <= 2.0 for e in effs)

    def test_never_raises_a_multiplier(self, dig_service):
        for raw in [1.05, 1.5, 2.0, 4.8, 9.7]:
            assert dig_service._log_capped_multiplier(raw) <= raw

    def test_settled_multiplier_folds_bonus_inside_the_cap(self, dig_service):
        # The shared settlement helper applies the ascension payout bonus
        # BEFORE log compression, so a bonus enlarges the pre-cap input
        # instead of multiplying the capped result past the 5x ceiling.
        settled = dig_service._settled_wager_multiplier(10.3, 1.5)
        assert settled == pytest.approx(1 + math.log(10.3 * 1.5))
        # A bonus stacked on an already-huge multiplier still cannot pass 5x.
        assert dig_service._settled_wager_multiplier(60.0, 2.0) == pytest.approx(5.0)
        # No bonus -> identical to the raw log cap.
        assert dig_service._settled_wager_multiplier(1.5) == (
            dig_service._log_capped_multiplier(1.5)
        )

    def test_composes_with_taper_near_fair_odds(self, dig_service):
        # The high-win-chance taper lands near fair odds (~1.05x); the log
        # cap must pass that through essentially unchanged so near-certain
        # wagers stay EV-neutral rather than getting double-penalized.
        tapered = dig_service._effective_wager_multiplier(4.8, WIN_CHANCE_CAP)
        eff = dig_service._log_capped_multiplier(tapered)
        assert eff == pytest.approx(tapered, abs=0.002)
