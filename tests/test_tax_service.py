import json

import pytest

from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.economy_ledger_repository import EconomyLedgerRepository
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from repositories.tax_repository import TaxRepository
from services.tax_service import TaxService
from tests.conftest import TEST_GUILD_ID

TAX_MAN_ID = 101
TARGET_ID = 303
OTHER_TARGET_ID = 404


@pytest.fixture
def tax_stack(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    loan_repo = LoanRepository(repo_db_path)
    ledger_repo = EconomyLedgerRepository(repo_db_path)
    tax_repo = TaxRepository(repo_db_path)
    prediction_repo = PredictionRepository(repo_db_path)
    bankruptcy_repo = BankruptcyRepository(repo_db_path)
    service = TaxService(
        tax_repo=tax_repo,
        ledger_repo=ledger_repo,
        player_repo=player_repo,
        loan_repo=loan_repo,
        bankruptcy_repo=bankruptcy_repo,
    )
    return {
        "player_repo": player_repo,
        "loan_repo": loan_repo,
        "ledger_repo": ledger_repo,
        "tax_repo": tax_repo,
        "prediction_repo": prediction_repo,
        "bankruptcy_repo": bankruptcy_repo,
        "service": service,
    }


def _add_player(player_repo: PlayerRepository, discord_id: int, balance: int) -> None:
    player_repo.add(discord_id, f"user-{discord_id}", TEST_GUILD_ID)
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def _insert_ledger_rows(tax_repo: TaxRepository, rows: list[tuple[int, int]]) -> None:
    with tax_repo.connection() as conn:
        conn.execute("DELETE FROM economy_ledger_entries")
        for idx, account_id in rows:
            conn.execute(
                """
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, reason, created_at
                )
                VALUES (?, 'player', ?, ?, ?, ?, 'test', ?, ?)
                """,
                (
                    TEST_GUILD_ID,
                    account_id,
                    idx + 1,
                    idx * 10,
                    idx * 10 + idx + 1,
                    f"ledger-entry-{idx}",
                    1_700_000_000 + idx,
                ),
            )


def _clear_ledger(tax_repo: TaxRepository) -> None:
    with tax_repo.connection() as conn:
        conn.execute("DELETE FROM economy_ledger_entries")


def test_recent_ledger_supports_offset_and_count(tax_stack):
    service = tax_stack["service"]
    _insert_ledger_rows(
        tax_stack["tax_repo"],
        [(idx, TARGET_ID if idx < 6 else OTHER_TARGET_ID) for idx in range(9)],
    )

    rows = service.get_recent_ledger(TEST_GUILD_ID, limit=3, offset=3)

    assert [row["reason"] for row in rows] == [
        "ledger-entry-5",
        "ledger-entry-4",
        "ledger-entry-3",
    ]
    assert service.count_ledger_entries(TEST_GUILD_ID) == 9
    assert service.count_ledger_entries(TEST_GUILD_ID, user_id=TARGET_ID) == 6


def test_player_snapshot_includes_loans_and_dark_bargains(tax_stack):
    player_repo = tax_stack["player_repo"]
    loan_repo = tax_stack["loan_repo"]
    tax_repo = tax_stack["tax_repo"]
    service = tax_stack["service"]
    _add_player(player_repo, TARGET_ID, 250)
    loan_repo.upsert_state(
        TARGET_ID,
        TEST_GUILD_ID,
        outstanding_principal=70,
        outstanding_fee=14,
        total_loans_taken=2,
        total_fees_paid=20,
    )

    with tax_repo.connection() as conn:
        conn.execute(
            """
            INSERT INTO manashop_buffs
            (discord_id, guild_id, buff_type, target_id, granted_at, expires_at, triggered, data)
            VALUES (?, ?, 'dark_bargain', NULL, ?, ?, 0, ?)
            """,
            (
                TARGET_ID,
                TEST_GUILD_ID,
                1_700_000_000,
                2_000_000_000,
                '{"amount_due": 700, "default_penalty_games": 5}',
            ),
        )

    snapshot = service.get_player_snapshot(TARGET_ID, TEST_GUILD_ID)

    assert snapshot["balance"] == 250
    assert snapshot["loan_total"] == 84
    assert snapshot["dark_bargain_due"] == 700
    assert snapshot["effective_obligations"] == 784


def test_levy_fine_caps_at_obligations_and_can_make_balance_negative(tax_stack):
    player_repo = tax_stack["player_repo"]
    loan_repo = tax_stack["loan_repo"]
    service = tax_stack["service"]
    tax_repo = tax_stack["tax_repo"]
    _add_player(player_repo, TARGET_ID, 7)
    loan_repo.upsert_state(
        TARGET_ID,
        TEST_GUILD_ID,
        outstanding_principal=70,
        outstanding_fee=14,
    )
    _clear_ledger(tax_repo)

    result = service.levy_fine(
        TARGET_ID,
        TEST_GUILD_ID,
        amount=100,
        actor_id=TAX_MAN_ID,
        reason="failure to file",
        now=1_700_000_000,
    )

    assert result["status"] == "ok"
    assert result["requested_amount"] == 100
    assert result["applied_amount"] == 84
    assert result["outstanding_obligations"] == 84
    assert result["capped"] is True
    assert result["balance_before"] == 7
    assert result["balance_after"] == -77
    assert result["nonprofit_credit"] == 84
    assert result["reserve_credit"] == 84
    assert player_repo.get_balance(TARGET_ID, TEST_GUILD_ID) == -77
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 84

    rows = service.get_recent_ledger(TEST_GUILD_ID, limit=1, user_id=TARGET_ID)
    assert len(rows) == 1
    assert rows[0]["delta"] == -84
    assert rows[0]["source"] == "tax_fine"
    assert rows[0]["actor_id"] == TAX_MAN_ID
    assert rows[0]["related_type"] == "tax_fine"
    assert rows[0]["related_id"] == str(TARGET_ID)
    assert rows[0]["reason"] == "tax fine: failure to file"
    assert json.loads(rows[0]["metadata"]) == {
        "requested_amount": 100,
        "applied_amount": 84,
        "outstanding_obligations": 84,
        "cooldown_seconds": 2_592_000,
    }

    all_rows = service.get_recent_ledger(TEST_GUILD_ID, limit=2)
    assert [(row["account_type"], row["delta"], row["source"]) for row in all_rows] == [
        ("nonprofit", 84, "tax_fine"),
        ("player", -84, "tax_fine"),
    ]
    assert all_rows[0]["reason"] == "tax fine: failure to file reserve credit"
    assert json.loads(all_rows[0]["metadata"]) == {
        "requested_amount": 100,
        "applied_amount": 84,
        "outstanding_obligations": 84,
        "cooldown_seconds": 2_592_000,
    }


def test_levy_fine_enforces_per_player_cooldown_without_new_ledger(tax_stack):
    player_repo = tax_stack["player_repo"]
    loan_repo = tax_stack["loan_repo"]
    service = tax_stack["service"]
    tax_repo = tax_stack["tax_repo"]
    _add_player(player_repo, TARGET_ID, 20)
    loan_repo.upsert_state(
        TARGET_ID,
        TEST_GUILD_ID,
        outstanding_principal=25,
        outstanding_fee=0,
    )
    _clear_ledger(tax_repo)

    first = service.levy_fine(
        TARGET_ID,
        TEST_GUILD_ID,
        amount=5,
        actor_id=TAX_MAN_ID,
        now=1_000,
        cooldown_seconds=100,
    )
    second = service.levy_fine(
        TARGET_ID,
        TEST_GUILD_ID,
        amount=5,
        actor_id=TAX_MAN_ID,
        now=1_050,
        cooldown_seconds=100,
    )

    assert first["status"] == "ok"
    assert second["status"] == "cooldown"
    assert second["next_fine_at"] == 1_100
    assert second["cooldown_remaining"] == 50
    assert player_repo.get_balance(TARGET_ID, TEST_GUILD_ID) == 15
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 5
    assert service.count_ledger_entries(TEST_GUILD_ID, user_id=TARGET_ID) == 1
    assert service.count_ledger_entries(TEST_GUILD_ID) == 2


def test_levy_fine_allows_different_players_during_cooldown(tax_stack):
    player_repo = tax_stack["player_repo"]
    loan_repo = tax_stack["loan_repo"]
    service = tax_stack["service"]
    _add_player(player_repo, TARGET_ID, 20)
    _add_player(player_repo, OTHER_TARGET_ID, 20)
    loan_repo.upsert_state(
        TARGET_ID,
        TEST_GUILD_ID,
        outstanding_principal=25,
        outstanding_fee=0,
    )
    loan_repo.upsert_state(
        OTHER_TARGET_ID,
        TEST_GUILD_ID,
        outstanding_principal=25,
        outstanding_fee=0,
    )

    first = service.levy_fine(
        TARGET_ID,
        TEST_GUILD_ID,
        amount=5,
        actor_id=TAX_MAN_ID,
        now=1_000,
        cooldown_seconds=100,
    )
    second = service.levy_fine(
        OTHER_TARGET_ID,
        TEST_GUILD_ID,
        amount=5,
        actor_id=TAX_MAN_ID,
        now=1_050,
        cooldown_seconds=100,
    )

    assert first["status"] == "ok"
    assert second["status"] == "ok"
    assert player_repo.get_balance(TARGET_ID, TEST_GUILD_ID) == 15
    assert player_repo.get_balance(OTHER_TARGET_ID, TEST_GUILD_ID) == 15


def test_levy_fine_rejects_player_without_outstanding_obligations(tax_stack):
    player_repo = tax_stack["player_repo"]
    service = tax_stack["service"]
    _add_player(player_repo, TARGET_ID, 100)

    result = service.levy_fine(
        TARGET_ID,
        TEST_GUILD_ID,
        amount=5,
        actor_id=TAX_MAN_ID,
        now=1_000,
        cooldown_seconds=100,
    )

    assert result["status"] == "no_outstanding_obligations"
    assert player_repo.get_balance(TARGET_ID, TEST_GUILD_ID) == 100


def test_prediction_market_exposure_includes_cost_basis_ev_and_liability(tax_stack):
    player_repo = tax_stack["player_repo"]
    prediction_repo = tax_stack["prediction_repo"]
    service = tax_stack["service"]
    _add_player(player_repo, TARGET_ID, 250)
    _add_player(player_repo, OTHER_TARGET_ID, 300)
    prediction_id = prediction_repo.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID,
        creator_id=TAX_MAN_ID,
        question="Will the prediction audit show the real exposure?",
        initial_fair=60,
        initial_levels=[
            ("yes_ask", 61, 10),
            ("yes_bid", 59, 12),
        ],
    )

    with tax_stack["tax_repo"].connection() as conn:
        conn.execute(
            """
            INSERT INTO prediction_positions (
                prediction_id, discord_id, yes_contracts, yes_cost_basis_total,
                no_contracts, no_cost_basis_total
            )
            VALUES (?, ?, 10, 50, 0, 0)
            """,
            (prediction_id, TARGET_ID),
        )
        conn.execute(
            """
            INSERT INTO prediction_positions (
                prediction_id, discord_id, yes_contracts, yes_cost_basis_total,
                no_contracts, no_cost_basis_total
            )
            VALUES (?, ?, 0, 0, 5, 25)
            """,
            (prediction_id, OTHER_TARGET_ID),
        )

    guild_snapshot = service.get_guild_snapshot(TEST_GUILD_ID)
    pred_summary = guild_snapshot["prediction_exposure"]["summary"]
    pred_market = guild_snapshot["prediction_exposure"]["markets"][0]

    assert pred_summary["open_markets"] == 1
    assert pred_summary["cost_basis"] == 75
    assert pred_summary["expected_payout"] == 80
    assert pred_summary["ev_to_holders"] == 5
    assert pred_summary["yes_liability"] == 100
    assert pred_summary["no_liability"] == 50
    assert pred_summary["worst_case_payout"] == 100
    assert pred_summary["book_contracts"] == 22
    assert pred_market["top_yes_ask"] == 61
    assert pred_market["top_yes_bid"] == 59

    player_snapshot = service.get_player_snapshot(TARGET_ID, TEST_GUILD_ID)
    position_summary = player_snapshot["prediction_exposure"]["summary"]
    position = player_snapshot["prediction_exposure"]["positions"][0]

    assert player_snapshot["prediction_cost_basis"] == 50
    assert player_snapshot["effective_obligations"] == 50
    assert position_summary["cost_basis"] == 50
    assert position_summary["expected_payout"] == 60
    assert position_summary["ev"] == 10
    assert position_summary["max_payout"] == 100
    assert position["yes_contracts"] == 10
    assert position["yes_cost_basis"] == 50


