"""Focused regressions for bulk loan discovery during match finalization."""

from types import SimpleNamespace
from unittest.mock import MagicMock, call, patch

from repositories.loan_repository import LoanRepository
from services.loan_service import LoanService, RepaymentResult
from services.match.recording_mixin import RecordingMixin
from services.result import Result
from tests.conftest import TEST_GUILD_ID


def test_outstanding_borrower_ids_are_bulk_filtered_and_guild_scoped(repo_db_path):
    repo = LoanRepository(repo_db_path)
    borrower_id = 8101
    fee_only_id = 8102
    other_guild_borrower_id = 8103
    other_guild_id = TEST_GUILD_ID + 1

    repo.upsert_state(
        borrower_id,
        TEST_GUILD_ID,
        outstanding_principal=50,
        outstanding_fee=10,
    )
    repo.upsert_state(
        fee_only_id,
        TEST_GUILD_ID,
        outstanding_principal=0,
        outstanding_fee=10,
    )
    repo.upsert_state(
        other_guild_borrower_id,
        other_guild_id,
        outstanding_principal=75,
        outstanding_fee=15,
    )

    requested_ids = [
        borrower_id,
        fee_only_id,
        other_guild_borrower_id,
        8999,
        borrower_id,
    ]
    with patch.object(repo, "connection", wraps=repo.connection) as connection:
        assert repo.get_outstanding_borrower_ids(requested_ids, TEST_GUILD_ID) == {
            borrower_id
        }
    connection.assert_called_once_with()

    assert repo.get_outstanding_borrower_ids(requested_ids, other_guild_id) == {
        other_guild_borrower_id
    }


def test_outstanding_borrower_ids_empty_input_skips_connection(repo_db_path):
    repo = LoanRepository(repo_db_path)

    with patch.object(repo, "connection") as connection:
        assert repo.get_outstanding_borrower_ids([], TEST_GUILD_ID) == set()
    connection.assert_not_called()


def test_loan_service_delegates_bulk_borrower_lookup():
    loan_repo = MagicMock()
    loan_repo.get_outstanding_borrower_ids.return_value = {2}
    service = LoanService(loan_repo, MagicMock())

    assert service.get_outstanding_borrower_ids([1, 2, 2], 77) == {2}
    loan_repo.get_outstanding_borrower_ids.assert_called_once_with([1, 2, 2], 77)


def test_match_repayment_bulk_checks_once_and_attempts_each_borrower_once():
    loan_service = MagicMock()
    loan_service.get_outstanding_borrower_ids.return_value = {2, 4, 5}
    loan_service.execute_repayment.side_effect = [
        Result.fail("Loan was already repaid"),
        Result.ok(
            RepaymentResult(
                principal=40,
                fee=8,
                total_repaid=48,
                balance_before=100,
                new_balance=52,
                nonprofit_total=8,
            )
        ),
        Result.ok(
            RepaymentResult(
                principal=25,
                fee=5,
                total_repaid=30,
                balance_before=50,
                new_balance=20,
                nonprofit_total=13,
            )
        ),
    ]
    subject = SimpleNamespace(loan_service=loan_service)
    participant_ids = [1, 2, 2, 3, 4, 5, 99]

    repayments = RecordingMixin._repay_outstanding_loans(
        subject, participant_ids, guild_id=77
    )

    loan_service.get_outstanding_borrower_ids.assert_called_once_with(
        participant_ids, 77
    )
    assert loan_service.execute_repayment.call_args_list == [
        call(2, 77),
        call(4, 77),
        call(5, 77),
    ]
    assert [repayment["player_id"] for repayment in repayments] == [4, 5]
    assert repayments[0]["total_repaid"] == 48
    assert repayments[1]["total_repaid"] == 30
