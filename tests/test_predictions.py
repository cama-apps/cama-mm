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
    PREDICTION_FADE_TICKS,
    PREDICTION_LEVELS_PER_SIDE,
    PREDICTION_PRICE_HIGH,
    PREDICTION_PRICE_LOW,
    PREDICTION_REFRESH_SIZE_PER_LEVEL,
    PREDICTION_REFRESH_SPREAD_TICKS,
    PREDICTION_SIZE_PER_LEVEL,
    PREDICTION_SPREAD_TICKS,
)
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository, _quote_total
from services.prediction_service import PredictionService
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

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


def test_position_transfer_rejects_non_open_market(prediction_repo):
    """Contract transfers must not mutate locked/resolved markets."""
    market_id = prediction_repo.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID,
        creator_id=999,
        question="Will this market lock?",
        initial_fair=50,
    )
    with prediction_repo.connection() as conn:
        conn.execute(
            """
            INSERT INTO prediction_positions
                (prediction_id, discord_id, yes_contracts, yes_cost_basis_total)
            VALUES (?, ?, 8, 24)
            """,
            (market_id, 1001),
        )
    prediction_repo.update_prediction_status(market_id, "locked")

    result = prediction_repo.transfer_position_contracts(
        market_id, 1001, 1002, "yes", 4
    )

    assert result is None
    victim_position = prediction_repo.get_position(market_id, 1001)
    attacker_position = prediction_repo.get_position(market_id, 1002)
    assert victim_position["yes_contracts"] == 8
    assert victim_position["yes_cost_basis_total"] == 24
    assert attacker_position is None


def test_predictions_has_orderbook_columns(prediction_repo):
    with prediction_repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute("PRAGMA table_info(predictions)")
        cols = {row["name"] for row in cursor.fetchall()}
    for col in ("current_price", "initial_fair", "last_refresh_at", "lp_pnl"):
        assert col in cols, f"missing column: {col}"


def test_prediction_trades_record_last_fill_price(prediction_repo):
    with prediction_repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute("PRAGMA table_info(prediction_trades)")
        cols = {row["name"] for row in cursor.fetchall()}
    assert "last_fill_price" in cols


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
    expected_asks = [52, 53, 54]
    expected_bids = [48, 47, 46]
    assert [p for p, _ in asks] == sorted(expected_asks)
    assert [p for p, _ in bids] == sorted(expected_bids, reverse=True)

    for _, size in asks + bids:
        assert size == PREDICTION_SIZE_PER_LEVEL


@pytest.mark.parametrize("initial_fair", [PREDICTION_PRICE_LOW, PREDICTION_PRICE_HIGH])
def test_create_at_fair_bounds_keeps_full_ladder(
    prediction_service, prediction_repo, initial_fair,
):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID,
        creator_id=1,
        question="Will the boundary ladder stay full?",
        initial_fair=initial_fair,
    )["prediction_id"]

    book = prediction_repo.get_book(pid)

    assert len(book["yes_asks"]) == PREDICTION_LEVELS_PER_SIDE
    assert len(book["yes_bids"]) == PREDICTION_LEVELS_PER_SIDE


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
    expected_cost = _quote_total(3 * 52, "buy")
    assert result["contracts"] == 3
    assert result["total_cost"] == expected_cost
    assert result["fills"] == [(52, 3)]
    pos = prediction_repo.get_position(pid, 1)
    assert pos["yes_contracts"] == 3
    assert pos["yes_cost_basis_total"] == expected_cost


def test_buy_yes_walks_deeper(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market b?", initial_fair=50,
    )["prediction_id"]
    # Depth per level = PREDICTION_SIZE_PER_LEVEL. Buy size_per_level + 3 sweeps the
    # whole top level then takes 3 from the next.
    take_l1 = PREDICTION_SIZE_PER_LEVEL
    take_l2 = 3
    result = prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="yes", contracts=take_l1 + take_l2,
    )
    assert result["fills"] == [(52, take_l1), (53, take_l2)]
    assert result["total_cost"] == _quote_total(take_l1 * 52 + take_l2 * 53, "buy")


def test_buy_yes_rejects_insufficient_depth(prediction_service, player_repository):
    _add_player(player_repository, 1, balance=1_000_000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market c?", initial_fair=50,
    )["prediction_id"]
    full_depth = PREDICTION_LEVELS_PER_SIDE * PREDICTION_SIZE_PER_LEVEL
    with pytest.raises(ValueError, match="Insufficient depth"):
        prediction_service.buy_contracts(
            prediction_id=pid, discord_id=1, side="yes", contracts=full_depth + 1,
        )


