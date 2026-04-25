"""Tests for the order-book prediction market (feat/predict-orderbook).

Covers schema migration, order-book mechanic (buy/sell sweeps, locked-in fills),
hedging, periodic refresh formula, atomic resolution, cost-basis cancellation,
admin gating, and position math.
"""

from __future__ import annotations

import random

import pytest

from config import (
    PREDICTION_CONTRACT_VALUE,
    PREDICTION_DRIFT_MAX,
    PREDICTION_DRIFT_MIN,
    PREDICTION_LEVELS_PER_SIDE,
    PREDICTION_PRICE_HIGH,
    PREDICTION_PRICE_LOW,
    PREDICTION_SIZE_PER_LEVEL,
    PREDICTION_SPREAD_TICKS,
)
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from services.prediction_service import PredictionService
from tests.conftest import TEST_GUILD_ID

# --------------------------------------------------------------------------- #
# Fixtures
# --------------------------------------------------------------------------- #


@pytest.fixture
def prediction_repo(repo_db_path):
    return PredictionRepository(repo_db_path)


@pytest.fixture
def prediction_service(prediction_repo, player_repository):
    return PredictionService(
        prediction_repo=prediction_repo,
        player_repo=player_repository,
        admin_user_ids=[999],
    )


def _add_player(player_repo: PlayerRepository, discord_id: int, balance: int = 1000):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"user{discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


# --------------------------------------------------------------------------- #
# Schema migration
# --------------------------------------------------------------------------- #


def test_schema_has_orderbook_tables(prediction_repo):
    """The migration creates prediction_levels, prediction_positions, prediction_trades."""
    with prediction_repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
        tables = {row["name"] for row in cursor.fetchall()}
    assert "prediction_levels" in tables
    assert "prediction_positions" in tables
    assert "prediction_trades" in tables


def test_predictions_has_orderbook_columns(prediction_repo):
    with prediction_repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute("PRAGMA table_info(predictions)")
        cols = {row["name"] for row in cursor.fetchall()}
    for col in ("current_price", "initial_fair", "last_refresh_at", "lp_pnl"):
        assert col in cols, f"missing column: {col}"


# --------------------------------------------------------------------------- #
# Market creation: ladder population
# --------------------------------------------------------------------------- #


def test_create_orderbook_prediction_populates_ladder(prediction_service, prediction_repo):
    result = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="Will Pranav win?", initial_fair=50,
    )
    pid = result["prediction_id"]
    assert result["current_price"] == 50

    book = prediction_repo.get_book(pid)
    assert book["current_price"] == 50

    asks = book["yes_asks"]
    bids = book["yes_bids"]
    assert len(asks) == PREDICTION_LEVELS_PER_SIDE
    assert len(bids) == PREDICTION_LEVELS_PER_SIDE

    # Asks sit above fair, bids below. Spread = SPREAD_TICKS each side.
    expected_asks = [50 + PREDICTION_SPREAD_TICKS + k for k in range(PREDICTION_LEVELS_PER_SIDE)]
    expected_bids = [50 - PREDICTION_SPREAD_TICKS - k for k in range(PREDICTION_LEVELS_PER_SIDE)]
    assert [p for p, _ in asks] == sorted(expected_asks)
    assert [p for p, _ in bids] == sorted(expected_bids, reverse=True)

    for _, size in asks + bids:
        assert size == PREDICTION_SIZE_PER_LEVEL


def test_create_rejects_question_too_short(prediction_service):
    with pytest.raises(ValueError, match="at least 5"):
        prediction_service.create_orderbook_prediction(
            guild_id=TEST_GUILD_ID, creator_id=1, question="hi", initial_fair=50,
        )


def test_create_rejects_initial_fair_out_of_range(prediction_service):
    with pytest.raises(ValueError, match="initial_fair"):
        prediction_service.create_orderbook_prediction(
            guild_id=TEST_GUILD_ID, creator_id=1, question="ok question", initial_fair=99,
        )
    with pytest.raises(ValueError, match="initial_fair"):
        prediction_service.create_orderbook_prediction(
            guild_id=TEST_GUILD_ID, creator_id=1, question="ok question", initial_fair=0,
        )


# --------------------------------------------------------------------------- #
# Buy YES: sweep, walk deeper, rejection paths
# --------------------------------------------------------------------------- #


def test_buy_yes_top_of_book_only(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market a?", initial_fair=50,
    )["prediction_id"]
    result = prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="yes", contracts=3,
    )
    assert result["contracts"] == 3
    assert result["total_cost"] == 3 * 51
    assert result["fills"] == [(51, 3)]
    pos = prediction_repo.get_position(pid, 1)
    assert pos["yes_contracts"] == 3
    assert pos["yes_cost_basis_total"] == 153


