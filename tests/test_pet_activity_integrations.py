"""Focused tests for gameplay-to-upbringing activity adapters."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import MagicMock, call

import pytest

from domain.models.pending_match_state import PendingMatchState
from domain.pet_evolution import PetActivity
from services.betting_service import BettingService
from services.duel_service import DuelService
from services.pet_brawl_service import PetBrawlService
from services.prediction_service import PredictionService
from tests.conftest import TEST_GUILD_ID
from utils.pet_activity import record_pet_activity

T0 = 1_800_000_000


@pytest.mark.asyncio
async def test_async_command_adapter_is_a_quiet_noop_when_pets_are_disabled():
    bot = SimpleNamespace(pet_evolution_service=None)

    assert (
        await record_pet_activity(
            bot,
            100,
            TEST_GUILD_ID,
            PetActivity.DIG_COMPLETED,
            "dig:1",
        )
        == 0
    )


@pytest.mark.asyncio
async def test_async_command_adapter_runs_the_sync_boundary_off_loop():
    evolution = MagicMock()
    evolution.record_activity.return_value = 2
    bot = SimpleNamespace(pet_evolution_service=evolution)

    assert (
        await record_pet_activity(
            bot,
            100,
            TEST_GUILD_ID,
            PetActivity.DIG_COMPLETED,
            "dig:1",
            occurred_at=T0,
        )
        == 2
    )
    evolution.record_activity.assert_called_once_with(
        100,
        TEST_GUILD_ID,
        PetActivity.DIG_COMPLETED,
        "dig:1",
        occurred_at=T0,
    )


def test_successful_bet_records_fortune_with_persisted_bet_id():
    bet_repo = MagicMock()
    bet_repo.VALID_TEAMS = {"radiant", "dire"}
    bet_repo.get_total_bets_by_guild.return_value = {"radiant": 0, "dire": 0}
    bet_repo.place_bet_against_pending_match_atomic.return_value = 77
    evolution = MagicMock()
    service = BettingService(
        bet_repo,
        MagicMock(),
        pet_evolution_service=evolution,
    )
    pending = PendingMatchState(
        shuffle_timestamp=T0 - 60,
        bet_lock_until=T0 + 60,
        pending_match_id=12,
    )

    service.place_bet(
        TEST_GUILD_ID,
        100,
        "radiant",
        10,
        pending,
    )

    evolution.record_activity.assert_called_once_with(
        100,
        TEST_GUILD_ID,
        PetActivity.BET_PLACED,
        "bet:77",
    )


def test_prediction_buy_records_fortune_with_atomic_trade_identity():
    prediction_repo = MagicMock()
    prediction_repo.buy_contracts_atomic.return_value = {
        "guild_id": TEST_GUILD_ID,
        "trade_id": 88,
        "trade_time": T0,
    }
    evolution = MagicMock()
    service = PredictionService(
        prediction_repo,
        MagicMock(),
        pet_evolution_service=evolution,
    )

    result = service.buy_contracts(9, 100, "yes", 2)

    assert result["trade_id"] == 88
    evolution.record_activity.assert_called_once_with(
        100,
        TEST_GUILD_ID,
        PetActivity.PREDICTION_TRADED,
        "prediction-trade:88",
        occurred_at=T0,
    )


def test_resolved_duel_records_competition_for_both_participants():
    challenge = SimpleNamespace(
        challenge_id=42,
        challenger_id=100,
        recipient_id=200,
    )
    resolved = SimpleNamespace(**vars(challenge), winner_id=100)
    duel_repo = MagicMock()
    duel_repo.get_challenge.return_value = challenge
    duel_repo.resolve_atomic.return_value = resolved
    evolution = MagicMock()
    service = DuelService(
        duel_repo,
        clock=lambda: T0,
        pet_evolution_service=evolution,
    )

    assert (
        service.resolve(
            TEST_GUILD_ID,
            actor_id=999,
            challenge_id=42,
            outcome="challenger_victory",
        )
        is resolved
    )

    assert evolution.record_activity.call_args_list == [
        call(
            100,
            TEST_GUILD_ID,
            PetActivity.DUEL_COMPLETED,
            "duel:42",
            occurred_at=T0,
        ),
        call(
            200,
            TEST_GUILD_ID,
            PetActivity.DUEL_COMPLETED,
            "duel:42",
            occurred_at=T0,
        ),
    ]


def test_settled_pet_brawl_records_competition_for_both_owners():
    now = T0
    pet_service = MagicMock()
    pet_service._now.return_value = now
    pet_service.decay_per_day = 20
    brawl_repo = MagicMock()
    brawl_repo.settle_brawl_atomic.return_value = {
        "winner_delta": 10,
        "loser_delta": -15,
        "winner_xp_delta": 2,
        "loser_xp_delta": 1,
        "winner_stat_gain": None,
        "loser_stat_gain": None,
        "payout": 0,
        "wager": 0,
        "fee": 0,
        "personality_event_key": None,
    }
    brawl_repo.get_records_for.return_value = {1: (0, 0), 2: (0, 0)}
    evolution = MagicMock()
    service = PetBrawlService(
        pet_service,
        brawl_repo,
        pet_evolution_service=evolution,
    )
    a = SimpleNamespace(
        owner_id=100,
        pet_id=1,
        species_id="common_cama",
        name="A",
        training_str=0,
        training_int=0,
        training_dex=0,
    )
    b = SimpleNamespace(
        owner_id=200,
        pet_id=2,
        species_id="common_cama",
        name="B",
        training_str=0,
        training_int=0,
        training_dex=0,
    )
    persisted = {
        1: SimpleNamespace(
            training_xp=0,
            training_str=0,
            training_int=0,
            training_dex=0,
        ),
        2: SimpleNamespace(
            training_xp=0,
            training_str=0,
            training_int=0,
            training_dex=0,
        ),
    }
    pet_service.pet_repo.get_pet_by_id.side_effect = (
        lambda pet_id, _guild_id: persisted[pet_id]
    )
    state = SimpleNamespace(winner="a", a=a, b=b)

    result = service.settle(55, TEST_GUILD_ID, state, rounds=3)

    assert result.success
    assert evolution.record_activity.call_args_list == [
        call(
            100,
            TEST_GUILD_ID,
            PetActivity.PET_BRAWL_COMPLETED,
            "pet-brawl:55",
            occurred_at=now,
        ),
        call(
            200,
            TEST_GUILD_ID,
            PetActivity.PET_BRAWL_COMPLETED,
            "pet-brawl:55",
            occurred_at=now,
        ),
    ]