def test_refresh_uses_widened_drift_bounds(
    prediction_service, prediction_repo, monkeypatch,
):
    """Daily refresh random walk uses the configured inclusive drift range."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market drift?", initial_fair=50,
    )["prediction_id"]
    bounds = {}

    def capture_randint(lo, hi):
        bounds["lo"] = lo
        bounds["hi"] = hi
        return hi

    monkeypatch.setattr(random, "randint", capture_randint)

    summary = prediction_service.refresh_market(pid)

    assert bounds == {"lo": -3, "hi": 3}
    assert summary["drift"] == 3


def test_buy_yes_rejects_insufficient_balance(prediction_service, player_repository):
    _add_player(player_repository, 1, balance=10)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market d?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match="Insufficient balance"):
        prediction_service.buy_contracts(
            prediction_id=pid, discord_id=1, side="yes", contracts=5,
        )


def test_buy_rejects_negative_or_zero_contracts(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market neg?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match="positive"):
        prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=0)
    with pytest.raises(ValueError, match="positive"):
        prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=-5)


def test_buy_rejects_above_per_trade_cap(prediction_service, player_repository):
    """Per-trade cap rejects more than PREDICTION_MAX_CONTRACTS_PER_TRADE in one go."""
    from config import PREDICTION_MAX_CONTRACTS_PER_TRADE
    _add_player(player_repository, 1, balance=1_000_000)  # plenty of jopa
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market cap?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match=f"capped at {PREDICTION_MAX_CONTRACTS_PER_TRADE}"):
        prediction_service.buy_contracts(
            prediction_id=pid, discord_id=1, side="yes",
            contracts=PREDICTION_MAX_CONTRACTS_PER_TRADE + 1,
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
    """NO ask price = 100 - YES bid (top YES bid is 48 -> NO ask cost 52 per contract)."""
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market f?", initial_fair=50,
    )["prediction_id"]
    result = prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="no", contracts=3,
    )
    expected_cost = _quote_total(3 * (100 - 48), "buy")
    assert result["total_cost"] == expected_cost
    pos = prediction_repo.get_position(pid, 1)
    assert pos["no_contracts"] == 3
    assert pos["no_cost_basis_total"] == expected_cost


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
    assert result["total_proceeds"] == _quote_total(2 * 48, "sell")
    pos = prediction_repo.get_position(pid, 1)
    assert pos["yes_contracts"] == 1
    # Cost basis reduced proportionally: original_basis * 2 // 3 worth removed.
    original_basis = _quote_total(3 * 52, "buy")
    expected_remaining = original_basis - (original_basis * 2) // 3
    assert pos["yes_cost_basis_total"] == expected_remaining


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
    # observed_mid = (52 + 48) / 2 = 50; drift 0 -> new price 50.
    assert summary["new_price"] == 50


def test_refresh_prunes_legacy_inner_quotes_but_keeps_crossing_levels(
    prediction_service, prediction_repo, monkeypatch,
):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID,
        creator_id=1,
        question="Will the live book widen safely?",
        initial_fair=50,
    )["prediction_id"]
    prediction_repo.replace_levels(
        pid,
        levels=[
            ("yes_ask", 49, 5),
            ("yes_ask", 51, 5),
            ("yes_ask", 52, 5),
            ("yes_bid", 51, 5),
            ("yes_bid", 49, 5),
            ("yes_bid", 48, 5),
        ],
    )
    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)

    prediction_service.refresh_market(pid)

    post = dict(post_helper(prediction_repo.get_book(pid)))
    assert ("yes_ask", 51) not in post
    assert ("yes_bid", 49) not in post
    assert post[("yes_ask", 52)] == 5
    assert post[("yes_bid", 48)] == 5
    assert post[("yes_ask", 49)] == 5
    assert post[("yes_bid", 51)] == 5


def test_refresh_layers_size_onto_existing_levels(
    prediction_service, prediction_repo, player_repository, monkeypatch,
):
    """Daily refresh tops up the existing ladder where it overlaps.

    Initial seed at fair=50 with spread=2: asks 52..54, bids 48..46, each at
    PREDICTION_SIZE_PER_LEVEL. Refresh uses a wider spread (4) and smaller
    per-level size, so it overlaps the seed only at 54 / 46 and adds new outer
    levels at 55,56 / 45,44; the nearer seed levels (53 / 47) are left alone.
    """
    _add_player(player_repository, 1, balance=1_000_000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market m?", initial_fair=50,
    )["prediction_id"]
    # Drain the top ask at 52 so we can confirm a consumed top-of-book level is
    # NOT resurrected by the refresh.
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="yes", contracts=PREDICTION_SIZE_PER_LEVEL,
    )
    assert ("yes_ask", 52) not in dict(post_helper(prediction_repo.get_book(pid)))

    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)
    summary = prediction_service.refresh_market(pid)
    # Draining the top ask moves top-of-book to 53, so observed_mid = (53+48)/2
    # = 50.5 and (zero drift) the refresh reposts around fair 50. Pin new_price
    # so the asserted ladder can't silently drift if the rounding/fade rules change.
    assert summary["new_price"] == 50
    post = dict(post_helper(prediction_repo.get_book(pid)))

    # Refresh ladder positions (spread 4) are 54, 55, 56 (asks) and 46, 45, 44 (bids).
    # 52 (consumed top seed) is NOT in the refresh ladder so stays absent.
    # 53 / 47 are seed-only (refresh no longer reaches them); 54 / 46 overlap and
    # gain a refresh layer; 55,56 / 45,44 are new refresh-only levels.
    assert ("yes_ask", 52) not in post
    assert post[("yes_ask", 53)] == PREDICTION_SIZE_PER_LEVEL
    assert post[("yes_ask", 54)] == PREDICTION_SIZE_PER_LEVEL + PREDICTION_REFRESH_SIZE_PER_LEVEL
    assert post[("yes_ask", 55)] == PREDICTION_REFRESH_SIZE_PER_LEVEL
    assert post[("yes_ask", 56)] == PREDICTION_REFRESH_SIZE_PER_LEVEL
    # Seed bids at 48, 47 untouched (not in refresh ladder).
    assert post[("yes_bid", 48)] == PREDICTION_SIZE_PER_LEVEL
    assert post[("yes_bid", 47)] == PREDICTION_SIZE_PER_LEVEL
    assert post[("yes_bid", 46)] == PREDICTION_SIZE_PER_LEVEL + PREDICTION_REFRESH_SIZE_PER_LEVEL
    assert post[("yes_bid", 45)] == PREDICTION_REFRESH_SIZE_PER_LEVEL
    assert post[("yes_bid", 44)] == PREDICTION_REFRESH_SIZE_PER_LEVEL


def test_refresh_quiet_market_accumulates_depth(
    prediction_service, prediction_repo, monkeypatch,
):
    """No trades + multiple refreshes at same fair = depth grows at the
    refresh-ladder positions only (fair±4 overlap plus the fair±5/±6 outer
    levels). Initial-seed-only positions (fair±2/±3) stay flat."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market mm?", initial_fair=50,
    )["prediction_id"]
    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)
    refreshes = 3
    for _ in range(refreshes):
        prediction_service.refresh_market(pid)
    post = dict(post_helper(prediction_repo.get_book(pid)))
    # Seed-only positions (no refresh overlap at spread 4) stay flat.
    for price in (52, 53):
        assert post[("yes_ask", price)] == PREDICTION_SIZE_PER_LEVEL
    for price in (48, 47):
        assert post[("yes_bid", price)] == PREDICTION_SIZE_PER_LEVEL
    # Overlap at fair±4 accumulates seed + one refresh layer per pass.
    overlap = PREDICTION_SIZE_PER_LEVEL + refreshes * PREDICTION_REFRESH_SIZE_PER_LEVEL
    assert post[("yes_ask", 54)] == overlap
    assert post[("yes_bid", 46)] == overlap
    # Refresh-only outer levels accumulate one layer per pass.
    outer = refreshes * PREDICTION_REFRESH_SIZE_PER_LEVEL
    for price in (55, 56):
        assert post[("yes_ask", price)] == outer
    for price in (45, 44):
        assert post[("yes_bid", price)] == outer


def test_refresh_fades_farther_up_when_asks_consumed(
    prediction_service, prediction_repo, player_repository, monkeypatch,
):
    """Heavy YES buying drains all asks; refresh fades from the last lifted ask."""
    _add_player(player_repository, 1, balance=1_000_000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market mf?", initial_fair=50,
    )["prediction_id"]
    # Drain every ask across all levels.
    full_depth = PREDICTION_LEVELS_PER_SIDE * PREDICTION_SIZE_PER_LEVEL
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="yes", contracts=full_depth,
    )
    book = prediction_repo.get_book(pid)
    assert book["yes_asks"] == []

    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)
    summary = prediction_service.refresh_market(pid)
    # observed_mid = last lifted ask (54) + FADE_TICKS, drift 0.
    assert summary["new_price"] == 54 + PREDICTION_FADE_TICKS


def test_refresh_fades_farther_down_when_bids_consumed(
    prediction_service, prediction_repo, player_repository, monkeypatch,
):
    """Heavy YES selling (or NO buying) drains all bids; refresh fades from the last hit bid."""
    _add_player(player_repository, 1, balance=1_000_000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market mfd?", initial_fair=50,
    )["prediction_id"]
    full_depth = PREDICTION_LEVELS_PER_SIDE * PREDICTION_SIZE_PER_LEVEL
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="no", contracts=full_depth,
    )
    book = prediction_repo.get_book(pid)
    assert book["yes_bids"] == []

    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)
    summary = prediction_service.refresh_market(pid)
    # observed_mid = last hit bid (46) - FADE_TICKS, drift 0.
    assert summary["new_price"] == 46 - PREDICTION_FADE_TICKS


def test_refresh_uses_terminal_fill_midpoint_when_both_sides_consumed(
    prediction_service, prediction_repo, player_repository, monkeypatch,
):
    """If both sides are gone, refresh centers between the terminal fills."""
    _add_player(player_repository, 1, balance=1_000_000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market both?", initial_fair=50,
    )["prediction_id"]
    prediction_repo.replace_levels(
        pid,
        levels=[
            ("yes_ask", 70, 5),
            ("yes_ask", 80, 5),
            ("yes_bid", 40, 5),
            ("yes_bid", 30, 5),
        ],
    )
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="yes", contracts=10,
    )
    prediction_service.buy_contracts(
        prediction_id=pid, discord_id=1, side="no", contracts=10,
    )
    book = prediction_repo.get_book(pid)
    assert book["yes_asks"] == []
    assert book["yes_bids"] == []

    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)
    summary = prediction_service.refresh_market(pid)
    # observed_mid = midpoint(last lifted ask 80, last hit bid 30), drift 0.
    assert summary["new_price"] == 55


def post_helper(book):
    """Flatten a book dict into ((side, price) -> size) entries for assertions."""
    for price, size in book["yes_asks"]:
        yield (("yes_ask", price), size)
    for price, size in book["yes_bids"]:
        yield (("yes_bid", price), size)