def test_levy_fine_can_use_prediction_cost_basis_as_obligation(tax_stack):
    player_repo = tax_stack["player_repo"]
    prediction_repo = tax_stack["prediction_repo"]
    service = tax_stack["service"]
    _add_player(player_repo, TARGET_ID, 3)
    prediction_id = prediction_repo.create_orderbook_prediction(
        guild_id=TEST_GUILD_ID,
        creator_id=TAX_MAN_ID,
        question="Will market exposure be taxable?",
        initial_fair=60,
        initial_levels=[],
    )
    with tax_stack["tax_repo"].connection() as conn:
        conn.execute(
            """
            INSERT INTO prediction_positions (
                prediction_id, discord_id, yes_contracts, yes_cost_basis_total,
                no_contracts, no_cost_basis_total
            )
            VALUES (?, ?, 10, 50, 0, 0)
            """,
            (prediction_id, TARGET_ID),
        )

    result = service.levy_fine(
        TARGET_ID,
        TEST_GUILD_ID,
        amount=20,
        actor_id=TAX_MAN_ID,
        now=1_000,
        cooldown_seconds=100,
    )

    assert result["status"] == "ok"
    assert result["applied_amount"] == 20
    assert result["outstanding_obligations"] == 50
    assert player_repo.get_balance(TARGET_ID, TEST_GUILD_ID) == -17