def test_buy_yes_walks_deeper(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market b?", initial_fair=50,
    )["prediction_id"]
    # Default depth = 3 levels x 5 = 15. Buy 8 -> sweeps level 1 (5 @ 51) + level 2 (3 @ 52)
    result = prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="yes", contracts=8,
    )
    assert result["fills"] == [(51, 5), (52, 3)]
    assert result["total_cost"] == 5 * 51 + 3 * 52


def test_buy_yes_rejects_insufficient_depth(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market c?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match="Insufficient depth"):
        prediction_service.buy_contracts(
            prediction_id=pid, discord_id=1, side="yes", contracts=100,
        )


def test_buy_yes_rejects_insufficient_balance(prediction_service, player_repository):
    _add_player(player_repository, 1, balance=10)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market d?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match="Insufficient balance"):
        prediction_service.buy_contracts(
            prediction_id=pid, discord_id=1, side="yes", contracts=5,
        )


def test_buy_yes_rejects_in_debt(prediction_service, player_repository):
    _add_player(player_repository, 1, balance=-50)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market e?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match="in debt"):
        prediction_service.buy_contracts(
            prediction_id=pid, discord_id=1, side="yes", contracts=1,
        )


def test_buy_no_mirrors_yes_bid(prediction_service, prediction_repo, player_repository):
    """NO ask price = 100 - YES bid (top YES bid is 49 -> NO ask cost 51 per contract)."""
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market f?", initial_fair=50,
    )["prediction_id"]
    result = prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="no", contracts=3,
    )
    assert result["total_cost"] == 3 * 51
    pos = prediction_repo.get_position(pid, 1)
    assert pos["no_contracts"] == 3
    assert pos["no_cost_basis_total"] == 153


# --------------------------------------------------------------------------- #
# Sell flows: proceeds, proportional cost basis, rejection
# --------------------------------------------------------------------------- #


def test_sell_yes_proceeds_at_top_bid(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market g?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=3)
    result = prediction_service.sell_contracts(
        prediction_id=pid, discord_id=1, side="yes", contracts=2,
    )
    assert result["total_proceeds"] == 2 * 49
    pos = prediction_repo.get_position(pid, 1)
    assert pos["yes_contracts"] == 1
    # Cost basis reduced proportionally: 153 - (153 * 2 // 3) = 153 - 102 = 51
    assert pos["yes_cost_basis_total"] == 51


def test_sell_yes_rejected_without_holdings(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market h?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match="hold"):
        prediction_service.sell_contracts(
            prediction_id=pid, discord_id=1, side="yes", contracts=1,
        )


def test_sell_full_position_deletes_row(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market i?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=3)
    prediction_service.sell_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=3)
    pos = prediction_repo.get_position(pid, 1)
    assert pos is None


# --------------------------------------------------------------------------- #
# Hedging: YES and NO held independently
# --------------------------------------------------------------------------- #


def test_hedging_yes_and_no_held_independently(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market j?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=3)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="no", contracts=2)
    pos = prediction_repo.get_position(pid, 1)
    assert pos["yes_contracts"] == 3
    assert pos["no_contracts"] == 2


# --------------------------------------------------------------------------- #
# Refresh: drift bounded, observed_mid pull, ladder reset
# --------------------------------------------------------------------------- #


def test_refresh_keeps_price_in_clamp(prediction_service, prediction_repo, monkeypatch):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market k?", initial_fair=50,
    )["prediction_id"]

    monkeypatch.setattr(random, "randint", lambda lo, hi: PREDICTION_DRIFT_MAX)
    summary = prediction_service.refresh_market(pid)
    assert summary["new_price"] <= PREDICTION_PRICE_HIGH
    assert summary["drift"] == PREDICTION_DRIFT_MAX

    monkeypatch.setattr(random, "randint", lambda lo, hi: PREDICTION_DRIFT_MIN)
    for _ in range(50):
        summary = prediction_service.refresh_market(pid)
    assert summary["new_price"] >= PREDICTION_PRICE_LOW


def test_refresh_uses_observed_mid_when_book_intact(prediction_service, prediction_repo, monkeypatch):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market l?", initial_fair=50,
    )["prediction_id"]
    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)
    summary = prediction_service.refresh_market(pid)
    # observed_mid = (51 + 49) / 2 = 50; drift 0 -> new price 50.
    assert summary["new_price"] == 50


def test_refresh_repopulates_ladder(prediction_service, prediction_repo, player_repository, monkeypatch):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market m?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    pre = prediction_repo.get_book(pid)
    assert pre["yes_asks"][0][0] == 52  # top ask moved up after sweep

    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)
    prediction_service.refresh_market(pid)
    post = prediction_repo.get_book(pid)
    for _, size in post["yes_asks"] + post["yes_bids"]:
        assert size == PREDICTION_SIZE_PER_LEVEL