def test_refresh_locked_level_at_top_bid_is_kept(prediction_service, prediction_repo):
    """An old ask at exactly the new top bid is locked (not crossed) — keep it."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market mx?", initial_fair=50,
    )["prediction_id"]
    # Inject a stale ask at price 51 explicitly so we can target the boundary.
    prediction_repo.replace_levels(pid, levels=[("yes_ask", 51, 5), ("yes_bid", 49, 5)])

    # Apply a refresh whose new top bid lands at exactly 51 → locked, not crossed.
    # Apply an explicit one-level ladder with ask 52 and bid 51.
    prediction_repo.apply_refresh(
        pid,
        new_price=52,
        levels=[("yes_ask", 52, 3), ("yes_bid", 51, 3)],
        now_ts=10**9,
    )
    post = dict(post_helper(prediction_repo.get_book(pid)))
    # Old ask at 51 is at-the-bid (locked, not crossed) — must survive.
    assert post[("yes_ask", 51)] == 5
    # New ladder positions added.
    assert post[("yes_ask", 52)] == 3
    assert post[("yes_bid", 51)] == 3


def test_refresh_preserves_crossing_ask_for_arb(prediction_repo, prediction_service):
    """Crossing leftovers from prior flow are preserved on refresh as arb pockets.

    An old ask below the new top bid is a true cross. The refresh layers a fresh
    ladder around the new fair but does NOT cancel the stale crossing ask — the
    next trader to spot it lifts a free position.
    """
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market mxc?", initial_fair=50,
    )["prediction_id"]
    prediction_repo.replace_levels(
        pid,
        levels=[
            ("yes_ask", 48, 5),
            ("yes_ask", 50, 5),
            ("yes_bid", 47, 5),
        ],
    )
    levels_around_52 = PredictionService._build_initial_levels(52)
    prediction_repo.apply_refresh(pid, new_price=52, levels=levels_around_52, now_ts=10**9)
    post = dict(post_helper(prediction_repo.get_book(pid)))
    # Stale crossing ask survives — that's the arb opportunity. New top bid is 50.
    assert post[("yes_ask", 48)] == 5
    # The pre-existing ask at 50 also survives (would have been deleted before the
    # change since 50 < 52 and crosses the new mid).
    assert post[("yes_ask", 50)] == 5


def test_set_fair_manual_changes_price_and_layers_ladder(
    prediction_service, prediction_repo,
):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market sf?", initial_fair=50,
    )["prediction_id"]
    result = prediction_service.set_fair_manual(prediction_id=pid, new_price=60)
    assert result["old_price"] == 50
    assert result["new_price"] == 60
    pred = prediction_repo.get_prediction(pid)
    assert pred["current_price"] == 60
    book = prediction_repo.get_book(pid)
    ask_prices = sorted(p for p, _ in book["yes_asks"])
    bid_prices = sorted((p for p, _ in book["yes_bids"]), reverse=True)
    # New ladder layers at 62,63,64 / 58,57,56 around the new fair.
    assert 62 in ask_prices and 63 in ask_prices and 64 in ask_prices
    assert 58 in bid_prices and 57 in bid_prices and 56 in bid_prices
    # Old asks/bids from the previous ladder are preserved — including the ones
    # that now cross the new bid/ask. Whoever spots a stale price arbs it.
    assert 52 in ask_prices and 53 in ask_prices and 54 in ask_prices
    assert 46 in bid_prices and 47 in bid_prices and 48 in bid_prices


def test_refresh_layers_with_refresh_params_not_initial(
    prediction_service, prediction_repo, monkeypatch,
):
    """Daily refresh uses thinner/wider params than the initial seed.

    Initial: PREDICTION_LEVELS_PER_SIDE × PREDICTION_SIZE_PER_LEVEL contracts at
    spread 2 (fair±2/±3/±4). Refresh: PREDICTION_REFRESH_LEVELS_PER_SIDE ×
    PREDICTION_REFRESH_SIZE_PER_LEVEL at spread 4 (fair±4/±5/±6), so it overlaps
    the seed only at fair±4 and adds new refresh-only levels at fair±5/±6.
    """
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market lr?", initial_fair=50,
    )["prediction_id"]
    monkeypatch.setattr(random, "randint", lambda lo, hi: 0)  # zero drift
    prediction_service.refresh_market(pid)

    book = prediction_repo.get_book(pid)
    asks_by_price = dict(book["yes_asks"])
    bids_by_price = dict(book["yes_bids"])

    seed = PREDICTION_SIZE_PER_LEVEL
    layered = seed + PREDICTION_REFRESH_SIZE_PER_LEVEL
    refresh_only = PREDICTION_REFRESH_SIZE_PER_LEVEL

    # Refresh (spread 4) overlaps the seed only at fair±4; fair±2/±3 stay seed-only
    # and fair±5/±6 are new refresh-only levels.
    assert asks_by_price[52] == seed
    assert asks_by_price[53] == seed
    assert asks_by_price[54] == layered
    assert asks_by_price[55] == refresh_only
    assert asks_by_price[56] == refresh_only
    assert 57 not in asks_by_price           # refresh goes 3 deep: 54, 55, 56

    assert bids_by_price[48] == seed
    assert bids_by_price[47] == seed
    assert bids_by_price[46] == layered
    assert bids_by_price[45] == refresh_only
    assert bids_by_price[44] == refresh_only
    assert 43 not in bids_by_price

    # Guard the "tops up the existing ladder" invariant: the refresh's innermost
    # offset must stay within the seed's reach so the two ladders overlap on at
    # least one price. Raising PREDICTION_REFRESH_SPREAD_TICKS past the seed's
    # outer edge would silently stop the refresh reinforcing the seed.
    seed_outer_offset = PREDICTION_SPREAD_TICKS + PREDICTION_LEVELS_PER_SIDE - 1
    assert seed_outer_offset >= PREDICTION_REFRESH_SPREAD_TICKS


def test_apply_refresh_stamps_prev_price(
    prediction_service, prediction_repo, monkeypatch,
):
    """Each refresh records the OLD current_price as prev_price for the digest delta."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market pp?", initial_fair=50,
    )["prediction_id"]
    pred = prediction_repo.get_prediction(pid)
    assert pred["current_price"] == 50
    assert pred["prev_price"] is None  # never refreshed yet

    monkeypatch.setattr(random, "randint", lambda lo, hi: 2)  # drift +2 → new price 52
    prediction_service.refresh_market(pid)
    pred = prediction_repo.get_prediction(pid)
    assert pred["current_price"] == 52
    assert pred["prev_price"] == 50  # OLD price was stamped as prev_price


def test_set_fair_manual_stamps_prev_price(prediction_service, prediction_repo):
    """Admin override also leaves a prev_price footprint so the digest reflects the move."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market sfp?", initial_fair=50,
    )["prediction_id"]
    prediction_service.set_fair_manual(prediction_id=pid, new_price=80)
    pred = prediction_repo.get_prediction(pid)
    assert pred["current_price"] == 80
    assert pred["prev_price"] == 50


def test_set_fair_manual_rejects_out_of_range(prediction_service):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market sfo?", initial_fair=50,
    )["prediction_id"]
    with pytest.raises(ValueError, match="new_price"):
        prediction_service.set_fair_manual(prediction_id=pid, new_price=99)
    with pytest.raises(ValueError, match="new_price"):
        prediction_service.set_fair_manual(prediction_id=pid, new_price=2)


def test_set_fair_manual_rejects_resolved_market(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market sfr?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=1)
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")
    with pytest.raises(ValueError, match="resolved"):
        prediction_service.set_fair_manual(prediction_id=pid, new_price=60)


def test_refresh_locked_bid_at_top_ask_is_kept(prediction_service, prediction_repo):
    """Symmetric: an old bid at exactly the new top ask is locked, not crossed."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market mxd?", initial_fair=50,
    )["prediction_id"]
    prediction_repo.replace_levels(pid, levels=[("yes_ask", 51, 5), ("yes_bid", 49, 5)])

    # Apply an explicit one-level ladder with ask 49 and bid 48.
    prediction_repo.apply_refresh(
        pid,
        new_price=48,
        levels=[("yes_ask", 49, 3), ("yes_bid", 48, 3)],
        now_ts=10**9,
    )
    post = dict(post_helper(prediction_repo.get_book(pid)))
    # Old bid at 49 is at-the-ask (locked, not crossed) — must survive.
    assert post[("yes_bid", 49)] == 5
    assert post[("yes_bid", 48)] == 3
    assert post[("yes_ask", 49)] == 3


