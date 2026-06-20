"""Tests for PredictionService resolution / lock path.

Covers the parts of the resolution flow that the order-book tests in
``test_predictions.py`` do not touch:

  - ``check_and_lock_expired`` (open markets past close lock; future ones don't)
  - ``resolve_orderbook`` settlement direction (winning side paid, losing side not)
  - bankruptcy-penalty netting at settlement and in realized-P&L reads

Uses the real ``PredictionRepository`` + ``PlayerRepository`` via fixtures.
"""

from __future__ import annotations

import time

import pytest

from config import PREDICTION_CONTRACT_VALUE
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from services.prediction_service import PredictionService
from tests.conftest import TEST_GUILD_ID

ADMIN_ID = 999


@pytest.fixture
def prediction_repo(repo_db_path):
    return PredictionRepository(repo_db_path)


@pytest.fixture
def prediction_service(prediction_repo, player_repository):
    return PredictionService(
        prediction_repo=prediction_repo,
        player_repo=player_repository,
        admin_user_ids=[ADMIN_ID],
    )


def _add_player(player_repo: PlayerRepository, discord_id: int, balance: int = 1000):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"user{discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


# --------------------------------------------------------------------------- #
# check_and_lock_expired
# --------------------------------------------------------------------------- #


def test_check_and_lock_expired_locks_only_past_close(prediction_service, prediction_repo):
    """Open markets past their close lock; markets still open don't."""
    expired = prediction_repo.create_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="expired?",
        closes_at=int(time.time()) - 10,
    )
    future = prediction_repo.create_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="future?",
        closes_at=int(time.time()) + 3600,
    )

    locked = prediction_service.check_and_lock_expired(TEST_GUILD_ID)

    assert expired in locked
    assert future not in locked
    assert prediction_repo.get_prediction(expired)["status"] == "locked"
    assert prediction_repo.get_prediction(future)["status"] == "open"


def test_check_and_lock_expired_ignores_already_locked(prediction_service, prediction_repo):
    """Only 'open' rows are candidates; a locked expired market isn't re-reported."""
    pid = prediction_repo.create_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="already locked?",
        closes_at=int(time.time()) - 10,
    )
    prediction_repo.update_prediction_status(pid, "locked")
    assert prediction_service.check_and_lock_expired(TEST_GUILD_ID) == []


# --------------------------------------------------------------------------- #
# resolve_orderbook: settlement pays the winning side only
# --------------------------------------------------------------------------- #


def test_resolve_orderbook_pays_no_side_when_no_wins(
    prediction_service, prediction_repo, player_repository
):
    """Resolving NO pays NO holders 10 jopa/contract and leaves YES holders flat.

    Mirror of the YES-wins coverage in test_predictions.py, asserting the
    opposite direction so a swapped payout branch would fail here.
    """
    _add_player(player_repository, 1, balance=1000)
    _add_player(player_repository, 2, balance=1000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="who wins side?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=2, side="no", contracts=4)

    yes_pre = player_repository.get_balance(1, TEST_GUILD_ID)
    no_pre = player_repository.get_balance(2, TEST_GUILD_ID)

    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="no")

    assert result["total_payout"] == 4 * PREDICTION_CONTRACT_VALUE
    # NO holder paid 4 * contract value; YES holder gets nothing.
    assert player_repository.get_balance(2, TEST_GUILD_ID) - no_pre == 4 * PREDICTION_CONTRACT_VALUE
    assert player_repository.get_balance(1, TEST_GUILD_ID) == yes_pre


