"""
Tests for ManaService and ManaRepository.

Covers:
- Repository CRUD (get_mana, set_mana, get_all_mana)
- get_today_pst() boundary logic
- has_assigned_today()
- assign_daily_mana() — happy path and double-claim guard
- calculate_land_weights() for each land's dominant conditions
"""

from __future__ import annotations

import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

import pytest

from commands.mana import _build_single_embed
from repositories.mana_repository import ManaRepository
from services.mana_service import (
    LAND_COLORS,
    ManaService,
    get_mana_day_start_timestamp,
    get_today_pst,
)
from tests.conftest import TEST_GUILD_ID

# =============================================================================
# Fixtures
# =============================================================================


@pytest.fixture
def mana_repo(repo_db_path):
    return ManaRepository(repo_db_path)


def _make_player(
    *,
    balance: int = 10,
    wins: int = 5,
    losses: int = 5,
    glicko_rating: float | None = 2000.0,
    lowest_balance: int | None = None,
):
    """Create a mock Player object."""
    p = MagicMock()
    p.jopacoin_balance = balance
    p.wins = wins
    p.losses = losses
    p.glicko_rating = glicko_rating
    return p


def _make_degen(
    *,
    total: int = 0,
    max_leverage_score: int = 0,
    loss_chase_score: int = 0,
    bet_size_score: int = 0,
    negative_loan_bonus: int = 0,
    bankruptcy_score: int = 0,
    debt_depth_score: int = 0,
):
    d = MagicMock()
    d.total = total
    d.max_leverage_score = max_leverage_score
    d.loss_chase_score = loss_chase_score
    d.bet_size_score = bet_size_score
    d.negative_loan_bonus = negative_loan_bonus
    d.bankruptcy_score = bankruptcy_score
    d.debt_depth_score = debt_depth_score
    return d


def _make_bk_state(*, penalty_games_remaining: int = 0, last_bankruptcy_at=None):
    s = MagicMock()
    s.penalty_games_remaining = penalty_games_remaining
    s.last_bankruptcy_at = last_bankruptcy_at
    return s


def _make_tip_stats(*, total_sent: int = 0, tips_sent_count: int = 0):
    return {"total_sent": total_sent, "tips_sent_count": tips_sent_count}


def _make_service(
    mana_repo,
    *,
    player=None,
    lowest_balance=None,
    degen=None,
    bk_state=None,
    tip_stats=None,
    current_streak: int = 0,
) -> ManaService:
    player_repo = MagicMock()
    player_repo.normalize_guild_id.side_effect = lambda gid: gid if gid is not None else 0
    player_repo.get_by_id.return_value = player or _make_player()
    player_repo.get_lowest_balance.return_value = lowest_balance

    gambling_stats = MagicMock()
    gambling_stats.calculate_degen_score.return_value = degen or _make_degen()
    # Stub bet history for streak calculation (empty → streak 0)
    gambling_stats.get_player_bet_outcomes.return_value = []

    bankruptcy_service = MagicMock()
    bankruptcy_service.get_state.return_value = bk_state or _make_bk_state()

    tip_repo = MagicMock()
    tip_repo.get_user_tip_stats.return_value = tip_stats or _make_tip_stats()

    svc = ManaService(
        mana_repo=mana_repo,
        player_repo=player_repo,
        gambling_stats_service=gambling_stats,
        bankruptcy_service=bankruptcy_service,
        tip_repo=tip_repo,
    )
    # Patch _get_current_win_streak to return a fixed value for deterministic tests
    svc._get_current_win_streak = MagicMock(return_value=current_streak)

    return svc


# =============================================================================
# get_today_pst
# =============================================================================