def test_set_fair_with_open_position_preserves_user_holdings(
    prediction_service, prediction_repo, player_repository,
):
    """An admin set_fair must not touch user contracts or cost basis."""
    _add_player(player_repository, 1, balance=1000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market sfp?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=3)
    pos_before = prediction_repo.get_position(pid, 1)

    prediction_service.set_fair_manual(prediction_id=pid, new_price=80)
    pos_after = prediction_repo.get_position(pid, 1)

    assert pos_after["yes_contracts"] == pos_before["yes_contracts"]
    assert pos_after["yes_cost_basis_total"] == pos_before["yes_cost_basis_total"]
    # Mark moves: user's position is now worth more on paper because fair jumped.
    book = prediction_repo.get_book(pid)
    new_top_bid = book["yes_bids"][0][0]
    assert new_top_bid > 50  # mark moved up


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
    assert bal_1_post - bal_1_pre == 5 * PREDICTION_CONTRACT_VALUE
    assert bal_2_post == bal_2_pre


def test_resolve_lp_pnl_balances_collected_minus_paid(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market p?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")
    expected_collected = _quote_total(5 * 52, "buy")
    expected_paid = 5 * PREDICTION_CONTRACT_VALUE
    assert result["lp_pnl"] == expected_collected - expected_paid


def test_resolve_marks_status_and_blocks_further_trades(prediction_service, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market q?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=2)
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")
    with pytest.raises(ValueError, match="not open"):
        prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=1)


def test_resolve_records_resolved_by(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market qq?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=1)
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes", resolved_by=12345)
    pred = prediction_repo.get_prediction(pid)
    assert pred["resolved_by"] == 12345


def test_apply_refresh_skips_resolved_market(prediction_service, prediction_repo, player_repository):
    _add_player(player_repository, 1)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market qr?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=1)
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")
    # Refresh worker may still race against a just-resolved market; apply_refresh
    # should no-op rather than overwrite the terminal state.
    prediction_repo.apply_refresh(
        pid,
        new_price=70,
        levels=[("yes_ask", 71, 5), ("yes_bid", 69, 5)],
        now_ts=10**12,
    )
    pred = prediction_repo.get_prediction(pid)
    assert pred["status"] == "resolved"
    assert pred["current_price"] == 50  # unchanged
    book = prediction_repo.get_book(pid)
    assert book["yes_asks"] == []
    assert book["yes_bids"] == []


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
    expected_refund_1 = _quote_total(3 * 52, "buy")
    expected_refund_2 = _quote_total(2 * 52, "buy")  # 100-48 = 52
    assert bal_1_post - bal_1_pre == expected_refund_1
    assert bal_2_post - bal_2_pre == expected_refund_2
    assert result["total_refunded"] == expected_refund_1 + expected_refund_2


def test_cancel_with_round_trip_keeps_realized_pnl(prediction_service, player_repository):
    _add_player(player_repository, 1, balance=1000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="market s?", initial_fair=50,
    )["prediction_id"]
    buy_cost = _quote_total(5 * 52, "buy")
    sell_proceeds = _quote_total(2 * 48, "sell")
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    prediction_service.sell_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=2)
    pre_cancel_balance = 1000 - buy_cost + sell_proceeds
    assert prediction_service.player_repo.get_balance(1, TEST_GUILD_ID) == pre_cancel_balance

    prediction_service.cancel_orderbook(pid)
    # Cancel refunds remaining cost basis: original buy cost minus the proportional
    # basis reduction from the sell. Spread on round-trip stays with LP.
    remaining_basis = buy_cost - (buy_cost * 2) // 5
    assert (
        prediction_service.player_repo.get_balance(1, TEST_GUILD_ID)
        == pre_cancel_balance + remaining_basis
    )


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


# --------------------------------------------------------------------------- #
# Cog: /predict force_refresh and /predict refresh_status admin commands
# --------------------------------------------------------------------------- #


from types import SimpleNamespace  # noqa: E402
from unittest.mock import AsyncMock, MagicMock  # noqa: E402

import discord  # noqa: E402

from commands.predictions import PersistentMarketView, PredictionCommands  # noqa: E402
from utils.thread_safety import THREAD_AUTO_ARCHIVE_MINUTES  # noqa: E402


class _FakeFollowup:
    def __init__(self):
        self.messages: list[dict] = []

    async def send(self, content=None, embed=None, ephemeral=None, allowed_mentions=None, view=None):
        self.messages.append({"content": content, "embed": embed, "ephemeral": ephemeral})


class _FakeResponse:
    def __init__(self):
        self.messages: list[dict] = []
        self._done = False

    def is_done(self):
        return self._done

    async def send_message(self, content=None, embed=None, ephemeral=None):
        self._done = True
        self.messages.append({"content": content, "embed": embed, "ephemeral": ephemeral})

    async def defer(self, ephemeral=False, thinking=False):
        self._done = True


class _FakeThread:
    def __init__(self, *, archived: bool = False, locked: bool = False):
        self.sent: list[str] = []
        self.edits: list[dict] = []
        self.archived = archived
        self.locked = locked

    async def send(self, content=None, embed=None):
        self.sent.append(content if content is not None else "<embed>")

    async def edit(self, **kwargs):
        self.edits.append(kwargs)
        for name, value in kwargs.items():
            setattr(self, name, value)


class _FakeInteraction:
    def __init__(self, user_id: int = 1, guild_id: int = TEST_GUILD_ID):
        self.user = SimpleNamespace(id=user_id)
        self.guild = SimpleNamespace(id=guild_id)
        self.channel = SimpleNamespace(id=11)
        self.response = _FakeResponse()
        self.followup = _FakeFollowup()


def _make_cog(prediction_service, thread=None) -> PredictionCommands:
    bot = SimpleNamespace()
    bot.prediction_service = prediction_service
    bot.player_service = SimpleNamespace()
    bot.get_channel = lambda cid: thread

    async def _fetch(cid):
        return thread

    bot.fetch_channel = _fetch
    return PredictionCommands(bot, prediction_service, bot.player_service)


@pytest.fixture
def patched_cog_helpers(monkeypatch):
    """Bypass require_gamba_channel/safe_defer/safe_followup so the cog body runs."""
    from commands import predictions as pmod

    async def _ok(_):
        return True

    monkeypatch.setattr(pmod, "require_gamba_channel", _ok)
    monkeypatch.setattr(pmod, "safe_defer", AsyncMock(return_value=True))

    async def _fwup(interaction, content=None, embed=None, ephemeral=None):
        await interaction.followup.send(content=content, embed=embed, ephemeral=ephemeral)

    monkeypatch.setattr(pmod, "safe_followup", _fwup)
    return monkeypatch


