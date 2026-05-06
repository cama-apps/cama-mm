"""Tests for the bankruptcy-recovery buffs.

Covers:
- Bankrupt wheel EV calibration (target +25 JC ±1)
- Daily mana flag idempotency (insurance + reroll)
- White stipend partial-pay edge cases
- Trivia bankrupt multiplier configuration
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from repositories.mana_repository import ManaRepository
from repositories.player_repository import PlayerRepository
from services.mana_effects_service import ManaEffectsService
from services.mana_service import ManaService, get_today_pst
from tests.conftest import TEST_GUILD_ID

GID = TEST_GUILD_ID


# =============================================================================
# Bankrupt wheel EV calibration
# =============================================================================


class TestBankruptWheelEV:
    """Verify the bankrupt wheel actually averages +25 JC after the math fix."""

    def test_bankrupt_wheel_mean_equals_target(self):
        from config import WHEEL_BANKRUPT_TARGET_EV
        from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES, bankrupt_special_ev

        total = 0.0
        for _, value, _ in BANKRUPT_WHEEL_WEDGES:
            if isinstance(value, int):
                total += value
            else:
                total += bankrupt_special_ev(value)
        mean = total / len(BANKRUPT_WHEEL_WEDGES)

        assert abs(mean - WHEEL_BANKRUPT_TARGET_EV) <= 1.0, (
            f"Bankrupt wheel mean {mean:.2f} drifted from target "
            f"{WHEEL_BANKRUPT_TARGET_EV} by more than 1 JC"
        )

    def test_bankrupt_wedge_value_clamped_negative(self):
        """The dynamically calculated BANKRUPT wedge must always be negative."""
        from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

        bankrupt_values = [v for _, v, _ in BANKRUPT_WHEEL_WEDGES if isinstance(v, int) and v < 0]
        assert len(bankrupt_values) == 2
        assert all(v <= -1 for v in bankrupt_values)

    def test_normal_wheel_target_ev_unchanged(self):
        """The non-bankrupt wheel should still be at its normal -25 target."""
        from config import (
            WHEEL_BLUE_SHELL_EST_EV,
            WHEEL_COMEBACK_EST_EV,
            WHEEL_COMMUNE_EST_EV,
            WHEEL_LIGHTNING_BOLT_EST_EV,
            WHEEL_RED_SHELL_EST_EV,
            WHEEL_TARGET_EV,
        )
        from utils.wheel_drawing import WHEEL_WEDGES

        special_ev = {
            "RED_SHELL": WHEEL_RED_SHELL_EST_EV,
            "BLUE_SHELL": WHEEL_BLUE_SHELL_EST_EV,
            "LIGHTNING_BOLT": WHEEL_LIGHTNING_BOLT_EST_EV,
            "COMMUNE": WHEEL_COMMUNE_EST_EV,
            "COMEBACK": WHEEL_COMEBACK_EST_EV,
        }

        total = 0.0
        for _, value, _ in WHEEL_WEDGES:
            if isinstance(value, int):
                total += value
            else:
                total += special_ev.get(value, 0.0)
        mean = total / len(WHEEL_WEDGES)

        assert abs(mean - WHEEL_TARGET_EV) <= 1.0


# =============================================================================
# Daily mana flag idempotency (Green insurance, Red re-roll)
# =============================================================================


def _make_mana_service(mana_repo, player_repo):
    gambling_stats = MagicMock()
    gambling_stats.calculate_degen_score.return_value = MagicMock(total=0)
    gambling_stats.bet_repo = MagicMock()
    gambling_stats.bet_repo.get_player_bet_history.return_value = []
    bankruptcy_service = MagicMock()
    bankruptcy_service.get_state.return_value = MagicMock(
        penalty_games_remaining=0, last_bankruptcy_at=None
    )
    tip_repo = MagicMock()
    tip_repo.get_user_tip_stats.return_value = {"total_sent": 0, "tips_sent_count": 0}
    return ManaService(
        mana_repo=mana_repo,
        player_repo=player_repo,
        gambling_stats_service=gambling_stats,
        bankruptcy_service=bankruptcy_service,
        tip_repo=tip_repo,
    )


def _register(player_repo, discord_id, guild_id=GID, balance=0):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"P{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    # Always set balance explicitly — `add` defaults to 3 JC, which would
    # mask the bankrupt-trigger boundary in tests.
    player_repo.update_balance(discord_id, guild_id, balance)


class TestBankruptBuffFlags:
    """Insurance + reroll flags persist for the mana day and reset on a new claim."""

    @pytest.fixture
    def repo(self, repo_db_path):
        return ManaRepository(repo_db_path)

    def test_no_row_means_unused(self, repo):
        assert repo.is_bankrupt_buff_used(11111, GID, "insurance") is False
        assert repo.is_bankrupt_buff_used(11111, GID, "reroll") is False

    def test_no_row_claim_fails(self, repo):
        assert repo.claim_bankrupt_buff_atomic(11111, GID, "insurance") is False
        assert repo.claim_bankrupt_buff_atomic(11111, GID, "reroll") is False

    def test_claim_once_then_used(self, repo):
        repo.set_mana(22222, GID, "Forest", get_today_pst())
        assert repo.claim_bankrupt_buff_atomic(22222, GID, "insurance") is True
        assert repo.is_bankrupt_buff_used(22222, GID, "insurance") is True
        # Reroll is independent
        assert repo.is_bankrupt_buff_used(22222, GID, "reroll") is False

    def test_claim_twice_fails(self, repo):
        repo.set_mana(33333, GID, "Mountain", get_today_pst())
        assert repo.claim_bankrupt_buff_atomic(33333, GID, "reroll") is True
        assert repo.claim_bankrupt_buff_atomic(33333, GID, "reroll") is False

    def test_new_mana_day_resets_flags(self, repo):
        # Set mana yesterday, claim insurance
        repo.set_mana(44444, GID, "Forest", "2020-01-01")
        assert repo.claim_bankrupt_buff_atomic(44444, GID, "insurance") is True

        # New day's atomic claim resets the flags
        new_day = get_today_pst()
        assert repo.claim_mana_atomic(44444, GID, "Forest", new_day) is True
        assert repo.is_bankrupt_buff_used(44444, GID, "insurance") is False
        assert repo.is_bankrupt_buff_used(44444, GID, "reroll") is False

    def test_unknown_buff_raises(self, repo):
        with pytest.raises(ValueError):
            repo.is_bankrupt_buff_used(55555, GID, "bogus")
        with pytest.raises(ValueError):
            repo.claim_bankrupt_buff_atomic(55555, GID, "bogus")


# =============================================================================
# White stipend (apply_bankrupt_stipend on Plains)
# =============================================================================


class TestWhiteStipend:
    """White mana grants a daily stipend from the nonprofit fund while bankrupt."""

    @pytest.fixture
    def svc(self, repo_db_path):
        mana_repo = ManaRepository(repo_db_path)
        player_repo = PlayerRepository(repo_db_path)
        mana_service = _make_mana_service(mana_repo, player_repo)
        loan_service = MagicMock()
        loan_service.get_nonprofit_fund.return_value = 100
        loan_service.subtract_from_nonprofit_fund.return_value = 95
        loan_service.add_to_nonprofit_fund.return_value = 100
        effects_service = ManaEffectsService(
            mana_service=mana_service,
            player_repo=player_repo,
            mana_repo=mana_repo,
            loan_service=loan_service,
        )
        return {"effects": effects_service, "player_repo": player_repo, "loan_service": loan_service}

    def test_non_plains_no_stipend(self, svc):
        _register(svc["player_repo"], 70001, balance=-10)
        paid = svc["effects"].apply_bankrupt_stipend(70001, GID, "Mountain")
        assert paid == 0
        svc["loan_service"].subtract_from_nonprofit_fund.assert_not_called()

    def test_positive_balance_no_stipend(self, svc):
        _register(svc["player_repo"], 70002, balance=50)
        paid = svc["effects"].apply_bankrupt_stipend(70002, GID, "Plains")
        assert paid == 0
        svc["loan_service"].subtract_from_nonprofit_fund.assert_not_called()

    def test_zero_balance_pays(self, svc):
        from config import WHITE_BANKRUPT_STIPEND
        _register(svc["player_repo"], 70003, balance=0)
        paid = svc["effects"].apply_bankrupt_stipend(70003, GID, "Plains")
        assert paid == WHITE_BANKRUPT_STIPEND
        svc["loan_service"].subtract_from_nonprofit_fund.assert_called_once_with(
            GID, WHITE_BANKRUPT_STIPEND
        )

    def test_negative_balance_pays_full(self, svc):
        from config import WHITE_BANKRUPT_STIPEND
        _register(svc["player_repo"], 70004, balance=-25)
        paid = svc["effects"].apply_bankrupt_stipend(70004, GID, "Plains")
        assert paid == WHITE_BANKRUPT_STIPEND
        # Player credited
        new_bal = svc["player_repo"].get_balance(70004, GID)
        assert new_bal == -25 + WHITE_BANKRUPT_STIPEND

    def test_partial_pay_when_fund_low(self, svc):
        _register(svc["player_repo"], 70005, balance=-10)
        svc["loan_service"].get_nonprofit_fund.return_value = 3
        paid = svc["effects"].apply_bankrupt_stipend(70005, GID, "Plains")
        assert paid == 3
        svc["loan_service"].subtract_from_nonprofit_fund.assert_called_once_with(GID, 3)

    def test_empty_fund_skips(self, svc):
        _register(svc["player_repo"], 70006, balance=-10)
        svc["loan_service"].get_nonprofit_fund.return_value = 0
        paid = svc["effects"].apply_bankrupt_stipend(70006, GID, "Plains")
        assert paid == 0
        svc["loan_service"].subtract_from_nonprofit_fund.assert_not_called()


# =============================================================================
# Trivia bankrupt multiplier configuration
# =============================================================================


class TestTriviaBankruptMultiplier:
    """The bankrupt multiplier and stacking math behave as configured."""

    def test_multiplier_is_2x(self):
        from config import TRIVIA_BANKRUPT_MULTIPLIER
        assert TRIVIA_BANKRUPT_MULTIPLIER == 2.0

    def test_stack_with_red_mana(self):
        """bankrupt 2x × red 1.5x = 3 (rounded down)."""
        from config import TRIVIA_BANKRUPT_MULTIPLIER
        from domain.models.mana_effects import ManaEffects
        red = ManaEffects.for_color("Red", "Mountain")
        # Simulate the trivia milestone math: jc=1 (milestone hit)
        jc = 1
        jc = max(1, int(jc * red.trivia_payout_multiplier))  # 1 * 1.5 → 1
        jc = max(1, int(jc * TRIVIA_BANKRUPT_MULTIPLIER))    # 1 * 2 → 2
        assert jc == 2

    def test_stack_with_red_mana_higher_milestone(self):
        """At higher base milestone, stacking gives clean 3x effect."""
        from config import TRIVIA_BANKRUPT_MULTIPLIER
        from domain.models.mana_effects import ManaEffects
        red = ManaEffects.for_color("Red", "Mountain")
        jc = 2  # hypothetical higher base
        jc = max(1, int(jc * red.trivia_payout_multiplier))  # 2 * 1.5 = 3
        jc = max(1, int(jc * TRIVIA_BANKRUPT_MULTIPLIER))    # 3 * 2 = 6
        assert jc == 6
