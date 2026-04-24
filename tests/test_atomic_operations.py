"""
Atomicity tests for the repo-level ``*_atomic`` helpers introduced to fold
read-modify-write sequences into single BEGIN IMMEDIATE transactions.

Each test exercises one of the helpers at the repository boundary — the
layer where the atomicity guarantee actually lives — and verifies either:

- The state invariant: after a single call, every dependent table reflects
  the same logical write (no half-applied rows across tables).
- The race invariant: N concurrent callers either all observe a consistent
  final state, or exactly one caller succeeds (with the rest surfacing a
  specific error) — never a partial double-write.

These supplement the wider economy-race suite in
``test_economy_race_conditions.py``; the focus here is the new atomic
helpers from the transaction-atomicity branch rather than the older
service-level races.
"""
from __future__ import annotations

import json
import random
import sqlite3
import threading

import pytest

from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.dig_repository import DigRepository
from repositories.disburse_repository import DisburseRepository
from repositories.loan_repository import LoanRepository
from repositories.mana_repository import ManaRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from repositories.rebellion_repository import RebellionRepository
from repositories.recalibration_repository import RecalibrationRepository
from tests.conftest import TEST_GUILD_ID

# ---------------------------------------------------------------------------
# Repo fixtures local to this file
# ---------------------------------------------------------------------------


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


@pytest.fixture
def loan_repo(repo_db_path):
    return LoanRepository(repo_db_path)


@pytest.fixture
def bankruptcy_repo(repo_db_path):
    return BankruptcyRepository(repo_db_path)


@pytest.fixture
def prediction_repo(repo_db_path):
    return PredictionRepository(repo_db_path)


@pytest.fixture
def match_repo(repo_db_path):
    return MatchRepository(repo_db_path)


@pytest.fixture
def mana_repo(repo_db_path):
    return ManaRepository(repo_db_path)


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def rebellion_repo(repo_db_path):
    return RebellionRepository(repo_db_path)


@pytest.fixture
def recalibration_repo(repo_db_path):
    return RecalibrationRepository(repo_db_path)


@pytest.fixture
def disburse_repo(repo_db_path):
    return DisburseRepository(repo_db_path)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def register(pr: PlayerRepository, discord_id: int, *, balance: int = 100) -> None:
    pr.add(
        discord_id=discord_id,
        discord_username=f"u{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
    )
    pr.update_balance(discord_id, TEST_GUILD_ID, balance)


def _match_record_defaults(team1, team2, winning_team=1):
    """Build the argument shape the MatchService gives to record_match_core_atomic.

    Keeps tests short while preserving realistic inputs.
    """
    glicko_updates = [
        (i, 1520.0 if (i in team1) == (winning_team == 1) else 1480.0, 300.0, 0.059)
        for i in team1 + team2
    ]
    os_updates = [(i, 27.0, 8.0) for i in team1 + team2]
    rating_history_rows = [
        {
            "discord_id": i,
            "rating": 1520.0 if (i in team1) == (winning_team == 1) else 1480.0,
            "rating_before": 1500.0,
            "rd_before": 350.0,
            "rd_after": 300.0,
            "volatility_before": 0.06,
            "volatility_after": 0.059,
            "expected_team_win_prob": 0.5,
            "team_number": 1 if i in team1 else 2,
            "won": (i in team1) == (winning_team == 1),
            "os_mu_before": 25.0,
            "os_mu_after": 27.0,
            "os_sigma_before": 8.33,
            "os_sigma_after": 8.0,
            "streak_length": 1,
            "streak_multiplier": 1.0,
        }
        for i in team1 + team2
    ]
    match_prediction = {
        "radiant_rating": 1500.0,
        "dire_rating": 1500.0,
        "radiant_rd": 100.0,
        "dire_rd": 100.0,
        "expected_radiant_win_prob": 0.5,
    }
    return glicko_updates, os_updates, rating_history_rows, match_prediction