async def test_force_refresh_admin_only(prediction_service, player_repository, patched_cog_helpers):
    """Non-admin gets a refusal reply; market is not refreshed."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: False)

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Will it work?", initial_fair=50,
    )["prediction_id"]
    before = prediction_service.prediction_repo.get_prediction(pid)

    cog = _make_cog(prediction_service)
    interaction = _FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    await cog.force_refresh.callback(cog, interaction, pid)

    after = prediction_service.prediction_repo.get_prediction(pid)
    assert before["last_refresh_at"] == after["last_refresh_at"]
    assert any("admin" in (m.get("content") or "").lower() for m in interaction.response.messages)
    assert interaction.followup.messages == []


async def test_force_refresh_announces_and_posts_to_thread(
    prediction_service, player_repository, patched_cog_helpers
):
    """Admin call announces the new price in the followup AND in the market thread."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Will it work?", initial_fair=50,
    )["prediction_id"]
    with prediction_service.prediction_repo.connection() as conn:
        conn.execute(
            "UPDATE predictions SET thread_id = ? WHERE prediction_id = ?", (12345, pid)
        )

    thread = _FakeThread()
    cog = _make_cog(prediction_service, thread=thread)
    # refresh_market_embed needs an embed_message_id; bypass to keep this test focused.
    cog.refresh_market_embed = AsyncMock()

    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    await cog.force_refresh.callback(cog, interaction, pid)

    fwup_contents = " ".join((m.get("content") or "") for m in interaction.followup.messages)
    assert "manually refreshed" in fwup_contents.lower()
    assert any("manually refreshed" in s.lower() for s in thread.sent)


async def test_force_refresh_skipped_when_resolved(
    prediction_service, player_repository, patched_cog_helpers
):
    """A resolved market hits the skipped path with a sensible reply."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Resolved Q?", initial_fair=50,
    )["prediction_id"]
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes", resolved_by=999)

    cog = _make_cog(prediction_service)
    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    await cog.force_refresh.callback(cog, interaction, pid)

    fwup = " ".join((m.get("content") or "") for m in interaction.followup.messages)
    assert "skipped" in fwup.lower()


async def test_resolve_announces_all_winners_and_losers(
    prediction_service, prediction_repo, player_repository, patched_cog_helpers
):
    """Admin resolution lists every settled winner and loser, not only the top winner."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)
    patched_cog_helpers.setattr(pmod, "get_neon_service", lambda _bot: None)

    for discord_id in (101, 102, 201, 202):
        _add_player(player_repository, discord_id, balance=1000)

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Will everyone print?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=101, side="yes", contracts=2)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=102, side="yes", contracts=1)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=201, side="no", contracts=3)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=202, side="no", contracts=1)
    winner_101_spent = prediction_repo.get_position(pid, 101)["yes_cost_basis_total"]
    winner_102_spent = prediction_repo.get_position(pid, 102)["yes_cost_basis_total"]
    loser_201_loss = prediction_repo.get_position(pid, 201)["no_cost_basis_total"]
    loser_202_loss = prediction_repo.get_position(pid, 202)["no_cost_basis_total"]

    cog = _make_cog(prediction_service)
    cog.announce_to_gamba = AsyncMock()
    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    # Admin must resolve from within the market's own guild (ownership guard).
    interaction.guild = SimpleNamespace(id=TEST_GUILD_ID)

    await cog.resolve.callback(
        cog, interaction, pid, SimpleNamespace(value="yes")
    )

    announce = "\n".join((m.get("content") or "") for m in interaction.followup.messages)
    # Each participant appears exactly once with spent, contract split, won, net.
    won_101 = 2 * PREDICTION_CONTRACT_VALUE
    won_102 = 1 * PREDICTION_CONTRACT_VALUE
    assert (
        f"<@101> spent {winner_101_spent} (2 yes / 0 no) → "
        f"won {won_101} JC (net {won_101 - winner_101_spent:+d})"
    ) in announce
    assert (
        f"<@102> spent {winner_102_spent} (1 yes / 0 no) → "
        f"won {won_102} JC (net {won_102 - winner_102_spent:+d})"
    ) in announce
    assert (
        f"<@201> spent {loser_201_loss} (0 yes / 3 no) → "
        f"won 0 JC (net {-loser_201_loss:+d})"
    ) in announce
    assert (
        f"<@202> spent {loser_202_loss} (0 yes / 1 no) → "
        f"won 0 JC (net {-loser_202_loss:+d})"
    ) in announce
    # No participant is double-listed (the old winners/losers split is gone).
    assert announce.count("<@101>") == 1
    assert announce.count("<@201>") == 1


async def test_rollback_admin_only(patched_cog_helpers):
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: False)
    prediction_service = MagicMock()
    cog = _make_cog(prediction_service)
    interaction = _FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)

    await cog.rollback.callback(cog, interaction, 42)

    prediction_service.rollback_orderbook.assert_not_called()
    assert interaction.response.messages == [
        {
            "content": "Only admins can roll back markets.",
            "embed": None,
            "ephemeral": True,
        }
    ]


async def test_rollback_reopens_thread_refreshes_embed_and_announces(
    patched_cog_helpers,
):
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)
    prediction_service = MagicMock()
    prediction_service.rollback_orderbook.return_value = {
        "prediction_id": 42,
        "previous_outcome": "no",
        "total_reversed": 30,
        "affected_players": 2,
    }
    prediction_service.get_prediction.return_value = {"thread_id": 12345}
    thread = _FakeThread(archived=True, locked=True)
    cog = _make_cog(prediction_service, thread=thread)
    cog.refresh_market_embed = AsyncMock()
    cog.announce_to_gamba = AsyncMock()
    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)

    await cog.rollback.callback(cog, interaction, 42)

    prediction_service.rollback_orderbook.assert_called_once_with(
        42, TEST_GUILD_ID, 999
    )
    assert thread.edits == [
        {
            "locked": False,
            "archived": False,
            "auto_archive_duration": THREAD_AUTO_ARCHIVE_MINUTES,
        }
    ]
    cog.refresh_market_embed.assert_awaited_once_with(42, restore_view=True)
    announce = interaction.followup.messages[0]["content"]
    assert "Market #42" in announce
    assert "resolution rolled back" in announce
    assert "NO" in announce
    assert "30" in announce
    assert thread.sent == [announce]
    cog.announce_to_gamba.assert_awaited_once_with(interaction.guild, announce)


async def test_resolve_chunks_large_winner_and_loser_lists(patched_cog_helpers):
    """Large settlements stay under Discord's per-message content limit."""
    from commands import predictions as pmod

    class _FakeGambaChannel:
        name = "gamba"

        def __init__(self):
            self.messages: list[str] = []

        async def send(self, content=None, embed=None, **kwargs):
            self.messages.append(content or "")

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)
    patched_cog_helpers.setattr(pmod, "get_neon_service", lambda _bot: None)

    winners = [
        {
            "discord_id": 10_000 + i,
            "yes_contracts": 1,
            "no_contracts": 0,
            "winning_contracts": 1,
            "cost_basis": 700,
            "payout": 1_000 - i,
            "profit": (1_000 - i) - 700,
        }
        for i in range(80)
    ]
    losers = [
        {
            "discord_id": 20_000 + i,
            "yes_contracts": 0,
            "no_contracts": 1,
            "winning_contracts": 0,
            "cost_basis": 900 - i,
            "payout": 0,
            "profit": -(900 - i),
        }
        for i in range(80)
    ]
    prediction_service = MagicMock()
    prediction_service.resolve_orderbook.return_value = {
        "winners": winners,
        "losers": losers,
        "participants": winners + losers,
    }
    prediction_service.get_prediction.return_value = {
        "guild_id": TEST_GUILD_ID,
        "thread_id": None,
    }

    cog = _make_cog(prediction_service)
    gamba = _FakeGambaChannel()
    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    interaction.guild = SimpleNamespace(id=TEST_GUILD_ID, text_channels=[gamba])

    await cog.resolve.callback(
        cog, interaction, 42, SimpleNamespace(value="yes")
    )

    followup_contents = [m["content"] for m in interaction.followup.messages]
    assert len(followup_contents) > 1
    assert len(gamba.messages) == len(followup_contents)
    assert all(len(content) <= pmod.DISCORD_MESSAGE_MAX_CHARS for content in followup_contents)
    assert all(len(content) <= pmod.DISCORD_MESSAGE_MAX_CHARS for content in gamba.messages)

    full_followup = "\n".join(followup_contents)
    assert "<@10000> spent 700 (1 yes / 0 no) → won 1000 JC (net +300)" in full_followup
    assert "<@10079> spent 700 (1 yes / 0 no) → won 921 JC (net +221)" in full_followup
    assert "<@20000> spent 900 (0 yes / 1 no) → won 0 JC (net -900)" in full_followup
    assert "<@20079> spent 821 (0 yes / 1 no) → won 0 JC (net -821)" in full_followup


