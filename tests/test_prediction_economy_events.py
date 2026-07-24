"""Daily economy-event integration for prediction-market mechanics."""

from __future__ import annotations

import json
import random
from types import SimpleNamespace

from config import (
    PREDICTION_CONTRACT_VALUE,
    PREDICTION_REFRESH_SIZE_PER_LEVEL,
    PREDICTION_SIZE_PER_LEVEL,
)
from repositories.prediction_repository import PredictionRepository
from services.prediction_service import PredictionService
from tests.conftest import TEST_GUILD_ID


class StubEconomyEventService:
    def __init__(self, *, payout: float = 1.0, depth: float = 1.0, spread: int = 0):
        self.effects = SimpleNamespace(
            prediction_payout_multiplier=payout,
            prediction_depth_multiplier=depth,
            prediction_spread_ticks_delta=spread,
        )
        self.guild_ids: list[int] = []

    def get_effects(self, guild_id: int):
        self.guild_ids.append(guild_id)
        return self.effects


def _service(repo_db_path, player_repository, events):
    repo = PredictionRepository(repo_db_path)
    service = PredictionService(
        prediction_repo=repo,
        player_repo=player_repository,
        economy_event_service=events,
    )
    return service, repo


def _add_player(player_repository, discord_id: int, balance: int = 1_000):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"user{discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    player_repository.update_balance(discord_id, TEST_GUILD_ID, balance)


def test_event_preserves_depth_while_modifying_new_and_refreshed_spreads(
    repo_db_path, player_repository, monkeypatch
):
    events = StubEconomyEventService(depth=0.5, spread=2)
    service, repo = _service(repo_db_path, player_repository, events)

    created = service.create_orderbook_prediction(
        TEST_GUILD_ID, 1, "Will Ravage affect the book?", initial_fair=50
    )
    prediction_id = created["prediction_id"]
    book = repo.get_book(prediction_id)
    assert book["yes_asks"] == [
        (54, PREDICTION_SIZE_PER_LEVEL),
        (55, PREDICTION_SIZE_PER_LEVEL),
        (56, PREDICTION_SIZE_PER_LEVEL),
    ]
    assert book["yes_bids"][0] == (46, PREDICTION_SIZE_PER_LEVEL)

    monkeypatch.setattr(random, "randint", lambda _lo, _hi: 0)
    result = service.refresh_market(prediction_id)
    book = repo.get_book(prediction_id)
    asks = dict(book["yes_asks"])
    # Refresh base spread 4 + event delta 2, with normal quote depth.
    assert asks[56] == PREDICTION_SIZE_PER_LEVEL + PREDICTION_REFRESH_SIZE_PER_LEVEL
    assert asks[57] == PREDICTION_REFRESH_SIZE_PER_LEVEL
    assert result["economy_event_modifiers"]["prediction_depth_multiplier"] == 1.0
    assert result["economy_event_modifiers"]["prediction_spread_ticks"] == 6
    assert events.guild_ids == [TEST_GUILD_ID, TEST_GUILD_ID]


def test_event_ladder_spread_is_clamped_without_changing_depth(
    repo_db_path, player_repository
):
    events = StubEconomyEventService(depth=-4, spread=-999)
    service, repo = _service(repo_db_path, player_repository, events)

    created = service.create_orderbook_prediction(
        TEST_GUILD_ID, 1, "Will Global Silence empty the book?", initial_fair=50
    )

    assert repo.get_book(created["prediction_id"])["yes_asks"] == [
        (51, PREDICTION_SIZE_PER_LEVEL),
        (52, PREDICTION_SIZE_PER_LEVEL),
        (53, PREDICTION_SIZE_PER_LEVEL),
    ]
    modifiers = created["economy_event_modifiers"]
    assert modifiers["prediction_depth_multiplier"] == 1.0
    assert modifiers["prediction_size_per_level"] == PREDICTION_SIZE_PER_LEVEL
    assert modifiers["prediction_spread_ticks"] == 1