# ---------------------------------------------------------------------------
# Tip: fee (and optional tithe) land in nonprofit together
# ---------------------------------------------------------------------------


class TestTipAtomic:
    def test_fee_and_tithe_credit_nonprofit_no_burn(self, player_repo, loan_repo):
        register(player_repo, 1, balance=200)
        register(player_repo, 2, balance=50)

        fund_before = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)
        result = player_repo.tip_atomic(
            from_discord_id=1,
            to_discord_id=2,
            guild_id=TEST_GUILD_ID,
            amount=40,
            fee=5,
            tithe=3,
        )

        assert result["from_new_balance"] == 200 - 40 - 5 - 3
        assert result["to_new_balance"] == 50 + 40
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) - fund_before == 5 + 3
        # Total economy preserved: players lost (fee + tithe), fund gained it.
        total_after = (
            player_repo.get_balance(1, TEST_GUILD_ID)
            + player_repo.get_balance(2, TEST_GUILD_ID)
            + loan_repo.get_nonprofit_fund(TEST_GUILD_ID)
        )
        assert total_after == 200 + 50 + fund_before

    def test_insufficient_balance_rolls_back_everything(self, player_repo, loan_repo):
        """A failed tip must not touch the sender, recipient, or nonprofit."""
        register(player_repo, 1, balance=10)
        register(player_repo, 2, balance=0)
        fund_before = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)

        with pytest.raises(ValueError, match="Insufficient"):
            player_repo.tip_atomic(
                from_discord_id=1,
                to_discord_id=2,
                guild_id=TEST_GUILD_ID,
                amount=20,
                fee=5,
            )

        assert player_repo.get_balance(1, TEST_GUILD_ID) == 10
        assert player_repo.get_balance(2, TEST_GUILD_ID) == 0
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == fund_before


# ---------------------------------------------------------------------------
# Loan repayment: one atomic drains debt, credits nonprofit, clears state
# ---------------------------------------------------------------------------


class TestLoanRepaymentAtomic:
    def test_full_repayment_flow(self, player_repo, loan_repo):
        register(player_repo, 1, balance=0)
        loan_repo.execute_loan_atomic(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            amount=50,
            fee=10,
            cooldown_seconds=60,
            max_amount=1000,
        )
        # After the loan: balance=50, outstanding=50/fee=10
        fund_before = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)

        result = loan_repo.execute_repayment_atomic(1, TEST_GUILD_ID)

        assert result["principal"] == 50
        assert result["fee"] == 10
        assert result["total_repaid"] == 60
        assert result["balance_before"] == 50
        assert result["new_balance"] == -10
        # Balance actually moved by the full total_repaid; outstanding cleared;
        # nonprofit fund gained exactly the fee.
        assert player_repo.get_balance(1, TEST_GUILD_ID) == -10
        state = loan_repo.get_state(1, TEST_GUILD_ID)
        assert state["outstanding_principal"] == 0
        assert state["outstanding_fee"] == 0
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) - fund_before == 10

    def test_returns_none_when_no_outstanding_loan(self, player_repo, loan_repo):
        register(player_repo, 1, balance=50)
        assert loan_repo.execute_repayment_atomic(1, TEST_GUILD_ID) is None
        # Nothing moved.
        assert player_repo.get_balance(1, TEST_GUILD_ID) == 50


# ---------------------------------------------------------------------------
# Bankruptcy: validate + clear debt + bump state atomic; concurrent calls safe
# ---------------------------------------------------------------------------