async def test_resolve_rejects_cross_guild_market(
    prediction_service, player_repository, patched_cog_helpers
):
    """An admin in guild B cannot resolve a market that belongs to guild A.

    prediction_id is a global auto-increment PK, so without the guild-ownership
    guard a guild-B admin could settle guild-A's market (paying/clawing real
    jopacoin from guild-A holders).
    """
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)

    # Market lives in guild A.
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Cross-guild reach?", initial_fair=50,
    )["prediction_id"]

    cog = _make_cog(prediction_service)
    cog.announce_to_gamba = AsyncMock()
    # Admin acts from guild B.
    interaction = _FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID_SECONDARY)
    interaction.guild = SimpleNamespace(id=TEST_GUILD_ID_SECONDARY)

    await cog.resolve.callback(cog, interaction, pid, SimpleNamespace(value="yes"))

    messages = [m.get("content") or "" for m in interaction.followup.messages]
    assert any("not found in this server" in m for m in messages)
    # The market must remain unresolved.
    assert prediction_service.get_prediction(pid)["status"] != "resolved"


async def test_refresh_status_admin_only(
    prediction_service, player_repository, patched_cog_helpers
):
    """Non-admin gets a refusal reply; service.list_open_orderbook_markets is not called."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: False)

    cog = _make_cog(prediction_service)
    interaction = _FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    await cog.refresh_status.callback(cog, interaction)

    assert any("admin" in (m.get("content") or "").lower() for m in interaction.response.messages)
    assert interaction.followup.messages == []


async def test_refresh_status_lists_only_this_guild(
    prediction_service, player_repository, patched_cog_helpers
):
    """refresh_status returns an embed listing this guild's open markets only."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)

    p1 = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Open Q1?", initial_fair=50,
    )["prediction_id"]
    p2 = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Open Q2?", initial_fair=70,
    )["prediction_id"]
    p_other = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID + 1, creator_id=999, question="OtherQ?", initial_fair=40,
    )["prediction_id"]

    cog = _make_cog(prediction_service)
    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    await cog.refresh_status.callback(cog, interaction)

    embeds = [m["embed"] for m in interaction.followup.messages if m.get("embed")]
    assert len(embeds) == 1
    body = embeds[0].description
    assert f"{p1:>4}" in body
    assert f"{p2:>4}" in body
    assert f"{p_other:>4}" not in body


# --------------------------------------------------------------------------- #
# refresh_market_embed — archived-thread recovery
# --------------------------------------------------------------------------- #


class _FakeEmbedMessage:
    def __init__(self):
        self.calls: list[str] = []
        self.edit_kwargs: dict | None = None
        self.thread: _FakeArchivableThread | None = None

    async def edit(self, **kwargs):
        # Discord rejects message edits in archived threads (error 50083);
        # note sends are different — they auto-unarchive unlocked threads.
        if self.thread is not None and self.thread.archived:
            raise RuntimeError("50083: Thread is archived")
        self.calls.append("msg.edit")
        self.edit_kwargs = kwargs


class _FakeArchivableThread:
    """Thread stand-in that rejects message edits while archived (50083)."""

    def __init__(self, archived: bool, embed_msg: _FakeEmbedMessage):
        self.archived = archived
        self.locked = False
        self.auto_archive_duration = 1440
        self._embed_msg = embed_msg
        self.calls = embed_msg.calls  # shared ordered call log
        embed_msg.thread = self

    async def edit(
        self,
        *,
        archived: bool,
        auto_archive_duration: int | None = None,
        locked: bool | None = None,
    ):
        # Keyword-only, like the real Thread.edit.
        self.calls.append(f"thread.edit(archived={archived})")
        self.archived = archived
        if auto_archive_duration is not None:
            self.auto_archive_duration = auto_archive_duration
        if locked is not None:
            self.locked = locked

    async def fetch_message(self, msg_id):
        self.calls.append("fetch_message")
        assert msg_id == _EMBED_MSG_ID, f"unexpected message id {msg_id}"
        return self._embed_msg


_THREAD_ID = 555
_EMBED_MSG_ID = 777


def _market_with_thread_ids(prediction_service):
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=999, question="Frozen embed?", initial_fair=50,
    )["prediction_id"]
    prediction_service.prediction_repo.update_prediction_discord_ids(
        pid, thread_id=_THREAD_ID, embed_message_id=_EMBED_MSG_ID
    )
    return pid


def _cog_with_uncached_thread(prediction_service, thread):
    """Cog whose bot resolves the thread only via fetch_channel.

    Mirrors production: discord.py evicts archived threads from the cache,
    so get_channel returns None and the code must take the fetch fallback.
    """
    cog = _make_cog(prediction_service, thread=thread)
    cog.bot.get_channel = lambda cid: None
    return cog


async def test_refresh_market_embed_unarchives_thread_before_editing(prediction_service):
    """An auto-archived market thread must be revived before the message edit,
    otherwise Discord rejects the edit and the embed silently freezes forever
    (the daily refresh keeps stamping last_refresh_at regardless). The message
    is fetched first — reading works in archived threads, and a vanished
    message must not trigger a pointless revival. The revival also re-widens
    the auto-archive window so pre-fix threads stop re-archiving daily."""
    msg = _FakeEmbedMessage()
    thread = _FakeArchivableThread(archived=True, embed_msg=msg)

    pid = _market_with_thread_ids(prediction_service)
    cog = _cog_with_uncached_thread(prediction_service, thread)
    cog.render_market_chart_file = AsyncMock(return_value=None)

    await cog.refresh_market_embed(pid)

    assert thread.calls == ["fetch_message", "thread.edit(archived=False)", "msg.edit"]
    assert thread.archived is False
    assert thread.auto_archive_duration == THREAD_AUTO_ARCHIVE_MINUTES
    # The edit carried a real payload (a regression to attachments=None would
    # TypeError in prod while a record-only fake stayed green).
    assert msg.edit_kwargs is not None
    assert "embed" in msg.edit_kwargs
    assert msg.edit_kwargs["attachments"] == []


async def test_refresh_market_embed_leaves_live_thread_alone(prediction_service):
    """No spurious thread.edit round-trips when the thread is not archived."""
    msg = _FakeEmbedMessage()
    thread = _FakeArchivableThread(archived=False, embed_msg=msg)

    pid = _market_with_thread_ids(prediction_service)
    cog = _make_cog(prediction_service, thread=thread)
    cog.render_market_chart_file = AsyncMock(return_value=None)

    await cog.refresh_market_embed(pid)

    assert thread.calls == ["fetch_message", "msg.edit"]


async def test_refresh_market_embed_restores_persistent_view(prediction_service):
    msg = _FakeEmbedMessage()
    thread = _FakeArchivableThread(archived=False, embed_msg=msg)

    pid = _market_with_thread_ids(prediction_service)
    cog = _make_cog(prediction_service, thread=thread)
    cog.render_market_chart_file = AsyncMock(return_value=None)

    await cog.refresh_market_embed(pid, restore_view=True)

    assert msg.edit_kwargs is not None
    assert isinstance(msg.edit_kwargs["view"], PersistentMarketView)


async def test_refresh_market_embed_vanished_message_does_not_revive_thread(
    prediction_service,
):
    """A deleted embed message must not turn the thread into a daily-revived
    zombie: fetch_message raises NotFound before any unarchive happens."""

    class _GoneMessageThread(_FakeArchivableThread):
        async def fetch_message(self, msg_id):
            self.calls.append("fetch_message")
            raise discord.NotFound(MagicMock(status=404), "message deleted")

    msg = _FakeEmbedMessage()
    thread = _GoneMessageThread(archived=True, embed_msg=msg)

    pid = _market_with_thread_ids(prediction_service)
    cog = _cog_with_uncached_thread(prediction_service, thread)
    cog.render_market_chart_file = AsyncMock(return_value=None)

    await cog.refresh_market_embed(pid)

    assert thread.calls == ["fetch_message"]
    assert thread.archived is True


