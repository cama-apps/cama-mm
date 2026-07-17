"""Tests for the duel challenge service facade."""

from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

from domain.models.duel import DuelDueKind, DuelResolution, DuelTrial
from services.duel_service import (
    CHALLENGER_COOLDOWN_SECONDS,
    RECIPIENT_COOLDOWN_SECONDS,
    RESPONSE_SECONDS,
    DuelService,
)

GUILD_ID = 123


@pytest.fixture
def duel_repo_mock():
    repo = MagicMock()
    repo.pending = SimpleNamespace(challenge_id=7)
    repo.challenge = SimpleNamespace(challenger_id=1, recipient_id=2)
    repo.get_pending_for_recipient.return_value = repo.pending
    repo.get_challenge.return_value = repo.challenge
    return repo


def test_service_issue_routes_policy_and_integer_clock(duel_repo_mock):
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000.75)

    service.issue(GUILD_ID, 20, 1, 2, 500)

    duel_repo_mock.create_challenge_atomic.assert_called_once_with(
        GUILD_ID,
        20,
        1,
        2,
        500,
        1_000_000,
        CHALLENGER_COOLDOWN_SECONDS,
        RECIPIENT_COOLDOWN_SECONDS,
        RESPONSE_SECONDS,
        1,
    )


def test_service_rejects_bot_before_repository_issue(duel_repo_mock):
    clock = MagicMock(return_value=1_000_000)
    service = DuelService(duel_repo_mock, clock=clock)

    with pytest.raises(ValueError, match="Bots cannot answer"):
        service.issue(GUILD_ID, 20, 1, 2, 500, recipient_is_bot=True)

    clock.assert_not_called()
    duel_repo_mock.create_challenge_atomic.assert_not_called()


def test_service_routes_response_choices(duel_repo_mock):
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000)

    service.respond(GUILD_ID, 2, DuelTrial.TRIAL_BY_COMBAT)

    duel_repo_mock.accept_atomic.assert_called_once_with(
        duel_repo_mock.pending.challenge_id,
        GUILD_ID,
        2,
        DuelTrial.TRIAL_BY_COMBAT,
        1_000_000,
        2,
    )


def test_service_routes_decline_choice(duel_repo_mock):
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000)

    service.respond(GUILD_ID, 2, "decline")

    duel_repo_mock.decline_atomic.assert_called_once_with(
        duel_repo_mock.pending.challenge_id,
        GUILD_ID,
        2,
        1_000_000,
        2,
    )


def test_service_rejects_response_without_pending_challenge(duel_repo_mock):
    duel_repo_mock.get_pending_for_recipient.return_value = None
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000)

    with pytest.raises(ValueError, match="no pending duel challenge"):
        service.respond(GUILD_ID, 2, DuelTrial.TRIAL_OF_FIVE)

    duel_repo_mock.accept_atomic.assert_not_called()
    duel_repo_mock.decline_atomic.assert_not_called()


@pytest.mark.parametrize(
    ("outcome", "winner_id"),
    [
        (DuelResolution.CHALLENGER_VICTORY, 1),
        (DuelResolution.RECIPIENT_VICTORY, 2),
    ],
)
def test_service_resolve_maps_victory_to_participant(
    duel_repo_mock, outcome, winner_id
):
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000)

    service.resolve(GUILD_ID, 99, 7, outcome)

    duel_repo_mock.resolve_atomic.assert_called_once_with(
        7, GUILD_ID, winner_id, 1_000_000, 99
    )


def test_service_resolve_maps_void_to_none(duel_repo_mock):
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000)

    service.resolve(GUILD_ID, 99, 7, DuelResolution.VOID)

    duel_repo_mock.resolve_atomic.assert_called_once_with(
        7, GUILD_ID, None, 1_000_000, 99
    )


def test_service_rejects_resolution_for_missing_challenge(duel_repo_mock):
    duel_repo_mock.get_challenge.return_value = None
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000)

    with pytest.raises(ValueError, match="Challenge not found"):
        service.resolve(GUILD_ID, 99, 7, DuelResolution.VOID)

    duel_repo_mock.resolve_atomic.assert_not_called()


def test_service_bind_message_is_thin_wrapper(duel_repo_mock):
    expected = object()
    duel_repo_mock.bind_message.return_value = expected
    service = DuelService(duel_repo_mock)

    assert service.bind_message(7, GUILD_ID, 44) is expected
    duel_repo_mock.bind_message.assert_called_once_with(7, GUILD_ID, 44)


def test_service_mark_delivery_failed_uses_integer_clock(duel_repo_mock):
    expected = object()
    duel_repo_mock.mark_delivery_failed_atomic.return_value = expected
    service = DuelService(duel_repo_mock, clock=lambda: 1_000_000.75)

    assert service.mark_delivery_failed(7, GUILD_ID, 1) is expected
    duel_repo_mock.mark_delivery_failed_atomic.assert_called_once_with(
        7, GUILD_ID, 1_000_000, 1
    )


def test_service_list_outstanding_is_thin_wrapper(duel_repo_mock):
    expected = [object()]
    duel_repo_mock.list_outstanding.return_value = expected
    service = DuelService(duel_repo_mock)

    assert service.list_outstanding(GUILD_ID) is expected
    duel_repo_mock.list_outstanding.assert_called_once_with(GUILD_ID)


def test_service_list_pending_all_is_thin_wrapper(duel_repo_mock):
    expected = [object()]
    duel_repo_mock.list_pending_all.return_value = expected
    service = DuelService(duel_repo_mock)

    assert service.list_pending_all() is expected
    duel_repo_mock.list_pending_all.assert_called_once_with()


def test_service_get_due_challenge_ids_is_thin_wrapper(duel_repo_mock):
    expected = [(7, GUILD_ID)]
    duel_repo_mock.get_due_challenge_ids.return_value = expected
    service = DuelService(duel_repo_mock)

    assert service.get_due_challenge_ids(1_000_000) is expected
    duel_repo_mock.get_due_challenge_ids.assert_called_once_with(1_000_000)


def test_service_process_due_returns_expiry_before_reminder(duel_repo_mock):
    expired = object()
    duel_repo_mock.expire_atomic.return_value = expired
    service = DuelService(duel_repo_mock)

    result = service.process_due(7, GUILD_ID, 1_000_000)

    assert result.kind is DuelDueKind.EXPIRED
    assert result.challenge is expired
    duel_repo_mock.expire_atomic.assert_called_once_with(7, GUILD_ID, 1_000_000)
    duel_repo_mock.claim_reminder_atomic.assert_not_called()


def test_service_process_due_claims_reminder_when_not_expired(duel_repo_mock):
    reminder = object()
    duel_repo_mock.expire_atomic.return_value = None
    duel_repo_mock.claim_reminder_atomic.return_value = reminder
    service = DuelService(duel_repo_mock)

    assert service.process_due(7, GUILD_ID, 1_000_000) is reminder
    duel_repo_mock.expire_atomic.assert_called_once_with(7, GUILD_ID, 1_000_000)
    duel_repo_mock.claim_reminder_atomic.assert_called_once_with(
        7, GUILD_ID, 1_000_000
    )