def test_get_markets_due_for_refresh(prediction_service, prediction_repo):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market n?", initial_fair=50,
    )["prediction_id"]
    with prediction_repo.connection() as conn:
        conn.execute("UPDATE predictions SET last_refresh_at = 0 WHERE prediction_id = ?", (pid,))
    due = prediction_service.get_markets_due_for_refresh(now_ts=10**12)
    assert any(m["prediction_id"] == pid for m in due)


# --------------------------------------------------------------------------- #
# Resolution: pays winners, lp_pnl correct
# --------------------------------------------------------------------------- #


def test_resolve_yes_pays_yes_holders(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1, balance=1000)
    _add_player(player_repository, 2, balance=1000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market o?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=2, side="no", contracts=4)
    bal_1_pre = player_repository.get_balance(1, TEST_GUILD_ID)
    bal_2_pre = player_repository.get_balance(2, TEST_GUILD_ID)

    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")

    assert result["total_payout"] == 5 * PREDICTION_CONTRACT_VALUE
    bal_1_post = player_repository.get_balance(1, TEST_GUILD_ID)
    bal_2_post = player_repository.get_balance(2, TEST_GUILD_ID)
    assert bal_1_post - bal_1_pre == 500
    assert bal_2_post == bal_2_pre


def test_resolve_lp_pnl_balances_collected_minus_paid(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market p?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")
    assert result["lp_pnl"] == 255 - 500


def test_resolve_marks_status_and_blocks_further_trades(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market q?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=2)
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")
    with pytest.raises(ValueError, match="not open"):
        prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=1)


# --------------------------------------------------------------------------- #
# Cancel: cost-basis refund
# --------------------------------------------------------------------------- #


def test_cancel_refunds_cost_basis(prediction_service, player_repository):
    _add_player(player_repository, 1, balance=1000)
    _add_player(player_repository, 2, balance=1000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market r?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=3)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=2, side="no", contracts=2)

    bal_1_pre = player_repository.get_balance(1, TEST_GUILD_ID)
    bal_2_pre = player_repository.get_balance(2, TEST_GUILD_ID)
    result = prediction_service.cancel_orderbook(pid)
    bal_1_post = player_repository.get_balance(1, TEST_GUILD_ID)
    bal_2_post = player_repository.get_balance(2, TEST_GUILD_ID)
    assert bal_1_post - bal_1_pre == 153
    assert bal_2_post - bal_2_pre == 102
    assert result["total_refunded"] == 255


def test_cancel_with_round_trip_keeps_realized_pnl(prediction_service, player_repository):
    _add_player(player_repository, 1, balance=1000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market s?", initial_fair=50,
    )["prediction_id"]
    # Buy 5 @ 51 -> cost 255. Sell 2 @ 49 -> proceeds 98.
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    prediction_service.sell_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=2)
    pre_cancel_balance = 1000 - 255 + 98  # 843
    assert prediction_service.player_repo.get_balance(1, TEST_GUILD_ID) == pre_cancel_balance

    prediction_service.cancel_orderbook(pid)
    # Cancel refunds remaining cost basis only (153). Spread on round-trip stays with LP.
    assert prediction_service.player_repo.get_balance(1, TEST_GUILD_ID) == pre_cancel_balance + 153


# --------------------------------------------------------------------------- #
# /predict mine / position queries
# --------------------------------------------------------------------------- #


def test_get_user_open_positions_returns_open_only(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid_open = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="open one", initial_fair=50,
    )["prediction_id"]
    pid_other = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="another one", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid_open, discord_id=1, side="yes", contracts=2)
    prediction_service.buy_contracts(prediction_id=pid_other, discord_id=1, side="no", contracts=1)

    positions = prediction_service.get_user_open_positions(1, TEST_GUILD_ID)
    assert len(positions) == 2

    prediction_service.resolve_orderbook(prediction_id=pid_other, outcome="no")
    positions = prediction_service.get_user_open_positions(1, TEST_GUILD_ID)
    assert len(positions) == 1
    assert positions[0]["prediction_id"] == pid_open


# --------------------------------------------------------------------------- #
# Position mark helper
# --------------------------------------------------------------------------- #


def test_position_mark_helper():
    book = {"yes_asks": [(55, 5), (56, 5)], "yes_bids": [(45, 5), (44, 5)]}
    assert PredictionService.position_mark(book, "yes") == 45
    assert PredictionService.position_mark(book, "no") == 100 - 55  # = 45


def test_position_mark_falls_back_when_side_empty():
    book = {"yes_asks": [], "yes_bids": [(45, 5)]}
    assert PredictionService.position_mark(book, "no") is None