async def test_resolve_rewrites_embed_and_archives_even_if_announcement_fails(
    prediction_service, patched_cog_helpers
):
    """The resolve announcement must not gate the embed rewrite or the thread
    lock: payouts have already happened, so a failed send would otherwise
    leave a live-looking book on a settled market forever. The thread is also
    revived first, so resolving a market whose thread auto-archived works."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)

    class _FailingSendThread(_FakeArchivableThread):
        async def send(self, content):
            raise RuntimeError("send blocked (e.g. locked thread)")

    msg = _FakeEmbedMessage()
    thread = _FailingSendThread(archived=True, embed_msg=msg)

    pid = _market_with_thread_ids(prediction_service)
    cog = _cog_with_uncached_thread(prediction_service, thread)
    cog.render_market_chart_file = AsyncMock(return_value=None)

    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    interaction.guild.text_channels = []
    await cog.resolve.callback(cog, interaction, pid, SimpleNamespace(value="yes"))

    assert prediction_service.get_prediction(pid)["status"] == "resolved"
    # The resolved embed was rendered despite the failed announcement...
    assert "msg.edit" in thread.calls
    # ...and the thread still got locked + archived afterwards.
    assert thread.locked is True
    assert thread.archived is True


async def test_create_uses_max_auto_archive_and_persists_thread_ids(
    prediction_service, patched_cog_helpers
):
    """/predict create must open its thread with the widest auto-archive window
    (quiet threads otherwise archive on the channel default and embed edits
    start failing) and persist the ids the refresh loop needs."""
    from commands import predictions as pmod

    patched_cog_helpers.setattr(pmod, "has_admin_permission", lambda _: True)

    embed_msg = SimpleNamespace(id=777, pin=AsyncMock())
    thread = SimpleNamespace(id=555, send=AsyncMock(return_value=embed_msg))
    channel_msg = SimpleNamespace(id=333, create_thread=AsyncMock(return_value=thread))

    cog = _make_cog(prediction_service)
    cog.render_market_chart_file = AsyncMock(return_value=None)

    interaction = _FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    interaction.channel.send = AsyncMock(return_value=channel_msg)
    interaction.channel_id = 11
    interaction.user.mention = "<@999>"

    await cog.create.callback(cog, interaction, "Will it archive?", 50)

    create_kwargs = channel_msg.create_thread.await_args.kwargs
    assert create_kwargs["auto_archive_duration"] == THREAD_AUTO_ARCHIVE_MINUTES

    opens = prediction_service.list_open_orderbook_markets(TEST_GUILD_ID)
    assert len(opens) == 1
    pred = prediction_service.get_prediction(opens[0]["prediction_id"])
    assert pred["thread_id"] == 555
    assert pred["embed_message_id"] == 777


# --------------------------------------------------------------------------- #
# 10:1 stock-split: rounding rule, fair-history snapshots, banner sentinel
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    "raw, kind, expected",
    [
        # Even multiples — no rounding needed.
        (200, "buy", 20),
        (200, "sell", 20),
        # Below the half-tick (0.x < 0.5): both round to the same nearest int.
        (201, "buy", 20),     # 20.1 → 20
        (201, "sell", 20),
        # Tie at .5 — goes to the house: buy ceils, sell floors.
        (205, "buy", 21),     # 20.5 → 21 (round-half-up)
        (205, "sell", 20),    # 20.5 → 20 (round-half-down)
        # Above the half-tick (0.x > 0.5): both round up.
        (206, "buy", 21),
        (206, "sell", 21),
        # Sub-1 buy gets bumped to the 1-jopa minimum.
        (4, "buy", 1),
        # Sub-1 sell can land at 0 jopa proceeds — that's fine.
        (4, "sell", 0),
        # Zero numerator (e.g. zero-qty fill, shouldn't actually happen) → 0.
        (0, "buy", 0),
        (0, "sell", 0),
    ],
)
def test_quote_total_rounding(raw, kind, expected):
    assert _quote_total(raw, kind) == expected


def test_create_orderbook_writes_initial_fair_snapshot(prediction_service, prediction_repo):
    """Creating a market drops a 'create' snapshot so the chart isn't empty on day one."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="snap a?", initial_fair=42,
    )["prediction_id"]
    history = prediction_repo.get_fair_history(pid, TEST_GUILD_ID)
    assert len(history) == 1
    assert history[0][1] == 42