def test_resolve_orderbook_two_sided_holder_profit_nets_both_bases(
    prediction_service, prediction_repo, player_repository
):
    """A hedger holding BOTH sides: resolving YES credits only the YES payout,
    and the reported profit subtracts the losing NO cost basis too. This keeps
    the bankruptcy-penalty base (charged on profit) and the stats P&L from
    over-crediting — and over-penalizing — two-sided holders.
    """
    _add_player(player_repository, 1, balance=10000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="hedger?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="no", contracts=3)

    pos = prediction_repo.get_position(pid, 1)
    yes_cost = pos["yes_cost_basis_total"]
    no_cost = pos["no_cost_basis_total"]
    assert no_cost > 0  # genuinely two-sided

    pre = player_repository.get_balance(1, TEST_GUILD_ID)
    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")

    payout = 5 * PREDICTION_CONTRACT_VALUE
    # Only the winning (YES) side is credited.
    assert player_repository.get_balance(1, TEST_GUILD_ID) - pre == payout
    winner = next(w for w in result["winners"] if w["discord_id"] == 1)
    assert winner["payout"] == payout
    # Profit nets BOTH cost bases, not just the winning side's.
    assert winner["profit"] == payout - yes_cost - no_cost


def test_resolve_orderbook_bankruptcy_penalty_is_netted_in_txn(
    repo_db_path, player_repository
):
    """A penalized winner's bankruptcy penalty is withheld inside the settlement
    txn (no follow-up debit): the credited balance is net of the penalty and the
    winner dict reports it."""
    from config import BANKRUPTCY_PENALTY_RATE
    from repositories.bankruptcy_repository import BankruptcyRepository
    from services.bankruptcy_service import BankruptcyService

    bankruptcy_service = BankruptcyService(
        BankruptcyRepository(repo_db_path), player_repository
    )
    prediction_repo = PredictionRepository(repo_db_path)
    prediction_service = PredictionService(
        prediction_repo=prediction_repo,
        player_repo=player_repository,
        admin_user_ids=[ADMIN_ID],
        bankruptcy_service=bankruptcy_service,
    )
    _add_player(player_repository, 1, balance=1000)
    # Put player 1 under penalty via the real path (declares bankruptcy from
    # debt), then top their balance back up so they can buy contracts.
    player_repository.update_balance(1, TEST_GUILD_ID, -50)
    assert bankruptcy_service.execute_bankruptcy(1, TEST_GUILD_ID).success
    player_repository.update_balance(1, TEST_GUILD_ID, 1000)

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="penalty?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    yes_cost = prediction_repo.get_position(pid, 1)["yes_cost_basis_total"]

    pre = player_repository.get_balance(1, TEST_GUILD_ID)
    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")

    payout = 5 * PREDICTION_CONTRACT_VALUE
    penalty = int((payout - yes_cost) * (1 - BANKRUPTCY_PENALTY_RATE))
    assert penalty > 0  # genuinely penalized
    winner = next(w for w in result["winners"] if w["discord_id"] == 1)
    assert winner["bankruptcy_penalty"] == penalty
    # Balance was credited net of the penalty, in one shot.
    assert player_repository.get_balance(1, TEST_GUILD_ID) - pre == payout - penalty


def test_orderbook_stats_net_bankruptcy_penalty_for_winner(
    repo_db_path, player_repository
):
    """Realized-P&L stats and the balance-chart delta for a penalized winner must
    net out the withheld bankruptcy penalty so they match the JC actually
    credited. Before persisting the penalty, both reads recomputed
    ``won*CONTRACT_VALUE - cost`` and overstated the gain by exactly the penalty.
    """
    from config import BANKRUPTCY_PENALTY_RATE
    from repositories.bankruptcy_repository import BankruptcyRepository
    from services.bankruptcy_service import BankruptcyService

    bankruptcy_service = BankruptcyService(
        BankruptcyRepository(repo_db_path), player_repository
    )
    prediction_repo = PredictionRepository(repo_db_path)
    prediction_service = PredictionService(
        prediction_repo=prediction_repo,
        player_repo=player_repository,
        admin_user_ids=[ADMIN_ID],
        bankruptcy_service=bankruptcy_service,
    )
    _add_player(player_repository, 1, balance=1000)
    player_repository.update_balance(1, TEST_GUILD_ID, -50)
    assert bankruptcy_service.execute_bankruptcy(1, TEST_GUILD_ID).success
    player_repository.update_balance(1, TEST_GUILD_ID, 1000)

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="stats net penalty?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    yes_cost = prediction_repo.get_position(pid, 1)["yes_cost_basis_total"]

    pre = player_repository.get_balance(1, TEST_GUILD_ID)
    prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")

    payout = 5 * PREDICTION_CONTRACT_VALUE
    penalty = int((payout - yes_cost) * (1 - BANKRUPTCY_PENALTY_RATE))
    assert penalty > 0  # genuinely penalized

    # The credited balance delta is the source of truth.
    credited = player_repository.get_balance(1, TEST_GUILD_ID) - pre
    assert credited == payout - penalty

    # Realized P&L must equal the credited gain (gross payout - cost - penalty),
    # not the un-penalized payout - cost. Cost basis is already a sunk debit, so
    # net realized P&L is credited - cost.
    stats = prediction_repo.get_user_orderbook_stats(1, TEST_GUILD_ID)
    expected_pnl = payout - yes_cost - penalty
    assert stats["realized_pnl"] == expected_pnl
    assert stats["wins"] == 1

    # The balance-chart delta must likewise be net of the penalty.
    history = prediction_repo.get_player_orderbook_pnl_history(1, TEST_GUILD_ID)
    entry = next(h for h in history if h["prediction_id"] == pid)
    assert entry["delta"] == expected_pnl


def test_resolve_orderbook_penalty_uses_net_profit_for_hedger(
    repo_db_path, player_repository
):
    """A two-sided holder under penalty is penalized on profit NET of both cost
    bases, not the (larger) winning-side-only figure — so a hedger isn't
    over-penalized. Fails on the old single-side profit basis."""
    from config import BANKRUPTCY_PENALTY_RATE
    from repositories.bankruptcy_repository import BankruptcyRepository
    from services.bankruptcy_service import BankruptcyService

    bankruptcy_service = BankruptcyService(
        BankruptcyRepository(repo_db_path), player_repository
    )
    prediction_repo = PredictionRepository(repo_db_path)
    prediction_service = PredictionService(
        prediction_repo=prediction_repo,
        player_repo=player_repository,
        admin_user_ids=[ADMIN_ID],
        bankruptcy_service=bankruptcy_service,
    )
    _add_player(player_repository, 1, balance=1000)
    player_repository.update_balance(1, TEST_GUILD_ID, -50)
    assert bankruptcy_service.execute_bankruptcy(1, TEST_GUILD_ID).success
    player_repository.update_balance(1, TEST_GUILD_ID, 1000)

    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="hedge under penalty?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="yes", contracts=5)
    prediction_service.buy_contracts(prediction_id=pid, discord_id=1, side="no", contracts=3)
    pos = prediction_repo.get_position(pid, 1)
    yes_cost, no_cost = pos["yes_cost_basis_total"], pos["no_cost_basis_total"]
    assert no_cost > 0  # genuinely two-sided

    pre = player_repository.get_balance(1, TEST_GUILD_ID)
    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")

    payout = 5 * PREDICTION_CONTRACT_VALUE
    net_profit = payout - yes_cost - no_cost  # penalty base nets BOTH stakes
    penalty = int(net_profit * (1 - BANKRUPTCY_PENALTY_RATE))
    assert penalty > 0
    winner = next(w for w in result["winners"] if w["discord_id"] == 1)
    assert winner["bankruptcy_penalty"] == penalty
    assert player_repository.get_balance(1, TEST_GUILD_ID) - pre == payout - penalty


def test_resolve_orderbook_settles_locked_market(
    prediction_service, prediction_repo, player_repository
):
    """A market locked (betting closed) must be settleable via resolve_orderbook.

    check_and_lock_expired / close_betting_early move a market to 'locked'.
    The admin /predict resolve command calls resolve_orderbook. If the status
    guard in settle_prediction_orderbook only allows 'open', locked markets
    become permanently unresolvable — a dead-end for any market whose betting
    window expired before an explicit admin close.
    """
    _add_player(player_repository, 10, balance=1000)
    pid = prediction_service.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID, creator_id=1, question="locked resolve?", initial_fair=50,
    )["prediction_id"]
    prediction_service.buy_contracts(prediction_id=pid, discord_id=10, side="yes", contracts=3)

    # Simulate betting close (lock the market)
    prediction_repo.update_prediction_status(pid, "locked")
    assert prediction_repo.get_prediction(pid)["status"] == "locked"

    pre = player_repository.get_balance(10, TEST_GUILD_ID)
    result = prediction_service.resolve_orderbook(prediction_id=pid, outcome="yes")

    assert result["outcome"] == "yes"
    assert result["total_payout"] == 3 * PREDICTION_CONTRACT_VALUE
    assert player_repository.get_balance(10, TEST_GUILD_ID) - pre == 3 * PREDICTION_CONTRACT_VALUE