def test_event_multiplier_changes_atomic_resolution_and_audit(
    repo_db_path, player_repository
):
    events = StubEconomyEventService(payout=0.75)
    service, repo = _service(repo_db_path, player_repository, events)
    _add_player(player_repository, 1)
    prediction_id = service.create_orderbook_prediction(
        TEST_GUILD_ID, 1, "Will Doom reduce the payout?", initial_fair=50
    )["prediction_id"]
    service.buy_contracts(prediction_id, 1, "yes", 5)
    position = repo.get_position(prediction_id, 1)
    balance_before_resolution = player_repository.get_balance(1, TEST_GUILD_ID)

    result = service.resolve_orderbook(prediction_id, "yes")

    expected_payout = round(5 * PREDICTION_CONTRACT_VALUE * 0.75)
    assert result["total_payout"] == expected_payout
    assert result["payout_multiplier"] == 0.75
    assert player_repository.get_balance(1, TEST_GUILD_ID) == (
        balance_before_resolution + expected_payout
    )
    assert result["lp_pnl"] == position["yes_cost_basis_total"] - expected_payout

    with repo.connection() as conn:
        row = conn.execute(
            "SELECT delta, metadata FROM economy_ledger_entries "
            "WHERE source = 'prediction_resolution' AND related_id = ?",
            (str(prediction_id),),
        ).fetchone()
    metadata = json.loads(row["metadata"])
    assert row["delta"] == expected_payout
    assert metadata["base_gross_payout"] == 50
    assert metadata["gross_payout"] == expected_payout
    assert metadata["payout_multiplier"] == 0.75

    stats = repo.get_user_orderbook_stats(1, TEST_GUILD_ID)
    assert stats["realized_pnl"] == expected_payout - position["yes_cost_basis_total"]
    assert repo.get_player_orderbook_pnl_history(1, TEST_GUILD_ID)[0]["delta"] == (
        expected_payout - position["yes_cost_basis_total"]
    )


def test_rollback_reverses_event_modified_payout(repo_db_path, player_repository):
    events = StubEconomyEventService(payout=0.75)
    service, repo = _service(repo_db_path, player_repository, events)
    _add_player(player_repository, 1)
    prediction_id = service.create_orderbook_prediction(
        TEST_GUILD_ID, 1, "Will False Promise be reversed?", initial_fair=50
    )["prediction_id"]
    service.buy_contracts(prediction_id, 1, "yes", 5)
    balance_before_resolution = player_repository.get_balance(1, TEST_GUILD_ID)
    lp_before_resolution = repo.get_prediction(prediction_id)["lp_pnl"]
    resolution = service.resolve_orderbook(prediction_id, "yes")

    # The active event may change before an admin rollback. The stored
    # settlement audit, rather than today's modifier, is the rollback source.
    events.effects.prediction_payout_multiplier = 1.0
    rolled_back = service.rollback_orderbook(prediction_id, TEST_GUILD_ID)

    assert rolled_back["total_reversed"] == resolution["total_payout"]
    assert player_repository.get_balance(1, TEST_GUILD_ID) == balance_before_resolution
    assert repo.get_prediction(prediction_id)["lp_pnl"] == lp_before_resolution
    assert repo.get_prediction(prediction_id)["status"] == "open"


def test_zero_payout_event_remains_auditable_and_rollback_safe(
    repo_db_path, player_repository
):
    events = StubEconomyEventService(payout=0)
    service, repo = _service(repo_db_path, player_repository, events)
    _add_player(player_repository, 1)
    prediction_id = service.create_orderbook_prediction(
        TEST_GUILD_ID, 1, "Will Doom mute the payout entirely?", initial_fair=50
    )["prediction_id"]
    service.buy_contracts(prediction_id, 1, "yes", 1)
    position = repo.get_position(prediction_id, 1)
    balance_before_resolution = player_repository.get_balance(1, TEST_GUILD_ID)

    result = service.resolve_orderbook(prediction_id, "yes")

    assert result["total_payout"] == 0
    assert player_repository.get_balance(1, TEST_GUILD_ID) == balance_before_resolution
    assert repo.get_user_orderbook_stats(1, TEST_GUILD_ID)["realized_pnl"] == (
        -position["yes_cost_basis_total"]
    )
    with repo.connection() as conn:
        audit = conn.execute(
            "SELECT delta, metadata FROM economy_ledger_entries "
            "WHERE source = 'prediction_resolution' AND related_id = ?",
            (str(prediction_id),),
        ).fetchone()
    assert audit["delta"] == 0
    assert json.loads(audit["metadata"])["payout_multiplier"] == 0

    rolled_back = service.rollback_orderbook(prediction_id, TEST_GUILD_ID)
    assert rolled_back["total_reversed"] == 0
    assert repo.get_prediction(prediction_id)["status"] == "open"