def test_apply_refresh_appends_fair_snapshot(
    prediction_service, prediction_repo, monkeypatch,
):
    """Each refresh adds a fair-history row carrying the new fair."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="snap b?", initial_fair=50,
    )["prediction_id"]
    monkeypatch.setattr(random, "randint", lambda lo, hi: 3)  # +3 drift
    prediction_service.refresh_market(pid)
    history = prediction_repo.get_fair_history(pid, TEST_GUILD_ID)
    assert len(history) >= 2
    assert history[-1][1] == 53


def test_set_fair_manual_appends_fair_snapshot(prediction_service, prediction_repo):
    """Admin set_fair stamps a snapshot the chart can render alongside refresh ones."""
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="snap c?", initial_fair=50,
    )["prediction_id"]
    prediction_service.set_fair_manual(prediction_id=pid, new_price=70)
    history = prediction_repo.get_fair_history(pid, TEST_GUILD_ID)
    assert history[-1][1] == 70


def test_pop_one_shot_flag_self_clears(prediction_repo):
    """``pop_one_shot_flag`` returns True once and never again."""
    with prediction_repo.connection() as conn:
        conn.execute(
            "INSERT INTO app_kv (guild_id, key, value) VALUES (?, ?, '0')",
            (TEST_GUILD_ID, "test_banner"),
        )
    assert prediction_repo.pop_one_shot_flag(TEST_GUILD_ID, "test_banner") is True
    assert prediction_repo.pop_one_shot_flag(TEST_GUILD_ID, "test_banner") is False


def test_pop_one_shot_flag_missing_returns_false(prediction_repo):
    """Guilds with no sentinel row never see the banner."""
    assert prediction_repo.pop_one_shot_flag(TEST_GUILD_ID, "nope") is False


def test_market_chart_renders_for_zero_one_and_many_snapshots():
    """The chart util tolerates empty / single / many snapshot inputs without erroring."""
    from utils.drawing.predictions import draw_market_fair_history

    created_at = 1_700_000_000
    # Empty.
    bytes_empty = draw_market_fair_history(market_id=1, snapshots=[], created_at=created_at)
    assert bytes_empty.getbuffer().nbytes > 0

    # Single point.
    bytes_one = draw_market_fair_history(
        market_id=2, snapshots=[(created_at + 60, 50)], created_at=created_at
    )
    assert bytes_one.getbuffer().nbytes > 0

    # Many points across hours.
    series = [(created_at + 3600 * i, 30 + i * 5) for i in range(8)]
    bytes_many = draw_market_fair_history(market_id=3, snapshots=series, created_at=created_at)
    assert bytes_many.getbuffer().nbytes > 0


def test_fair_history_backfill_from_levels_groups_by_utc_day(prediction_repo, prediction_service):
    """The retro-population migration walks prediction_levels.posted_at,
    buckets to UTC day, and inserts one snapshot per day at end-of-day with
    a defensible mid (or single-side fallback when only one side was posted).
    """
    from datetime import UTC, datetime

    from infrastructure.schema_manager import SchemaManager

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="retro?", initial_fair=50,
    )["prediction_id"]

    # Day A (2026-04-01 UTC): both sides posted → mid (35 + 65) / 2 = 50.
    day_a_ts = int(datetime(2026, 4, 1, 12, 0, 0, tzinfo=UTC).timestamp())
    day_a_eod = int(datetime(2026, 4, 1, 23, 59, 59, tzinfo=UTC).timestamp())
    # Day B (2026-04-03 UTC): only yes_ask posted → snapshot at min ask = 70.
    day_b_ts = int(datetime(2026, 4, 3, 6, 30, 0, tzinfo=UTC).timestamp())
    day_b_eod = int(datetime(2026, 4, 3, 23, 59, 59, tzinfo=UTC).timestamp())

    seed = [
        ("yes_ask", 65, 100, day_a_ts),
        ("yes_ask", 67, 100, day_a_ts + 60),  # same day, same side, larger price
        ("yes_bid", 35, 100, day_a_ts),
        ("yes_ask", 70, 100, day_b_ts),
        ("yes_ask", 72, 100, day_b_ts + 30),
    ]
    with prediction_repo.connection() as conn:
        cursor = conn.cursor()
        # Wipe the snapshot + levels written during create_orderbook_prediction
        # so we control the seeded state in isolation.
        cursor.execute("DELETE FROM prediction_fair_snapshots WHERE market_id = ?", (pid,))
        cursor.execute("DELETE FROM prediction_levels WHERE prediction_id = ?", (pid,))
        for side, price, size, posted_at in seed:
            cursor.execute(
                "INSERT INTO prediction_levels "
                "(prediction_id, side, price, remaining_size, posted_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (pid, side, price, size, posted_at),
            )
        SchemaManager(prediction_repo.db_path)._migration_predictions_fair_history_backfill_from_levels(cursor)

    history = prediction_repo.get_fair_history(pid, TEST_GUILD_ID)
    # One snapshot per UTC day of activity, each at end-of-day.
    assert [snapshot_at for snapshot_at, _ in history] == [day_a_eod, day_b_eod]
    # Day A: mid of cheapest yes_ask (65) and highest yes_bid (35) = 50.
    # Day B: only yes_ask side posted → cheapest yes_ask (70).
    assert [pct for _, pct in history] == [50, 70]

    # Provenance: backfill_levels.
    with prediction_repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "SELECT reason FROM prediction_fair_snapshots WHERE market_id = ? ORDER BY snapshot_at",
            (pid,),
        )
        reasons = [row["reason"] for row in cursor.fetchall()]
    assert reasons == ["backfill_levels", "backfill_levels"]

    # Re-running the migration must not duplicate snapshots — the HAVING NOT
    # EXISTS guard skips (market, day) buckets already present.
    with prediction_repo.connection() as conn:
        cursor = conn.cursor()
        SchemaManager(prediction_repo.db_path)._migration_predictions_fair_history_backfill_from_levels(cursor)
    history_after = prediction_repo.get_fair_history(pid, TEST_GUILD_ID)
    assert history_after == history


def test_market_chart_auto_zooms_for_narrow_band_markets():
    """Narrow-band series should auto-zoom Y so the line is legible. A series
    sitting in 17–22% must render differently from the same shape lifted to
    cover the full 0–100% range — proving the Y axis adapted."""
    from utils.drawing.predictions import draw_market_fair_history

    created_at = 1_700_000_000
    narrow = [(created_at + 3600 * i, p) for i, p in enumerate([17, 19, 22, 18, 20])]
    wide = [(created_at + 3600 * i, p) for i, p in enumerate([5, 30, 70, 40, 95])]

    narrow_bytes = draw_market_fair_history(
        market_id=10, snapshots=narrow, created_at=created_at
    ).getvalue()
    wide_bytes = draw_market_fair_history(
        market_id=10, snapshots=wide, created_at=created_at
    ).getvalue()

    # The two charts must render to different pixels — gridlines and line
    # positions both depend on the auto-zoomed range.
    assert narrow_bytes != wide_bytes


# --------------------------------------------------------------------------- #
# Per-user order-book stats (profile Predictions tab)
# --------------------------------------------------------------------------- #


def test_get_user_orderbook_stats_empty(prediction_repo):
    """A user with no order-book activity gets zeroed stats."""
    stats = prediction_repo.get_user_orderbook_stats(12345, TEST_GUILD_ID)
    assert stats == {"realized_pnl": 0, "wins": 0, "losses": 0, "resolved_markets": 0}


def test_get_user_orderbook_stats_realized_pnl(
    prediction_service, prediction_repo, player_repository
):
    """Realized P&L sums resolved markets only; wins/losses split on P&L sign."""
    _add_player(player_repository, 1, balance=100_000)

    # Win market: buy YES, resolve YES.
    win_pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="Win market?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=win_pid, discord_id=1, side="yes", contracts=3,
    )
    win_cost = prediction_repo.get_position(win_pid, 1)["yes_cost_basis_total"]

    # Loss market: buy YES, resolve NO.
    loss_pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="Loss market?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=loss_pid, discord_id=1, side="yes", contracts=3,
    )
    loss_cost = prediction_repo.get_position(loss_pid, 1)["yes_cost_basis_total"]

    # Open market: buy YES, leave open — must NOT count toward realized stats.
    open_pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="Open market?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=open_pid, discord_id=1, side="yes", contracts=3,
    )

    prediction_service.resolve_orderbook(prediction_id=win_pid, outcome="yes")
    prediction_service.resolve_orderbook(prediction_id=loss_pid, outcome="no")

    stats = prediction_repo.get_user_orderbook_stats(1, TEST_GUILD_ID)

    expected_pnl = (3 * PREDICTION_CONTRACT_VALUE - win_cost) + (0 - loss_cost)
    assert stats["realized_pnl"] == expected_pnl
    assert stats["resolved_markets"] == 2
    assert stats["wins"] == 1
    assert stats["losses"] == 1


def test_get_player_orderbook_pnl_history(
    prediction_service, prediction_repo, player_repository
):
    """One row per resolved market (delta = payout - cost); open markets excluded."""
    _add_player(player_repository, 1, balance=100_000)

    win_pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="History win?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=win_pid, discord_id=1, side="yes", contracts=3,
    )
    win_cost = prediction_repo.get_position(win_pid, 1)["yes_cost_basis_total"]

    open_pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="History open?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(
        prediction_id=open_pid, discord_id=1, side="yes", contracts=3,
    )

    prediction_service.resolve_orderbook(prediction_id=win_pid, outcome="yes")

    history = prediction_repo.get_player_orderbook_pnl_history(1, TEST_GUILD_ID)

    assert len(history) == 1  # open market excluded
    assert history[0]["prediction_id"] == win_pid
    assert history[0]["delta"] == 3 * PREDICTION_CONTRACT_VALUE - win_cost
    assert history[0]["settle_time"] > 0


# --------------------------------------------------------------------------- #
# announce_to_gamba — digest chart attachment plumbing
# --------------------------------------------------------------------------- #


def _gamba_cog_and_channel():
    """A bare PredictionCommands plus a fake #gamba channel that records send()."""
    sent: dict = {}

    class _Ch:
        name = "gamba"

        async def send(self, content=None, embed=None, **kwargs):
            sent["content"] = content
            sent["embed"] = embed
            sent["file_passed"] = "file" in kwargs
            sent["file"] = kwargs.get("file")

    guild = SimpleNamespace(text_channels=[_Ch()])
    cog = PredictionCommands(SimpleNamespace(), SimpleNamespace(), SimpleNamespace())
    return cog, guild, sent


async def test_announce_to_gamba_forwards_file_when_present():
    cog, guild, sent = _gamba_cog_and_channel()
    chart = object()
    await cog.announce_to_gamba(guild, embed="E", file=chart)
    assert sent["file_passed"] is True
    assert sent["file"] is chart


async def test_announce_to_gamba_omits_file_kwarg_when_none():
    """discord.py rejects file=None, so it must not be passed through at all."""
    cog, guild, sent = _gamba_cog_and_channel()
    await cog.announce_to_gamba(guild, embed="E")
    assert sent["file_passed"] is False
