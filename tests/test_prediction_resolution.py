"""Tests for PredictionService resolution / voting / lock path.

Covers the vote-gated resolution flow that the order-book tests in
``test_predictions.py`` do not touch:

  - ``add_resolution_vote`` (close-time gate, duplicate-flip rejection)
  - ``_check_can_resolve`` / ``can_resolve`` (3-vote threshold + admin override)
  - ``check_and_lock_expired`` (open markets past close lock; future ones don't)
  - ``resolve_orderbook`` settlement direction (winning side paid, losing side not)

Uses the real ``PredictionRepository`` + ``PlayerRepository`` via fixtures.
Voting requires a closed betting window, so these create the market row
directly through the repo with a past ``closes_at`` (the service-level
``create_prediction`` deliberately rejects past close times).
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


def _make_closed_prediction(repo: PredictionRepository, *, closes_offset: int = -3600) -> int:
    """Create a legacy (yes/no vote) prediction whose betting window has closed."""
    pid = repo.create_prediction(
        guild_id=TEST_GUILD_ID,
        creator_id=1,
        question="Will it resolve?",
        closes_at=int(time.time()) + closes_offset,
    )
    return pid


# --------------------------------------------------------------------------- #
# add_resolution_vote: gating
# --------------------------------------------------------------------------- #


def test_vote_rejected_before_betting_closes(prediction_service, prediction_repo):
    """Voting before close_time raises — you can't call a market mid-bet."""
    pid = prediction_repo.create_prediction(
        guild_id=TEST_GUILD_ID,
        creator_id=1,
        question="Still open?",
        closes_at=int(time.time()) + 3600,  # future
    )
    with pytest.raises(ValueError, match="until betting period closes"):
        prediction_service.add_resolution_vote(pid, user_id=10, outcome="yes")


def test_vote_rejected_on_resolved_market(prediction_service, prediction_repo):
    pid = _make_closed_prediction(prediction_repo)
    prediction_repo.update_prediction_status(pid, "resolved")
    with pytest.raises(ValueError, match="already been resolved"):
        prediction_service.add_resolution_vote(pid, user_id=10, outcome="yes")


def test_vote_flip_rejected(prediction_service, prediction_repo):
    """A user cannot change their vote to the opposite outcome."""
    pid = _make_closed_prediction(prediction_repo)
    prediction_service.add_resolution_vote(pid, user_id=10, outcome="yes")
    with pytest.raises(ValueError, match="different outcome"):
        prediction_service.add_resolution_vote(pid, user_id=10, outcome="no")


# --------------------------------------------------------------------------- #
# _check_can_resolve / can_resolve: threshold + admin
# --------------------------------------------------------------------------- #


def test_three_matching_votes_enable_resolution(prediction_service, prediction_repo):
    """MIN_RESOLUTION_VOTES (3) matching non-admin votes flips can_resolve True.

    Two votes is not enough; the third (same outcome) crosses the threshold.
    """
    assert PredictionService.MIN_RESOLUTION_VOTES == 3
    pid = _make_closed_prediction(prediction_repo)

    r1 = prediction_service.add_resolution_vote(pid, user_id=10, outcome="yes", is_admin=False)
    assert r1["can_resolve"] is False
    r2 = prediction_service.add_resolution_vote(pid, user_id=11, outcome="yes", is_admin=False)
    assert r2["can_resolve"] is False
    r3 = prediction_service.add_resolution_vote(pid, user_id=12, outcome="yes", is_admin=False)
    assert r3["can_resolve"] is True
    assert r3["yes_count"] == 3

    # can_resolve(pid, "yes") agrees; the other side has zero votes.
    assert prediction_service.can_resolve(pid, "yes") is True
    assert prediction_service.can_resolve(pid, "no") is False
    # Outcome-agnostic check: any side at threshold -> True.
    assert prediction_service.can_resolve(pid) is True
    assert prediction_service.get_pending_outcome(pid) == "yes"


def test_split_votes_do_not_reach_threshold(prediction_service, prediction_repo):
    """Votes split across outcomes never give either side 3 -> cannot resolve."""
    pid = _make_closed_prediction(prediction_repo)
    prediction_service.add_resolution_vote(pid, user_id=10, outcome="yes", is_admin=False)
    prediction_service.add_resolution_vote(pid, user_id=11, outcome="yes", is_admin=False)
    res = prediction_service.add_resolution_vote(pid, user_id=12, outcome="no", is_admin=False)
    assert res["can_resolve"] is False
    assert prediction_service.can_resolve(pid) is False
    assert prediction_service.get_pending_outcome(pid) is None


def test_single_admin_vote_resolves_immediately(prediction_service, prediction_repo):
    """An admin vote satisfies _check_can_resolve on its own (1 vote)."""
    pid = _make_closed_prediction(prediction_repo)
    res = prediction_service.add_resolution_vote(pid, user_id=ADMIN_ID, outcome="no")
    assert res["is_admin"] is True
    assert res["can_resolve"] is True
    assert res["no_count"] == 1


def test_can_resolve_false_for_open_and_cancelled(prediction_service, prediction_repo):
    """can_resolve only applies to open/locked; cancelled never resolves.

    A market with 3 yes votes can_resolve while open/locked, but once cancelled
    the status guard short-circuits to False even though the votes remain.
    """
    pid = _make_closed_prediction(prediction_repo)
    for uid in (10, 11, 12):
        prediction_service.add_resolution_vote(pid, user_id=uid, outcome="yes", is_admin=False)
    assert prediction_service.can_resolve(pid, "yes") is True

    prediction_repo.update_prediction_status(pid, "cancelled")
    assert prediction_service.can_resolve(pid, "yes") is False

    # Unknown prediction id -> False, not an error.
    assert prediction_service.can_resolve(999999, "yes") is False


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