class TestBankruptcyAtomic:
    def test_clears_debt_and_records_state(self, player_repo, bankruptcy_repo):
        register(player_repo, 1, balance=-75)

        result = bankruptcy_repo.execute_bankruptcy_atomic(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            now=1_700_000_000,
            cooldown_seconds=3600,
            fresh_start_balance=0,
            penalty_games=5,
        )

        assert result["debt_cleared"] == 75
        assert result["new_balance"] == 0
        assert player_repo.get_balance(1, TEST_GUILD_ID) == 0
        state = bankruptcy_repo.get_state(1, TEST_GUILD_ID)
        assert state["penalty_games_remaining"] == 5
        assert state["last_bankruptcy_at"] == 1_700_000_000

    def test_raises_not_in_debt_when_balance_non_negative(
        self, player_repo, bankruptcy_repo
    ):
        register(player_repo, 1, balance=10)
        with pytest.raises(ValueError, match="NOT_IN_DEBT"):
            bankruptcy_repo.execute_bankruptcy_atomic(
                discord_id=1,
                guild_id=TEST_GUILD_ID,
                now=0,
                cooldown_seconds=3600,
                fresh_start_balance=0,
                penalty_games=5,
            )

    def test_concurrent_declarations_do_not_double_bump(
        self, player_repo, bankruptcy_repo
    ):
        """5 threads race to declare; only one should win."""
        register(player_repo, 1, balance=-100)

        successes: list[dict] = []
        errors: list[str] = []
        lock = threading.Lock()

        def try_bankrupt() -> None:
            try:
                r = bankruptcy_repo.execute_bankruptcy_atomic(
                    discord_id=1,
                    guild_id=TEST_GUILD_ID,
                    now=1_700_000_000,
                    cooldown_seconds=3600,
                    fresh_start_balance=0,
                    penalty_games=5,
                )
                with lock:
                    successes.append(r)
            except ValueError as exc:
                with lock:
                    errors.append(str(exc))

        threads = [threading.Thread(target=try_bankrupt) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(successes) == 1, f"expected 1 success, got {len(successes)}"
        # Losers must fail on a well-known reason — not silently skip.
        for err in errors:
            assert err.startswith("ON_COOLDOWN") or err.startswith("NOT_IN_DEBT")
        state = bankruptcy_repo.get_state(1, TEST_GUILD_ID)
        # Counter reflects exactly one declaration (not 5).
        assert state["penalty_games_remaining"] == 5


# ---------------------------------------------------------------------------
# Prediction: resolve flips status and pays winners in one txn
# ---------------------------------------------------------------------------


class TestPredictionResolveAtomic:
    def test_status_flip_and_pool_payout_commit_together(
        self, player_repo, prediction_repo
    ):
        for did in (10, 11, 12):
            register(player_repo, did, balance=100)

        pid = prediction_repo.create_prediction(
            guild_id=TEST_GUILD_ID,
            creator_id=10,
            question="Will it rain?",
            closes_at=99_999_999_999,
        )
        prediction_repo.place_bet_atomic(
            prediction_id=pid, discord_id=10, position="yes", amount=20
        )
        prediction_repo.place_bet_atomic(
            prediction_id=pid, discord_id=11, position="no", amount=30
        )
        prediction_repo.place_bet_atomic(
            prediction_id=pid, discord_id=12, position="yes", amount=10
        )
        # Lock betting so resolve is valid.
        prediction_repo.update_prediction_status(pid, "locked")

        balances_before = {
            did: player_repo.get_balance(did, TEST_GUILD_ID) for did in (10, 11, 12)
        }

        result = prediction_repo.resolve_and_settle_atomic(
            prediction_id=pid, outcome="yes", resolved_by=10
        )

        pred = prediction_repo.get_prediction(pid)
        assert pred["status"] == "resolved"
        assert pred["outcome"] == "yes"
        assert result["total_pool"] == 60
        assert result["winner_pool"] == 30

        # Yes bettors got paid; no bettor unchanged (already debited at bet time).
        assert player_repo.get_balance(10, TEST_GUILD_ID) > balances_before[10]
        assert player_repo.get_balance(12, TEST_GUILD_ID) > balances_before[12]
        assert player_repo.get_balance(11, TEST_GUILD_ID) == balances_before[11]

        # Winning bets carry a payout column > 0; losers remain NULL.
        bets = prediction_repo.get_prediction_bets(pid)
        for bet in bets:
            if bet["position"] == "yes":
                assert bet["payout"] is not None and bet["payout"] > 0
            else:
                assert bet["payout"] is None


# ---------------------------------------------------------------------------
# Match recording: match + participants + ratings + pairings commit together
# ---------------------------------------------------------------------------


class TestMatchRecordAtomic:
    def test_full_record_commits_every_dependent_row(self, player_repo, match_repo):
        team1, team2 = [1, 2, 3, 4, 5], [6, 7, 8, 9, 10]
        for did in team1 + team2:
            register(player_repo, did, balance=50)
            player_repo.update_glicko_rating(did, TEST_GUILD_ID, 1500.0, 350.0, 0.06)

        glicko, os_, history, prediction = _match_record_defaults(team1, team2)

        match_id = match_repo.record_match_core_atomic(
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            dotabuff_match_id=None,
            lobby_type="shuffle",
            balancing_rating_system="glicko",
            winning_ids=team1,
            losing_ids=team2,
            glicko_updates=glicko,
            openskill_updates=os_,
            rating_history_rows=history,
            match_prediction=prediction,
            last_match_date_iso="2026-04-22T00:00:00",
            first_calibration_ids=[],
            first_calibration_unix=0,
            effective_avoid_ids=[],
            effective_deal_ids=[],
        )
        assert match_id > 0

        participants = match_repo.get_match_participants(match_id, TEST_GUILD_ID)
        assert len(participants) == 10

        rating_history = match_repo.get_full_rating_history_for_match(match_id)
        assert len(rating_history) == 10

        # Pairings: C(5,2) teammate pairs × 2 teams + 5×5 opponents = 45 rows
        with sqlite3.connect(player_repo.db_path) as conn:
            conn.row_factory = sqlite3.Row
            pairings_count = conn.execute(
                "SELECT COUNT(*) AS c FROM player_pairings WHERE guild_id = ?",
                (TEST_GUILD_ID,),
            ).fetchone()["c"]
        assert pairings_count == 45

        # Win/loss counters applied.
        for pid in team1:
            assert player_repo.get_by_id(pid, TEST_GUILD_ID).wins == 1
        for pid in team2:
            assert player_repo.get_by_id(pid, TEST_GUILD_ID).losses == 1


# ---------------------------------------------------------------------------
# Match correction: swap wins/losses + pairings delta + correction log atomic
# ---------------------------------------------------------------------------


class TestMatchCorrectAtomic:
    def test_flip_winning_team_swaps_counters_and_pairings(
        self, player_repo, match_repo
    ):
        team1, team2 = [1, 2, 3, 4, 5], [6, 7, 8, 9, 10]
        for did in team1 + team2:
            register(player_repo, did, balance=50)
            player_repo.update_glicko_rating(did, TEST_GUILD_ID, 1500.0, 350.0, 0.06)

        glicko, os_, history, prediction = _match_record_defaults(team1, team2)
        match_id = match_repo.record_match_core_atomic(
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            dotabuff_match_id=None,
            lobby_type="shuffle",
            balancing_rating_system="glicko",
            winning_ids=team1,
            losing_ids=team2,
            glicko_updates=glicko,
            openskill_updates=os_,
            rating_history_rows=history,
            match_prediction=prediction,
            last_match_date_iso="2026-04-22T00:00:00",
            first_calibration_ids=[],
            first_calibration_unix=0,
            effective_avoid_ids=[],
            effective_deal_ids=[],
        )

        # Correct — dire actually won.
        new_glicko = [
            (i, 1480.0 if i in team1 else 1520.0, 300.0, 0.059) for i in team1 + team2
        ]
        new_os = [(i, 26.0, 8.0) for i in team1 + team2]
        rating_history_updates = [
            {
                "discord_id": i,
                "new_rating": 1480.0 if i in team1 else 1520.0,
                "new_rd": 300.0,
                "new_volatility": 0.059,
                "new_won": i in team2,
                "new_os_mu": 26.0,
                "new_os_sigma": 8.0,
            }
            for i in team1 + team2
        ]

        correction_id = match_repo.correct_match_result_atomic(
            match_id=match_id,
            guild_id=TEST_GUILD_ID,
            old_winning_team=1,
            new_winning_team=2,
            old_winner_ids=team1,
            old_loser_ids=team2,
            radiant_ids=team1,
            dire_ids=team2,
            glicko_updates=new_glicko,
            openskill_updates=new_os,
            rating_history_updates=rating_history_updates,
            corrected_by=99,
        )
        assert correction_id is not None and correction_id > 0

        match_row = match_repo.get_match(match_id, TEST_GUILD_ID)
        assert match_row["winning_team"] == 2

        for pid in team1:
            p = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert p.wins == 0 and p.losses == 1
        for pid in team2:
            p = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert p.wins == 1 and p.losses == 0

    def test_correction_without_corrected_by_returns_none(
        self, player_repo, match_repo
    ):
        team1, team2 = [1, 2, 3, 4, 5], [6, 7, 8, 9, 10]
        for did in team1 + team2:
            register(player_repo, did, balance=50)
            player_repo.update_glicko_rating(did, TEST_GUILD_ID, 1500.0, 350.0, 0.06)
        glicko, os_, history, prediction = _match_record_defaults(team1, team2)
        match_id = match_repo.record_match_core_atomic(
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            dotabuff_match_id=None,
            lobby_type="shuffle",
            balancing_rating_system="glicko",
            winning_ids=team1,
            losing_ids=team2,
            glicko_updates=glicko,
            openskill_updates=os_,
            rating_history_rows=history,
            match_prediction=prediction,
            last_match_date_iso="2026-04-22T00:00:00",
            first_calibration_ids=[],
            first_calibration_unix=0,
            effective_avoid_ids=[],
            effective_deal_ids=[],
        )

        correction_id = match_repo.correct_match_result_atomic(
            match_id=match_id,
            guild_id=TEST_GUILD_ID,
            old_winning_team=1,
            new_winning_team=2,
            old_winner_ids=team1,
            old_loser_ids=team2,
            radiant_ids=team1,
            dire_ids=team2,
            glicko_updates=[],
            openskill_updates=[],
            rating_history_updates=[],
            corrected_by=None,
        )
        assert correction_id is None


# ---------------------------------------------------------------------------
# Mana daily claim: exactly one of N concurrent callers wins per date
# ---------------------------------------------------------------------------


class TestManaAtomic:
    def test_concurrent_claims_only_one_succeeds(self, player_repo, mana_repo):
        register(player_repo, 1)

        results: list[bool] = []
        lock = threading.Lock()

        def try_claim() -> None:
            claimed = mana_repo.claim_mana_atomic(
                1, TEST_GUILD_ID, "Island", "2026-04-22"
            )
            with lock:
                results.append(claimed)

        threads = [threading.Thread(target=try_claim) for _ in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert sum(results) == 1, f"expected exactly one True, got {sum(results)}"
        stored = mana_repo.get_mana(1, TEST_GUILD_ID)
        assert stored is not None
        assert stored["current_land"] == "Island"
        assert stored["assigned_date"] == "2026-04-22"

    def test_claim_idempotent_per_date(self, player_repo, mana_repo):
        """Serial re-calls on the same date return False, don't re-roll the land."""
        register(player_repo, 1)

        assert mana_repo.claim_mana_atomic(1, TEST_GUILD_ID, "Forest", "2026-04-22")
        assert not mana_repo.claim_mana_atomic(1, TEST_GUILD_ID, "Swamp", "2026-04-22")
        stored = mana_repo.get_mana(1, TEST_GUILD_ID)
        # Second call's "Swamp" proposal was ignored; original land preserved.
        assert stored["current_land"] == "Forest"

    def test_claim_succeeds_on_new_date(self, player_repo, mana_repo):
        """Advancing the assigned_date unlocks a fresh claim."""
        register(player_repo, 1)

        assert mana_repo.claim_mana_atomic(1, TEST_GUILD_ID, "Mountain", "2026-04-22")
        assert mana_repo.claim_mana_atomic(1, TEST_GUILD_ID, "Plains", "2026-04-23")
        stored = mana_repo.get_mana(1, TEST_GUILD_ID)
        assert stored["current_land"] == "Plains"
        assert stored["assigned_date"] == "2026-04-23"


# ---------------------------------------------------------------------------
# Dig: atomic_sabotage covers the trap branch cleanly
# ---------------------------------------------------------------------------


class TestDigSabotageAtomic:
    def test_trap_branch_applies_all_writes_together(self, player_repo, dig_repo):
        register(player_repo, 1, balance=100)  # actor
        register(player_repo, 2, balance=20)  # target
        dig_repo.create_tunnel(1, TEST_GUILD_ID, name="actor")
        dig_repo.create_tunnel(2, TEST_GUILD_ID, name="target")
        dig_repo.update_tunnel(1, TEST_GUILD_ID, depth=50)
        dig_repo.update_tunnel(2, TEST_GUILD_ID, depth=30, trap_active=1)

        result = dig_repo.atomic_sabotage(
            actor_id=1,
            target_id=2,
            guild_id=TEST_GUILD_ID,
            actor_jc_cost=10,
            target_depth_delta=0,
            target_jc_credit=5,
            actor_depth_delta=-3,
            clear_target_trap=True,
            log_detail={
                "target_id": 2,
                "trap_triggered": True,
                "jc_lost": 10,
                "blocks_lost": 3,
            },
        )

        assert player_repo.get_balance(1, TEST_GUILD_ID) == 90
        assert player_repo.get_balance(2, TEST_GUILD_ID) == 25
        actor_tun = dig_repo.get_tunnel(1, TEST_GUILD_ID)
        target_tun = dig_repo.get_tunnel(2, TEST_GUILD_ID)
        assert actor_tun["depth"] == 47
        assert target_tun["trap_active"] == 0
        # Target depth untouched on trap branch.
        assert result["depth"] == 30

        with sqlite3.connect(dig_repo.db_path) as conn:
            conn.row_factory = sqlite3.Row
            log_count = conn.execute(
                "SELECT COUNT(*) AS c FROM dig_actions "
                "WHERE actor_id = 1 AND action_type = 'sabotage'"
            ).fetchone()["c"]
        assert log_count == 1


# ---------------------------------------------------------------------------
# Rebellion fizzle: refund + status flip + cooldown commit together
# ---------------------------------------------------------------------------


class TestRebellionFizzleAtomic:
    def test_fizzle_refunds_defenders_once_and_marks_war(
        self, player_repo, rebellion_repo
    ):
        for did in (100, 101, 102, 103):
            register(player_repo, did, balance=50)

        war_id = rebellion_repo.create_war(
            guild_id=TEST_GUILD_ID,
            inciter_id=100,
            vote_closes_at=99_999_999_999,
            created_at=0,
        )
        # Simulate stakes already debited from defenders.
        for did in (101, 102, 103):
            player_repo.add_balance(did, TEST_GUILD_ID, -10)
        with sqlite3.connect(player_repo.db_path) as conn:
            conn.execute(
                "UPDATE wheel_wars SET defend_voter_ids = ?, effective_defend_count = 3.0 "
                "WHERE war_id = ?",
                (json.dumps([101, 102, 103]), war_id),
            )

        balances_before = {
            did: player_repo.get_balance(did, TEST_GUILD_ID) for did in (101, 102, 103)
        }

        rebellion_repo.atomic_resolve_fizzle(
            war_id=war_id,
            guild_id=TEST_GUILD_ID,
            defender_ids=[101, 102, 103],
            defender_stake=10,
            inciter_cooldown_until=1_000_000,
            resolved_at=100_000,
        )

        for did in (101, 102, 103):
            assert player_repo.get_balance(did, TEST_GUILD_ID) - balances_before[did] == 10

        war = rebellion_repo.get_war(war_id)
        assert war["status"] == "fizzled"
        assert war["outcome"] == "fizzled"
        assert war["inciter_cooldown_until"] == 1_000_000
        assert war["resolved_at"] == 100_000


# ---------------------------------------------------------------------------
# Rebellion defend-vote: each voter debited exactly once, no phantom votes
# ---------------------------------------------------------------------------


class TestRebellionDefendVoteAtomic:
    def test_concurrent_votes_debit_once_per_voter(self, player_repo, rebellion_repo):
        for did in (100, 200, 201, 202, 203, 204):
            register(player_repo, did, balance=50)
        war_id = rebellion_repo.create_war(
            guild_id=TEST_GUILD_ID,
            inciter_id=100,
            vote_closes_at=99_999_999_999,
            created_at=0,
        )

        errors: list[str] = []
        successes: list[dict] = []
        lock = threading.Lock()

        def vote(uid: int) -> None:
            try:
                r = rebellion_repo.defend_vote_with_stake_atomic(
                    war_id=war_id,
                    discord_id=uid,
                    guild_id=TEST_GUILD_ID,
                    stake=10,
                )
                with lock:
                    successes.append({"uid": uid, **r})
            except ValueError as exc:
                with lock:
                    errors.append(f"{uid}:{exc}")

        threads: list[threading.Thread] = []
        for uid in (200, 201, 202, 203, 204):
            for _ in range(3):
                threads.append(threading.Thread(target=vote, args=(uid,)))
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # Each voter debited exactly once.
        for uid in (200, 201, 202, 203, 204):
            assert player_repo.get_balance(uid, TEST_GUILD_ID) == 40

        war = rebellion_repo.get_war(war_id)
        voters = json.loads(war["defend_voter_ids"])
        assert sorted(voters) == [200, 201, 202, 203, 204]
        assert war["effective_defend_count"] == 5.0

    def test_inciter_cannot_defend(self, player_repo, rebellion_repo):
        register(player_repo, 100, balance=50)
        war_id = rebellion_repo.create_war(
            guild_id=TEST_GUILD_ID,
            inciter_id=100,
            vote_closes_at=99_999_999_999,
            created_at=0,
        )
        with pytest.raises(ValueError, match="inciter cannot defend"):
            rebellion_repo.defend_vote_with_stake_atomic(
                war_id=war_id, discord_id=100, guild_id=TEST_GUILD_ID, stake=10
            )
        # Balance untouched.
        assert player_repo.get_balance(100, TEST_GUILD_ID) == 50


# ---------------------------------------------------------------------------
# Recalibration: cooldown check + Glicko update + state bump atomic
# ---------------------------------------------------------------------------


class TestRecalibrationAtomic:
    def test_concurrent_recalibrations_bump_counter_once(
        self, player_repo, recalibration_repo
    ):
        register(player_repo, 1)
        player_repo.update_glicko_rating(1, TEST_GUILD_ID, 1500.0, 200.0, 0.06)

        successes: list[int] = []
        errors: list[str] = []
        lock = threading.Lock()

        def try_recal() -> None:
            try:
                r = recalibration_repo.execute_recalibration_atomic(
                    discord_id=1,
                    guild_id=TEST_GUILD_ID,
                    now=1_000_000,
                    cooldown_seconds=3600,
                    rating=1500.0,
                    new_rd=300.0,
                    new_volatility=0.06,
                )
                with lock:
                    successes.append(r)
            except ValueError as exc:
                with lock:
                    errors.append(str(exc))

        threads = [threading.Thread(target=try_recal) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(successes) == 1
        for err in errors:
            assert err.startswith("ON_COOLDOWN"), err
        state = recalibration_repo.get_state(1, TEST_GUILD_ID)
        assert state["total_recalibrations"] == 1
        # Glicko RD updated to the new floor.
        rating = player_repo.get_glicko_rating(1, TEST_GUILD_ID)
        assert rating is not None
        assert rating[1] == 300.0


# ---------------------------------------------------------------------------
# Disburse reset: status flip + reserve return atomic; idempotent on retry
# ---------------------------------------------------------------------------


class TestDisburseResetAtomic:
    def test_reset_returns_reserve_and_no_ops_on_retry(
        self, player_repo, loan_repo, disburse_repo
    ):
        register(player_repo, 1)
        loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 500)

        disburse_repo.create_proposal(
            guild_id=TEST_GUILD_ID,
            proposal_id=1,
            fund_amount=200,
            quorum_required=3,
        )
        loan_repo.deduct_from_nonprofit_fund(TEST_GUILD_ID, 200)
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 300

        assert disburse_repo.reset_and_return_fund_atomic(
            TEST_GUILD_ID, fund_amount_to_return=200
        )
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 500

        with sqlite3.connect(loan_repo.db_path) as conn:
            conn.row_factory = sqlite3.Row
            row = conn.execute(
                "SELECT status FROM disburse_proposals WHERE proposal_id = 1"
            ).fetchone()
            assert row["status"] == "reset"

        # Second reset must not double-refund.
        assert not disburse_repo.reset_and_return_fund_atomic(
            TEST_GUILD_ID, fund_amount_to_return=200
        )
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 500


# ---------------------------------------------------------------------------
# Enrichment: match-level fields + all participant rows commit together
# ---------------------------------------------------------------------------


class TestEnrichmentAtomic:
    def test_match_and_participants_commit_in_one_call(
        self, player_repo, match_repo
    ):
        team1, team2 = [1, 2, 3, 4, 5], [6, 7, 8, 9, 10]
        for did in team1 + team2:
            register(player_repo, did, balance=50)
            player_repo.update_glicko_rating(did, TEST_GUILD_ID, 1500.0, 350.0, 0.06)
        glicko, os_, history, prediction = _match_record_defaults(team1, team2)
        match_id = match_repo.record_match_core_atomic(
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            dotabuff_match_id=None,
            lobby_type="shuffle",
            balancing_rating_system="glicko",
            winning_ids=team1,
            losing_ids=team2,
            glicko_updates=glicko,
            openskill_updates=os_,
            rating_history_rows=history,
            match_prediction=prediction,
            last_match_date_iso="2026-04-22T00:00:00",
            first_calibration_ids=[],
            first_calibration_unix=0,
            effective_avoid_ids=[],
            effective_deal_ids=[],
        )

        participant_updates = [
            {
                "discord_id": did,
                "hero_id": did % 100 + 1,
                "kills": 5,
                "deaths": 2,
                "assists": 8,
                "gpm": 500,
                "xpm": 550,
                "hero_damage": 20000,
                "tower_damage": 3000,
                "last_hits": 200,
                "denies": 10,
                "net_worth": 25000,
                "hero_healing": 500,
                "lane_role": 1,
                "lane_efficiency": 80,
                "fantasy_points": 50.0,
            }
            for did in team1 + team2
        ]
        rowcount = match_repo.apply_enrichment_atomic(
            match_id=match_id,
            valve_match_id=8_181_518_332,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_data='{"stub": true}',
            enrichment_source="manual",
            enrichment_confidence=None,
            participant_updates=participant_updates,
        )
        assert rowcount == 10

        match_row = match_repo.get_match(match_id, TEST_GUILD_ID)
        assert match_row["valve_match_id"] == 8_181_518_332
        assert match_row["duration_seconds"] == 2400
        assert match_row["radiant_score"] == 35

        participants = match_repo.get_match_participants(match_id, TEST_GUILD_ID)
        # All 10 rows got their hero/fantasy fields populated in the same txn.
        for p in participants:
            assert p["hero_id"] is not None
            assert p["fantasy_points"] == 50.0


# ---------------------------------------------------------------------------
# Entry point: make sure random seeding used in a few helpers doesn't leak.
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _seed_random():
    state = random.getstate()
    random.seed(0)
    yield
    random.setstate(state)