def test_tax_man_can_add_and_remove_bankruptcy_modifier(tax_stack):
    player_repo = tax_stack["player_repo"]
    bankruptcy_repo = tax_stack["bankruptcy_repo"]
    service = tax_stack["service"]
    _add_player(player_repo, TARGET_ID, 100)

    added = service.add_bankruptcy_modifier(
        TARGET_ID,
        TEST_GUILD_ID,
        games=3,
        actor_id=TAX_MAN_ID,
        reason="court order",
    )
    added_again = service.add_bankruptcy_modifier(
        TARGET_ID,
        TEST_GUILD_ID,
        games=2,
        actor_id=TAX_MAN_ID,
    )
    removed = service.remove_bankruptcy_modifier(
        TARGET_ID,
        TEST_GUILD_ID,
        actor_id=TAX_MAN_ID,
        reason="appeal granted",
    )

    assert added["status"] == "ok"
    assert added["previous_games"] == 0
    assert added["penalty_games_remaining"] == 3
    assert added["reason"] == "court order"
    assert added_again["previous_games"] == 3
    assert added_again["penalty_games_remaining"] == 5
    assert removed["previous_games"] == 5
    assert removed["penalty_games_remaining"] == 0
    state = bankruptcy_repo.get_state(TARGET_ID, TEST_GUILD_ID)
    assert state is not None
    assert state["bankruptcy_count"] == 0
    assert state["last_bankruptcy_at"] is None
    assert state["penalty_games_remaining"] == 0


def test_tax_man_bankruptcy_modifier_rejects_invalid_or_unknown_target(tax_stack):
    service = tax_stack["service"]

    invalid_games = service.add_bankruptcy_modifier(
        TARGET_ID,
        TEST_GUILD_ID,
        games=0,
        actor_id=TAX_MAN_ID,
    )
    missing_target = service.remove_bankruptcy_modifier(
        TARGET_ID,
        TEST_GUILD_ID,
        actor_id=TAX_MAN_ID,
    )

    assert invalid_games["status"] == "invalid_games"
    assert missing_target["status"] == "target_not_registered"