class TestGetTodayPst:
    def test_after_reset_hour_returns_today(self):
        from zoneinfo import ZoneInfo

        la_tz = ZoneInfo("America/Los_Angeles")
        # Simulate 10 AM PST — well after 4 AM boundary
        fake_now = datetime(2025, 6, 15, 10, 0, 0, tzinfo=la_tz)
        with patch("services.mana_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            result = get_today_pst()
        assert result == "2025-06-15"

    def test_before_reset_hour_returns_yesterday(self):
        from zoneinfo import ZoneInfo

        la_tz = ZoneInfo("America/Los_Angeles")
        # Simulate 2 AM PST — before 4 AM boundary
        fake_now = datetime(2025, 6, 15, 2, 0, 0, tzinfo=la_tz)
        with patch("services.mana_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            result = get_today_pst()
        assert result == "2025-06-14"

    def test_exactly_at_reset_hour_returns_today(self):
        from zoneinfo import ZoneInfo

        la_tz = ZoneInfo("America/Los_Angeles")
        # Exactly 4:00 AM — at the boundary, counts as today
        fake_now = datetime(2025, 6, 15, 4, 0, 0, tzinfo=la_tz)
        with patch("services.mana_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            result = get_today_pst()
        assert result == "2025-06-15"


def test_mana_day_start_timestamp_observes_four_am_boundary():
    from zoneinfo import ZoneInfo

    la_tz = ZoneInfo("America/Los_Angeles")
    before_reset = datetime(2025, 6, 15, 2, 30, tzinfo=la_tz)
    after_reset = datetime(2025, 6, 15, 10, 30, tzinfo=la_tz)

    assert get_mana_day_start_timestamp(before_reset) == int(
        datetime(2025, 6, 14, 4, 0, tzinfo=la_tz).timestamp()
    )
    assert get_mana_day_start_timestamp(after_reset) == int(
        datetime(2025, 6, 15, 4, 0, tzinfo=la_tz).timestamp()
    )


def test_plains_embed_shows_guardian_capacity_and_retro_refund():
    embed = _build_single_embed(
        SimpleNamespace(display_name="Guardian"),
        {
            "land": "Plains",
            "color": "White",
            "emoji": "🌾",
            "assigned_date": get_today_pst(),
            "guardian_remaining": 16,
            "retro_refund": 9,
            "consumed": False,
        },
    )

    guardian = next(field for field in embed.fields if field.name == "🌾 Guardian Aura")
    assert "16/25 JC" in guardian.value
    assert "Recovered **9 JC**" in guardian.value


# =============================================================================
# ManaRepository
# =============================================================================


class TestManaRepository:
    def test_get_mana_returns_none_when_not_set(self, mana_repo):
        result = mana_repo.get_mana(1234, TEST_GUILD_ID)
        assert result is None

    def test_set_and_get_mana(self, mana_repo):
        mana_repo.set_mana(1234, TEST_GUILD_ID, "Island", "2025-06-15")
        result = mana_repo.get_mana(1234, TEST_GUILD_ID)
        assert result is not None
        assert result["current_land"] == "Island"
        assert result["assigned_date"] == "2025-06-15"

    def test_set_mana_overwrites_previous(self, mana_repo):
        mana_repo.set_mana(1234, TEST_GUILD_ID, "Forest", "2025-06-14")
        mana_repo.set_mana(1234, TEST_GUILD_ID, "Mountain", "2025-06-15")
        result = mana_repo.get_mana(1234, TEST_GUILD_ID)
        assert result["current_land"] == "Mountain"
        assert result["assigned_date"] == "2025-06-15"

    def test_guild_isolation(self, mana_repo):
        mana_repo.set_mana(1234, TEST_GUILD_ID, "Swamp", "2025-06-15")
        other_guild = TEST_GUILD_ID + 1
        result = mana_repo.get_mana(1234, other_guild)
        assert result is None

    def test_guild_id_none_normalised_to_zero(self, mana_repo):
        mana_repo.set_mana(1234, None, "Plains", "2025-06-15")
        result = mana_repo.get_mana(1234, 0)
        assert result is not None
        assert result["current_land"] == "Plains"

    def test_get_all_mana_returns_guild_rows(self, mana_repo):
        mana_repo.set_mana(1, TEST_GUILD_ID, "Island", "2025-06-15")
        mana_repo.set_mana(2, TEST_GUILD_ID, "Mountain", "2025-06-15")
        mana_repo.set_mana(3, TEST_GUILD_ID + 1, "Forest", "2025-06-15")  # different guild
        rows = mana_repo.get_all_mana(TEST_GUILD_ID)
        ids = {r["discord_id"] for r in rows}
        assert ids == {1, 2}
        assert 3 not in ids

    def test_get_all_mana_empty(self, mana_repo):
        rows = mana_repo.get_all_mana(TEST_GUILD_ID)
        assert rows == []

    def test_claim_mana_batch_is_one_connection_and_returns_only_new_claims(self, mana_repo):
        today = "2025-06-15"
        assert mana_repo.claim_mana_atomic(1, TEST_GUILD_ID, "Forest", today)
        assert mana_repo.claim_mana_atomic(2, TEST_GUILD_ID, "Mountain", "2025-06-14")
        assert mana_repo.mark_mana_consumed_atomic(2, TEST_GUILD_ID)
        assert mana_repo.claim_bankrupt_buff_atomic(2, TEST_GUILD_ID, "reroll")

        with patch.object(
            mana_repo, "get_connection", wraps=mana_repo.get_connection
        ) as connection:
            claimed = mana_repo.claim_mana_batch_atomic(
                [
                    (1, "Swamp"),
                    (2, "Plains"),
                    (3, "Island"),
                    (2, "Forest"),
                ],
                TEST_GUILD_ID,
                today,
            )

        assert connection.call_count == 1
        assert [row["discord_id"] for row in claimed] == [2, 3]
        assert [row["current_land"] for row in claimed] == ["Plains", "Island"]
        assert mana_repo.get_mana(1, TEST_GUILD_ID)["current_land"] == "Forest"
        player_two = mana_repo.get_mana(2, TEST_GUILD_ID)
        assert player_two == {
            "current_land": "Plains",
            "assigned_date": today,
            "consumed_today": 0,
            "white_shield_remaining": 25,
        }
        assert not mana_repo.is_bankrupt_buff_used(2, TEST_GUILD_ID, "reroll")

    def test_claim_mana_batch_is_guild_scoped(self, mana_repo):
        today = "2025-06-15"
        other_guild = TEST_GUILD_ID + 1
        assert mana_repo.claim_mana_atomic(10, other_guild, "Swamp", today)

        claimed = mana_repo.claim_mana_batch_atomic([(10, "Island")], TEST_GUILD_ID, today)

        assert [row["discord_id"] for row in claimed] == [10]
        assert mana_repo.get_mana(10, TEST_GUILD_ID)["current_land"] == "Island"
        assert mana_repo.get_mana(10, other_guild)["current_land"] == "Swamp"

    def test_concurrent_mana_batches_return_each_claim_once(self, mana_repo, repo_db_path):
        barrier = threading.Barrier(2)

        def claim(land: str):
            repo = ManaRepository(repo_db_path)
            barrier.wait()
            return repo.claim_mana_batch_atomic(
                [(20, land), (21, land)], TEST_GUILD_ID, "2025-06-15"
            )

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(claim, ["Plains", "Forest"]))

        assert sorted(len(result) for result in results) == [0, 2]
        assert sum(len(result) for result in results) == 2
        assert mana_repo.get_mana(20, TEST_GUILD_ID)["current_land"] in {
            "Plains",
            "Forest",
        }
        assert (
            mana_repo.get_mana(20, TEST_GUILD_ID)["current_land"]
            == mana_repo.get_mana(21, TEST_GUILD_ID)["current_land"]
        )


# =============================================================================
# ManaService — has_assigned_today / get_current_mana
# =============================================================================


class TestManaServiceBasics:
    def test_has_assigned_today_false_when_no_row(self, mana_repo):
        svc = _make_service(mana_repo)
        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            assert svc.has_assigned_today(9999, TEST_GUILD_ID) is False

    def test_has_assigned_today_false_when_different_date(self, mana_repo):
        mana_repo.set_mana(9999, TEST_GUILD_ID, "Forest", "2025-06-14")
        svc = _make_service(mana_repo)
        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            assert svc.has_assigned_today(9999, TEST_GUILD_ID) is False

    def test_has_assigned_today_true(self, mana_repo):
        mana_repo.set_mana(9999, TEST_GUILD_ID, "Island", "2025-06-15")
        svc = _make_service(mana_repo)
        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            assert svc.has_assigned_today(9999, TEST_GUILD_ID) is True

    def test_get_current_mana_none_when_never_assigned(self, mana_repo):
        svc = _make_service(mana_repo)
        assert svc.get_current_mana(9999, TEST_GUILD_ID) is None

    def test_get_current_mana_returns_dict(self, mana_repo):
        mana_repo.set_mana(9999, TEST_GUILD_ID, "Plains", "2025-06-15")
        svc = _make_service(mana_repo)
        result = svc.get_current_mana(9999, TEST_GUILD_ID)
        assert result is not None
        assert result["land"] == "Plains"
        assert result["color"] == "White"
        assert "emoji" in result
        assert result["assigned_date"] == "2025-06-15"


# =============================================================================
# ManaService — assign_daily_mana
# =============================================================================


class TestAssignDailyMana:
    def test_assigns_mana_and_returns_dict(self, mana_repo):
        svc = _make_service(mana_repo)
        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            result = svc.assign_daily_mana(42, TEST_GUILD_ID)
        assert "land" in result
        assert result["land"] in LAND_COLORS
        assert result["color"] == LAND_COLORS[result["land"]]
        assert "emoji" in result

        stored = mana_repo.get_mana(42, TEST_GUILD_ID)
        assert stored is not None
        assert stored["current_land"] == result["land"]
        assert stored["assigned_date"] == "2025-06-15"

    def test_raises_if_already_assigned(self, mana_repo):
        mana_repo.set_mana(42, TEST_GUILD_ID, "Forest", "2025-06-15")
        svc = _make_service(mana_repo)
        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            with pytest.raises(ValueError, match="Already assigned"):
                svc.assign_daily_mana(42, TEST_GUILD_ID)

    def test_can_reassign_next_day(self, mana_repo):
        mana_repo.set_mana(42, TEST_GUILD_ID, "Mountain", "2025-06-14")
        svc = _make_service(mana_repo)
        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            result = svc.assign_daily_mana(42, TEST_GUILD_ID)
        assert result["land"] in LAND_COLORS
        stored = mana_repo.get_mana(42, TEST_GUILD_ID)
        assert stored["assigned_date"] == "2025-06-15"

    def test_result_is_always_valid_land(self, mana_repo):
        svc = _make_service(mana_repo)
        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            for i in range(20):
                # new discord_id per iteration to avoid double-claim
                result = svc.assign_daily_mana(1000 + i, TEST_GUILD_ID)
            assert result["land"] in LAND_COLORS

    def test_plains_assignment_reconciles_guardian_since_daily_reset(self, mana_repo):
        svc = _make_service(mana_repo)
        protection_service = MagicMock()
        protection_service.reconcile_guardian.return_value = 9
        svc.protection_service = protection_service

        with (
            patch("services.mana_service.get_today_pst", return_value="2025-06-15"),
            patch("services.mana_service.random.choices", return_value=["Plains"]),
            patch(
                "services.mana_service.get_mana_day_start_timestamp",
                return_value=1_750_000_000,
            ),
        ):
            result = svc.assign_daily_mana(42, TEST_GUILD_ID)

        protection_service.reconcile_guardian.assert_called_once_with(
            42, TEST_GUILD_ID, 1_750_000_000
        )
        assert result["retro_refund"] == 9

    def test_single_assignment_reuses_one_bet_history(self, mana_repo):
        svc = _make_service(mana_repo)
        history = [{"outcome": "lost"}, {"outcome": "won"}]
        svc.gambling_stats_service.bet_repo.get_player_bet_history.return_value = history

        with patch("services.mana_service.get_today_pst", return_value="2025-06-15"):
            svc.assign_daily_mana(43, TEST_GUILD_ID)

        svc.gambling_stats_service.bet_repo.get_player_bet_history.assert_called_once_with(
            43, TEST_GUILD_ID
        )
        svc.gambling_stats_service.calculate_degen_score.assert_called_once_with(
            43, TEST_GUILD_ID, history=history
        )
        svc._get_current_win_streak.assert_called_once_with(43, TEST_GUILD_ID, bet_history=history)


class TestAssignAllDailyMana:
    def test_batches_claims_and_reuses_loaded_players_and_bulk_stats(self):
        today = "2025-06-15"
        players = [
            SimpleNamespace(
                discord_id=1,
                jopacoin_balance=10,
                wins=5,
                losses=5,
                glicko_rating=2000.0,
            ),
            SimpleNamespace(
                discord_id=2,
                jopacoin_balance=-10,
                wins=5,
                losses=5,
                glicko_rating=2000.0,
            ),
            SimpleNamespace(
                discord_id=3,
                jopacoin_balance=20,
                wins=5,
                losses=5,
                glicko_rating=2000.0,
            ),
        ]
        mana_repo = MagicMock()
        mana_repo.get_all_mana.return_value = [
            {"discord_id": 1, "current_land": "Swamp", "assigned_date": today}
        ]
        mana_repo.claim_mana_batch_atomic.return_value = [
            {
                "discord_id": 2,
                "current_land": "Plains",
                "assigned_date": today,
                "white_shield_remaining": 25,
            }
        ]
        player_repo = MagicMock()
        player_repo.normalize_guild_id.return_value = TEST_GUILD_ID
        player_repo.get_all.return_value = players
        player_repo.get_lowest_balances_bulk.return_value = {2: -10, 3: 0}

        gambling_stats = MagicMock()
        gambling_stats.bet_repo.get_current_bet_streaks_bulk.return_value = {
            2: -1,
            3: 1,
        }
        degen_scores = {2: _make_degen(), 3: _make_degen()}
        gambling_stats.calculate_degen_scores_bulk.return_value = degen_scores
        bankruptcy = MagicMock()
        bankruptcy.get_bulk_states.return_value = {
            2: _make_bk_state(),
            3: _make_bk_state(),
        }
        tips = MagicMock()
        tips.get_user_tip_stats.return_value = _make_tip_stats()
        tips.get_user_tip_stats_bulk.return_value = {
            2: _make_tip_stats(),
            3: _make_tip_stats(),
        }
        protection = MagicMock()
        protection.reconcile_guardian.return_value = 5
        service = ManaService(
            mana_repo,
            player_repo,
            gambling_stats,
            bankruptcy,
            tips,
            protection_service=protection,
        )

        with (
            patch("services.mana_service.get_today_pst", return_value=today),
            patch(
                "services.mana_service.get_mana_day_start_timestamp",
                return_value=1_750_000_000,
            ),
            patch(
                "services.mana_service.random.choices",
                side_effect=[["Plains"], ["Forest"]],
            ),
        ):
            assignments, board = service.assign_all_daily_mana_with_board(
                TEST_GUILD_ID, ash_fan_ids={3}
            )

        mana_repo.get_all_mana.assert_called_once_with(TEST_GUILD_ID)
        mana_repo.get_mana.assert_not_called()
        player_repo.get_all.assert_called_once_with(TEST_GUILD_ID)
        player_repo.get_by_id.assert_not_called()
        player_repo.get_lowest_balances_bulk.assert_called_once_with([2, 3], TEST_GUILD_ID)
        player_repo.get_lowest_balance.assert_not_called()
        gambling_stats.bet_repo.get_player_bet_history.assert_not_called()
        gambling_stats.bet_repo.get_current_bet_streaks_bulk.assert_called_once_with(
            [2, 3], TEST_GUILD_ID
        )
        gambling_stats.calculate_degen_scores_bulk.assert_called_once_with([2, 3], TEST_GUILD_ID)
        gambling_stats.calculate_degen_score.assert_not_called()
        gambling_stats.get_player_bet_outcomes.assert_not_called()
        bankruptcy.get_bulk_states.assert_called_once_with([2, 3], TEST_GUILD_ID)
        bankruptcy.get_state.assert_not_called()
        tips.get_user_tip_stats_bulk.assert_called_once_with([2, 3], TEST_GUILD_ID)
        tips.get_user_tip_stats.assert_not_called()
        mana_repo.claim_mana_batch_atomic.assert_called_once_with(
            [(2, "Plains"), (3, "Forest")], TEST_GUILD_ID, today
        )
        protection.reconcile_guardian.assert_called_once_with(2, TEST_GUILD_ID, 1_750_000_000)
        assert assignments == [
            {
                "discord_id": 2,
                "land": "Plains",
                "color": "White",
                "emoji": "🌾",
                "assigned_date": today,
                "retro_refund": 5,
                "guardian_remaining": 20,
                "consumed": False,
            }
        ]
        assert {row["discord_id"] for row in board} == {1, 2}


# =============================================================================
# ManaService — calculate_land_weights (signal dominance checks)
# =============================================================================


class TestLandWeights:
    """Check that strong signals push the expected land to be highest-weighted."""

    def _weights(self, mana_repo, **kwargs) -> dict[str, float]:
        svc = _make_service(mana_repo, **kwargs)
        return svc.calculate_land_weights(1, TEST_GUILD_ID)

    def test_high_balance_boosts_island(self, mana_repo):
        player = _make_player(balance=600)
        w = self._weights(mana_repo, player=player)
        assert w["Island"] > w["Forest"]  # Island should be competitive

    def test_high_degen_boosts_mountain(self, mana_repo):
        degen = _make_degen(total=90, max_leverage_score=22, loss_chase_score=5)
        w = self._weights(mana_repo, degen=degen)
        assert w["Mountain"] == max(w.values())

    def test_bankruptcy_boosts_swamp(self, mana_repo):
        bk = _make_bk_state(penalty_games_remaining=5, last_bankruptcy_at=1000)
        w = self._weights(mana_repo, bk_state=bk, player=_make_player(balance=-200))
        assert w["Swamp"] == max(w.values())

    def test_heavy_tipper_boosts_plains(self, mana_repo):
        tips = _make_tip_stats(total_sent=600, tips_sent_count=25)
        w = self._weights(mana_repo, tip_stats=tips, player=_make_player(balance=50))
        assert w["Plains"] == max(w.values())

    def test_average_player_forest_leads(self, mana_repo):
        # New player, no special signals — Forest baseline (2.0) should dominate
        player = _make_player(balance=10, wins=3, losses=3, glicko_rating=None)
        degen = _make_degen(total=5)
        bk = _make_bk_state()
        tips = _make_tip_stats()
        w = self._weights(mana_repo, player=player, degen=degen, bk_state=bk, tip_stats=tips)
        assert w["Forest"] >= max(w.values()) - 0.01  # Forest wins or ties

    def test_ash_fan_role_boosts_island(self, mana_repo):
        player = _make_player(balance=600)
        player_repo = MagicMock()
        player_repo.normalize_guild_id.side_effect = lambda gid: gid if gid is not None else 0
        player_repo.get_by_id.return_value = player
        player_repo.get_lowest_balance.return_value = None

        gambling_stats = MagicMock()
        gambling_stats.calculate_degen_score.return_value = _make_degen()
        gambling_stats.get_player_bet_outcomes.return_value = []

        bankruptcy_service = MagicMock()
        bankruptcy_service.get_state.return_value = _make_bk_state()

        tip_repo = MagicMock()
        tip_repo.get_user_tip_stats.return_value = _make_tip_stats()

        svc = ManaService(
            mana_repo=mana_repo,
            player_repo=player_repo,
            gambling_stats_service=gambling_stats,
            bankruptcy_service=bankruptcy_service,
            tip_repo=tip_repo,
        )
        svc._get_current_win_streak = MagicMock(return_value=0)

        w_no_ash = svc.calculate_land_weights(1, TEST_GUILD_ID, is_ash_fan=False)
        w_ash = svc.calculate_land_weights(1, TEST_GUILD_ID, is_ash_fan=True)
        assert w_ash["Island"] == w_no_ash["Island"] + 4.0

    def test_losing_streak_boosts_mountain(self, mana_repo):
        degen = _make_degen(total=35)  # Some degen already
        w_no_streak = self._weights(mana_repo, degen=degen, current_streak=0)
        w_streak = self._weights(mana_repo, degen=degen, current_streak=-5)
        assert w_streak["Mountain"] == w_no_streak["Mountain"] + 1.0

    def test_all_weights_positive(self, mana_repo):
        # Worst case — highly degen, bankrupt, no tips
        degen = _make_degen(
            total=120, max_leverage_score=25, loss_chase_score=5,
            bet_size_score=25, negative_loan_bonus=25, bankruptcy_score=15,
            debt_depth_score=20,
        )
        bk = _make_bk_state(penalty_games_remaining=3, last_bankruptcy_at=1)
        player = _make_player(balance=-400)
        w = self._weights(mana_repo, degen=degen, bk_state=bk, player=player,
                          lowest_balance=-400)
        assert all(v > 0 for v in w.values()), f"Non-positive weight found: {w}"
