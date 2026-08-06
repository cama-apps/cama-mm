"""Daily reserve-funded betting pools for each Dota lobby type."""

from repositories.economy_event_repository import EconomyEventRepository
from repositories.loan_repository import LoanRepository
from tests.conftest import TEST_GUILD_ID


def test_daily_funding_moves_exact_pair_and_preserves_monetary_stock(repo_db_path):
    loan_repo = LoanRepository(repo_db_path)
    economy_repo = EconomyEventRepository(repo_db_path)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 500)
    stock_before = economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"]

    result = loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-05", 100)

    assert result == {
        "processed_dates": ["2026-08-05"],
        "funded_dates": ["2026-08-05"],
        "skipped_dates": [],
        "open": 100,
        "lowskill": 100,
    }
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 300
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 100,
        "lowskill": 100,
    }
    assert economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"] == stock_before


def test_daily_funding_skips_both_pools_when_reserve_is_below_pair_cost(repo_db_path):
    loan_repo = LoanRepository(repo_db_path)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 199)

    first = loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-05", 100)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 200)
    retry = loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-05", 100)

    assert first["funded_dates"] == []
    assert first["skipped_dates"] == ["2026-08-05"]
    assert retry["processed_dates"] == []
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 399
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 0,
        "lowskill": 0,
    }


def test_daily_funding_stacks_missed_game_dates_oldest_first(repo_db_path):
    loan_repo = LoanRepository(repo_db_path)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 600)

    loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-03", 100)
    result = loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-05", 100)

    assert result["processed_dates"] == ["2026-08-04", "2026-08-05"]
    assert result["funded_dates"] == ["2026-08-04", "2026-08-05"]
    assert result["skipped_dates"] == []
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 300,
        "lowskill": 300,
    }


def test_daily_funding_is_guild_scoped(repo_db_path):
    loan_repo = LoanRepository(repo_db_path)
    other_guild = TEST_GUILD_ID + 1
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 200)
    loan_repo.add_to_nonprofit_fund(other_guild, 199)

    loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-05", 100)
    loan_repo.fund_first_game_pools(other_guild, "2026-08-05", 100)

    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 100,
        "lowskill": 100,
    }
    assert loan_repo.get_first_game_pool_balances(other_guild) == {
        "open": 0,
        "lowskill": 0,
    }


def test_claims_are_once_per_game_date_and_independent_by_lobby(repo_db_path):
    loan_repo = LoanRepository(repo_db_path)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 400)
    loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-05", 100)

    assert (
        loan_repo.claim_first_game_pool(
            TEST_GUILD_ID, "open", "2026-08-05", pending_match_id=501
        )
        == 100
    )
    assert (
        loan_repo.claim_first_game_pool(
            TEST_GUILD_ID, "open", "2026-08-05", pending_match_id=502
        )
        == 0
    )
    assert (
        loan_repo.claim_first_game_pool(
            TEST_GUILD_ID, "lowskill", "2026-08-05", pending_match_id=503
        )
        == 100
    )
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 0,
        "lowskill": 0,
    }


def test_next_game_date_claim_receives_newly_stacked_balance(repo_db_path):
    loan_repo = LoanRepository(repo_db_path)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 400)
    loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-05", 100)
    assert (
        loan_repo.claim_first_game_pool(
            TEST_GUILD_ID, "open", "2026-08-05", pending_match_id=601
        )
        == 100
    )

    loan_repo.fund_first_game_pools(TEST_GUILD_ID, "2026-08-06", 100)

    assert (
        loan_repo.claim_first_game_pool(
            TEST_GUILD_ID, "open", "2026-08-06", pending_match_id=602
        )
        == 100
    )
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID)["open"] == 0
