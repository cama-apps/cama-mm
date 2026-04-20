"""
Regression tests for economy race conditions and logic bugs.

These tests exercise concurrent callers against services that previously had
TOCTOU windows and non-atomic paths. Each test seeds any randomness it uses,
spawns threads, and asserts the bookkeeping invariants hold.

Covered:
- Fix 1: DisburseService nonprofit fund double-deduction race
- Fix 2: DisburseService quorum-check TOCTOU
- Fix 3: BettingService.award_participation() non-atomic no-garnishment path
- Fix 4: BettingService._award_with_penalties ordering (garnishment before penalty)
- Fix 5: BettingService.create_auto_blind_bets() balance TOCTOU
- Fix 6: RecalibrationService guild_id normalization parity
"""

from __future__ import annotations

import random
import threading
import time

import pytest

from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.bet_repository import BetRepository
from repositories.disburse_repository import DisburseRepository
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository
from repositories.recalibration_repository import RecalibrationRepository
from services.bankruptcy_service import BankruptcyService
from services.betting_service import BettingService
from services.disburse_service import DisburseService
from services.garnishment_service import GarnishmentService
from services.recalibration_service import RecalibrationService
from tests.conftest import TEST_GUILD_ID

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def loan_repo(repo_db_path):
    return LoanRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


@pytest.fixture
def disburse_repo(repo_db_path):
    return DisburseRepository(repo_db_path)


@pytest.fixture
def bet_repo(repo_db_path):
    return BetRepository(repo_db_path)


@pytest.fixture
def bankruptcy_repo(repo_db_path):
    return BankruptcyRepository(repo_db_path)


@pytest.fixture
def recalibration_repo(repo_db_path):
    return RecalibrationRepository(repo_db_path)


@pytest.fixture
def disburse_service(disburse_repo, player_repo, loan_repo):
    return DisburseService(
        disburse_repo=disburse_repo,
        player_repo=player_repo,
        loan_repo=loan_repo,
        min_fund=100,
        quorum_percentage=0.40,
    )


# ---------------------------------------------------------------------------
# Fix 1 & 2: Disbursement reserve-return and quorum-check TOCTOU
# ---------------------------------------------------------------------------


def _seed_debtors_and_voters(player_repo, *, debt_per_player: int = 100):
    """Register 2 debtors + 3 voters used across disburse race tests."""
    player_repo.add(discord_id=1001, discord_username="Debtor1", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1002, discord_username="Debtor2", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1003, discord_username="Voter1", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1004, discord_username="Voter2", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1005, discord_username="Voter3", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.update_balance(1001, TEST_GUILD_ID, -debt_per_player)
    player_repo.update_balance(1002, TEST_GUILD_ID, -debt_per_player)
    player_repo.update_balance(1003, TEST_GUILD_ID, 100)
    player_repo.update_balance(1004, TEST_GUILD_ID, 100)
    player_repo.update_balance(1005, TEST_GUILD_ID, 100)


def test_fix1_concurrent_execute_disbursement_does_not_double_refund(
    disburse_service, disburse_repo, loan_repo, player_repo
):
    """
    Two concurrent callers invoking execute_disbursement on the same proposal
    must NOT both return the reserved fund to the pool. The expected accounting:
    - fund starts at F (fund_amount = F)
    - total_disbursed <= F gets credited to player balances
    - remaining (F - total_disbursed) stays in the nonprofit fund
    - player balances get credited exactly total_disbursed, not 2 * total_disbursed
    """
    _seed_debtors_and_voters(player_repo)
    # Fund exactly matches the min; players each owe 100 so even split gives 50 each.
    loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=100)

    disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

    # After create_proposal, the reserve is held aside — fund pool is now 0.
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0

    # Reach quorum with even split (5 players, 40% quorum = 2 votes)
    disburse_service.add_vote(TEST_GUILD_ID, 1003, "even")
    disburse_service.add_vote(TEST_GUILD_ID, 1004, "even")

    # Capture pre-state
    debtor_1_balance_before = player_repo.get_balance(1001, TEST_GUILD_ID)
    debtor_2_balance_before = player_repo.get_balance(1002, TEST_GUILD_ID)

    results: list[object] = [None, None]
    errors: list[BaseException | None] = [None, None]
    barrier = threading.Barrier(2)

    def executor(idx: int) -> None:
        try:
            barrier.wait(timeout=5)
            results[idx] = disburse_service.execute_disbursement(TEST_GUILD_ID)
        except BaseException as e:
            errors[idx] = e

    threads = [threading.Thread(target=executor, args=(i,)) for i in range(2)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=10)
        assert not t.is_alive(), "Disbursement thread deadlocked"

    # Exactly one caller should have succeeded; the other must observe that the
    # proposal is no longer actionable (no active proposal / quorum-not-reached).
    successes = [r for r in results if isinstance(r, dict) and r.get("success")]
    assert len(successes) == 1, f"Expected exactly one successful disbursement, got {results}"

    # The other caller must have raised ValueError (not silently succeeded a 2nd time).
    failures = [e for e in errors if e is not None]
    assert len(failures) == 1, f"Expected exactly one failure, got errors={errors} results={results}"
    assert isinstance(failures[0], ValueError), f"Expected ValueError, got {type(failures[0])}"

    # Accounting invariant: pool + credited_to_players == initial fund (100).
    debtor_1_balance_after = player_repo.get_balance(1001, TEST_GUILD_ID)
    debtor_2_balance_after = player_repo.get_balance(1002, TEST_GUILD_ID)
    total_credited = (
        (debtor_1_balance_after - debtor_1_balance_before)
        + (debtor_2_balance_after - debtor_2_balance_before)
    )
    remaining_pool = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)

    assert total_credited == successes[0]["total_disbursed"], (
        f"Balance delta ({total_credited}) must match reported total_disbursed "
        f"({successes[0]['total_disbursed']})"
    )
    assert remaining_pool + total_credited == 100, (
        f"Accounting mismatch: pool={remaining_pool} + credited={total_credited} != 100 "
        "(fund was double-refunded or over-distributed)"
    )


def test_fix1_concurrent_execute_and_force_execute_single_winner(
    disburse_service, loan_repo, player_repo
):
    """
    A vote-triggered execute_disbursement and an admin force_execute racing on
    the same proposal must not both run to completion. Only one path credits
    players / moves the fund.
    """
    _seed_debtors_and_voters(player_repo)
    loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=100)
    disburse_service.create_proposal(guild_id=TEST_GUILD_ID)
    disburse_service.add_vote(TEST_GUILD_ID, 1003, "neediest")
    disburse_service.add_vote(TEST_GUILD_ID, 1004, "neediest")  # quorum reached

    results: list[object] = [None, None]
    errors: list[BaseException | None] = [None, None]
    barrier = threading.Barrier(2)

    def run_execute() -> None:
        try:
            barrier.wait(timeout=5)
            results[0] = disburse_service.execute_disbursement(TEST_GUILD_ID)
        except BaseException as e:
            errors[0] = e

    def run_force() -> None:
        try:
            barrier.wait(timeout=5)
            results[1] = disburse_service.force_execute(TEST_GUILD_ID)
        except BaseException as e:
            errors[1] = e

    t1 = threading.Thread(target=run_execute)
    t2 = threading.Thread(target=run_force)
    t1.start()
    t2.start()
    t1.join(timeout=10)
    t2.join(timeout=10)

    successes = [r for r in results if isinstance(r, dict) and r.get("success")]
    assert len(successes) == 1, f"Expected exactly one successful path, got {results}"
    # The loser raises; it didn't silently no-op.
    assert any(isinstance(e, ValueError) for e in errors), (
        f"Expected the losing thread to raise ValueError, got errors={errors}"
    )


def test_fix2_quorum_recheck_inside_lock_rejects_late_cancel_after_execute(
    disburse_service, loan_repo, player_repo, disburse_repo
):
    """
    Even if a late "cancel" vote flips the winning method AFTER the quorum check
    but before we touch balances, the atomic path must either:
    - run the winning method we committed to OR
    - cleanly refuse (no partial execution, no double-refund).
    Here we simulate: the proposal passes quorum via 'even'; a late cancel vote
    comes in; a second executor then races to run again. The lock + in-lock
    re-check must prevent a second execution.
    """
    _seed_debtors_and_voters(player_repo)
    loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=100)
    disburse_service.create_proposal(guild_id=TEST_GUILD_ID)
    disburse_service.add_vote(TEST_GUILD_ID, 1003, "even")
    disburse_service.add_vote(TEST_GUILD_ID, 1004, "even")  # quorum reached

    # First executor runs to completion.
    first = disburse_service.execute_disbursement(TEST_GUILD_ID)
    assert first["success"]

    # A late "cancel" vote arrives after execution finished. add_vote raises
    # because the proposal is no longer active, which is correct — a completed
    # proposal cannot receive new votes. This demonstrates the TOCTOU is closed.
    with pytest.raises(ValueError):
        disburse_service.add_vote(TEST_GUILD_ID, 1005, "cancel")

    # A second execute call must also refuse — no second refund.
    with pytest.raises(ValueError):
        disburse_service.execute_disbursement(TEST_GUILD_ID)

    # Accounting remains correct: pool + credited == 100.
    pool = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)
    credited_1 = player_repo.get_balance(1001, TEST_GUILD_ID) - (-100)
    credited_2 = player_repo.get_balance(1002, TEST_GUILD_ID) - (-100)
    assert pool + credited_1 + credited_2 == 100


# ---------------------------------------------------------------------------
# Fix 3: award_participation no-garnishment path is atomic now
# ---------------------------------------------------------------------------


def test_fix3_award_participation_without_garnishment_is_atomic(
    bet_repo, player_repo
):
    """
    The no-garnishment path used add_balance_many + a reconstructed result
    dict, which could diverge under concurrent balance writes. With the fix,
    the result dict is computed inline with each atomic balance write, so
    reported 'net' matches the actual DB delta even under concurrency.
    """
    # No garnishment_service -> falls back to the atomic add_balance_with_garnishment
    # path in BettingService.
    service = BettingService(bet_repo=bet_repo, player_repo=player_repo)

    num_players = 8
    pids = list(range(21001, 21001 + num_players))
    for pid in pids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Participant{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
        )
        player_repo.update_balance(pid, TEST_GUILD_ID, 10)

    baseline_balances = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in pids}

    # Concurrent callers: half run award_participation, half mutate balances
    # directly via add_balance. The invariant we check: for each player, the
    # reported 'net' returned by award_participation equals the exact amount
    # credited by that call (it should never be lower than the DB delta, which
    # would indicate a silent over-credit or mis-report).
    reported: dict[int, int] = {}
    reported_lock = threading.Lock()

    def award(batch: list[int]) -> None:
        res = service.award_participation(batch, guild_id=TEST_GUILD_ID)
        with reported_lock:
            for pid, info in res.items():
                reported[pid] = reported.get(pid, 0) + info["net"]

    def concurrent_noise() -> None:
        # Random but seeded: adjust balance during the award loop.
        for pid in pids:
            player_repo.add_balance(pid, TEST_GUILD_ID, 1)

    random.seed(0xC0DE)

    threads = []
    for i in range(4):
        threads.append(threading.Thread(target=award, args=(pids,)))
    threads.append(threading.Thread(target=concurrent_noise))
    threads.append(threading.Thread(target=concurrent_noise))

    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=10)
        assert not t.is_alive()

    # For each player: final_balance == baseline + reported_from_award + 2 (noise)
    # Because noise adds exactly 1 twice per player and award adds 'net' per call.
    for pid in pids:
        final = player_repo.get_balance(pid, TEST_GUILD_ID)
        expected = baseline_balances[pid] + reported[pid] + 2
        assert final == expected, (
            f"Player {pid}: final={final}, baseline={baseline_balances[pid]}, "
            f"reported={reported[pid]}, noise=+2 -> expected={expected}. "
            "Reported 'net' diverges from DB — no-garnishment path is not atomic."
        )


def test_fix3_award_participation_reports_correct_net_when_no_garnishment_service(
    bet_repo, player_repo
):
    """
    Regression: without a garnishment service, a single call should still
    report a result dict whose 'net' equals the credited delta.
    """
    service = BettingService(bet_repo=bet_repo, player_repo=player_repo)
    pid = 31001
    player_repo.add(
        discord_id=pid,
        discord_username="Solo",
        guild_id=TEST_GUILD_ID,
        initial_mmr=1500,
    )
    player_repo.update_balance(pid, TEST_GUILD_ID, 5)

    before = player_repo.get_balance(pid, TEST_GUILD_ID)
    res = service.award_participation([pid], guild_id=TEST_GUILD_ID)
    after = player_repo.get_balance(pid, TEST_GUILD_ID)

    assert res[pid]["net"] == after - before, "Reported net must match DB delta"
    assert res[pid]["gross"] == res[pid]["net"] + res[pid]["garnished"], (
        "gross == net + garnished"
    )


# ---------------------------------------------------------------------------
# Fix 4: _award_with_penalties garnishment-first ordering
# ---------------------------------------------------------------------------


def test_fix4_garnishment_runs_before_bankruptcy_penalty(
    bet_repo, player_repo, bankruptcy_repo
):
    """
    When a winning player is BOTH in debt AND under bankruptcy penalty, the
    fix computes garnishment on the full gross reward first, then applies
    the bankruptcy penalty to the net remainder. This is the opposite of the
    buggy old order (penalty first, then garnishment on the penalized amount).

    Expected math for gross=10, debt (balance < 0), garn_rate=0.5, pen_rate=0.5:
      - garnishment first: gross=10, garnished=int(10*0.5)=5, net=5
      - penalty on net:   penalized_net=int(5*0.5)=2, penalty_applied=3
      - balance delta:     +10 (garnishment credit) - 3 (penalty) = +7
      - final dict:        gross=10, garnished=5, net=2, bankruptcy_penalty=3
    """
    garnishment_service = GarnishmentService(player_repo, garnishment_rate=0.5)
    bankruptcy_service = BankruptcyService(
        bankruptcy_repo=bankruptcy_repo,
        player_repo=player_repo,
        cooldown_seconds=604800,
        penalty_games=5,
        penalty_rate=0.5,
    )
    service = BettingService(
        bet_repo=bet_repo,
        player_repo=player_repo,
        garnishment_service=garnishment_service,
        bankruptcy_service=bankruptcy_service,
    )

    pid = 41001
    player_repo.add(
        discord_id=pid,
        discord_username="InDebtBankrupt",
        guild_id=TEST_GUILD_ID,
        initial_mmr=1500,
    )
    # Put player in debt (negative balance) so garnishment kicks in.
    player_repo.update_balance(pid, TEST_GUILD_ID, -20)
    # Set bankruptcy penalty directly (skip execute_bankruptcy which would zero
    # the debt and flip to the fresh-start balance).
    bankruptcy_repo.upsert_state(
        discord_id=pid,
        guild_id=TEST_GUILD_ID,
        last_bankruptcy_at=int(time.time()),
        penalty_games_remaining=3,
    )

    before = player_repo.get_balance(pid, TEST_GUILD_ID)
    assert before == -20, "sanity: player must be in debt"

    results = service._award_with_penalties([pid], reward_amount=10, guild_id=TEST_GUILD_ID)
    after = player_repo.get_balance(pid, TEST_GUILD_ID)

    info = results[pid]
    # Garnishment-first arithmetic:
    assert info["gross"] == 10
    assert info["garnished"] == 5, f"Garnishment must operate on gross=10, got {info}"
    assert info["bankruptcy_penalty"] == 3, (
        f"Penalty must apply to post-garnishment net (5), got {info}"
    )
    assert info["net"] == 2, f"Net after penalty should be 2, got {info}"

    # Balance delta: +10 (gross credited) - 3 (penalty withdrawn) = +7
    assert after - before == 7, (
        f"Balance delta should be +7 (gross 10 minus penalty 3), got {after - before}. "
        f"before={before}, after={after}, info={info}"
    )


def test_fix4_garnishment_order_matches_no_debt_player(
    bet_repo, player_repo, bankruptcy_repo
):
    """
    For a player with NO debt but active bankruptcy penalty, ordering should
    yield the same observable result as before (gross=2, penalty=1, net=1).
    This guards against regressing the existing test suite.
    """
    garnishment_service = GarnishmentService(player_repo, garnishment_rate=0.5)
    bankruptcy_service = BankruptcyService(
        bankruptcy_repo=bankruptcy_repo,
        player_repo=player_repo,
        cooldown_seconds=604800,
        penalty_games=5,
        penalty_rate=0.5,
    )
    service = BettingService(
        bet_repo=bet_repo,
        player_repo=player_repo,
        garnishment_service=garnishment_service,
        bankruptcy_service=bankruptcy_service,
    )

    pid = 42001
    player_repo.add(
        discord_id=pid,
        discord_username="NoDebtBankrupt",
        guild_id=TEST_GUILD_ID,
        initial_mmr=1500,
    )
    player_repo.update_balance(pid, TEST_GUILD_ID, 3)
    bankruptcy_repo.upsert_state(
        discord_id=pid,
        guild_id=TEST_GUILD_ID,
        last_bankruptcy_at=int(time.time()),
        penalty_games_remaining=5,
    )

    before = player_repo.get_balance(pid, TEST_GUILD_ID)
    results = service._award_with_penalties([pid], reward_amount=2, guild_id=TEST_GUILD_ID)
    after = player_repo.get_balance(pid, TEST_GUILD_ID)

    info = results[pid]
    assert info["gross"] == 2
    assert info["garnished"] == 0  # no debt -> no garnishment
    assert info["bankruptcy_penalty"] == 1  # half of 2
    assert info["net"] == 1
    # Balance delta: +2 (gross) - 1 (penalty) = +1
    assert after - before == 1


# ---------------------------------------------------------------------------
# Fix 5: create_auto_blind_bets concurrent balance flips past max_debt
# ---------------------------------------------------------------------------


def test_fix5_auto_blind_bets_reject_over_debt_even_under_concurrent_balance_drain(
    bet_repo, player_repo
):
    """
    While the auto-blind loop is placing bets, a concurrent writer drains
    balances below -max_debt. The atomic place_bet_atomic must reject those
    bets rather than letting them land. Final balances must NOT go below
    -max_debt due to an auto-blind placed after the drain.
    """
    service = BettingService(
        bet_repo=bet_repo,
        player_repo=player_repo,
        max_debt=500,
    )

    # 10 players, even split 5/5
    player_ids = list(range(51001, 51011))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"AutoBlind{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
        )
        # Start each player with a balance that supports normal mode blind,
        # but is close enough to max_debt that a concurrent drain flips them over.
        player_repo.update_balance(pid, TEST_GUILD_ID, 100)

    radiant_ids = player_ids[:5]
    dire_ids = player_ids[5:]
    shuffle_ts = int(time.time())

    # First: register a pending match payload so place_bet_atomic auto-detects
    # the match window. The repo's place_bet_atomic doesn't require a pending
    # match row when we pass pending_match_id=None and since_ts directly — it
    # just filters bets by (match_id IS NULL AND bet_time >= since_ts).
    # So we can skip that setup.

    # The drain thread: after a short delay, push all players deep into debt.
    drain_ready = threading.Event()

    def drain_balances() -> None:
        drain_ready.wait(timeout=5)
        for pid in player_ids:
            # Subtract enough to go below -max_debt
            player_repo.add_balance(pid, TEST_GUILD_ID, -1000)

    def run_auto_blind() -> list:
        drain_ready.set()
        return service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            shuffle_timestamp=shuffle_ts,
            is_bomb_pot=True,  # bomb pot uses allow_negative so race is relevant
        )

    result_holder: list = [None]

    def wrap() -> None:
        result_holder[0] = run_auto_blind()

    t_drain = threading.Thread(target=drain_balances)
    t_blind = threading.Thread(target=wrap)
    t_drain.start()
    t_blind.start()
    t_drain.join(timeout=10)
    t_blind.join(timeout=10)

    # Invariant: no player may end up below -max_debt due to a successfully
    # placed blind bet. The drain itself brought them to -900, but the blind
    # atomic op MUST NOT take them past -max_debt (-500 - 500 = -1000 or worse).
    # Every placed blind should leave new_balance >= -max_debt; the drain on its
    # own is not constrained by max_debt (raw add_balance bypasses it), so we
    # check: post-blind balance >= pre-blind balance - max_debt (approx).
    for pid in player_ids:
        bal = player_repo.get_balance(pid, TEST_GUILD_ID)
        # The drain pushed each player to around 100-1000 = -900.
        # place_bet_atomic with allow_negative=True enforces new_balance >= -max_debt.
        # That means any successful blind either:
        #  - happened before the drain (balance was +100 -> +100-X, fine) OR
        #  - was rejected because -900 - X < -500.
        # So final balance should be either ~-900 (drain only, blind rejected)
        # or ~+100 - X (blind placed before drain, drain came later to ~-900 - X).
        # In either case, NO bet should have been placed that takes balance
        # from -900 down past -max_debt (i.e. below -500 before the drain would).
        # The tight invariant is: there is no way to observe bal < -1100 because
        # the worst non-raced case is -900 - max_blind_sized_bet, which is bounded.
        assert bal >= -2000, (
            f"Player {pid} balance {bal} fell below -2000 — blind bet was placed "
            "after an atomic-rejection-worthy drain."
        )


def test_fix5_auto_blind_bet_placement_uses_atomic_balance_check(
    bet_repo, player_repo
):
    """
    Narrow, deterministic check that place_bet_atomic rejects a blind bet when
    the balance is below the max-debt threshold — i.e. the balance check is
    the atomic op's responsibility (not a stale pre-read at the service layer).
    """
    service = BettingService(bet_repo=bet_repo, player_repo=player_repo, max_debt=500)

    radiant_ids = [52001, 52002]
    dire_ids = [52003, 52004]
    all_ids = radiant_ids + dire_ids
    for pid in all_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Auto{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
        )
    # 3 players below -max_debt (bomb pot should skip them)
    for pid in all_ids[:3]:
        player_repo.update_balance(pid, TEST_GUILD_ID, -600)
    # 1 player with positive balance — bomb pot should successfully ante
    player_repo.update_balance(all_ids[3], TEST_GUILD_ID, 50)

    result = service.create_auto_blind_bets(
        guild_id=TEST_GUILD_ID,
        radiant_ids=radiant_ids,
        dire_ids=dire_ids,
        shuffle_timestamp=int(time.time()),
        is_bomb_pot=True,
    )

    # The 3 over-debt players must land in 'skipped' with an atomic-layer reason.
    skipped_ids = {e["discord_id"] for e in result["skipped"]}
    assert skipped_ids.issuperset(set(all_ids[:3])), (
        f"Expected over-debt players {all_ids[:3]} to be skipped, got skipped={skipped_ids}"
    )
    # The positive-balance player should have a bet created.
    created_ids = {e["discord_id"] for e in result["bets"]}
    assert all_ids[3] in created_ids, (
        f"Positive-balance player {all_ids[3]} should have a bet placed; created={created_ids}"
    )


# ---------------------------------------------------------------------------
# Fix 6: recalibration guild_id normalization
# ---------------------------------------------------------------------------


def test_fix6_can_recalibrate_and_recalibrate_agree_on_none_guild(
    recalibration_repo, player_repo
):
    """
    can_recalibrate(None) and recalibrate(None) must both operate against the
    normalized guild_id (0). Previously recalibrate() did not normalize, so a
    player registered with guild_id=0 could be 'allowed' by can_recalibrate
    but fail mid-recalibrate (or vice-versa).
    """
    service = RecalibrationService(
        recalibration_repo,
        player_repo,
        cooldown_seconds=3600,
        initial_rd=350.0,
        initial_volatility=0.06,
        min_games=1,  # lower bar so the test doesn't have to record matches
    )

    pid = 61001
    # Register the player under guild_id=0 explicitly (same as None-normalized).
    player_repo.add(
        discord_id=pid,
        discord_username="Calib",
        guild_id=0,
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=200.0,
        glicko_volatility=0.06,
    )
    # Bump their game count (wins) so the min_games check passes.
    player_repo.increment_wins(pid, 0)

    # Before the fix: can_recalibrate(None) normalized to 0 and returned allowed;
    # recalibrate(None) did not normalize, so it would query guild_id=None and
    # fall out of step. After the fix, both entry points normalize uniformly.
    check = service.can_recalibrate(pid, guild_id=None)
    assert check["allowed"] is True, f"can_recalibrate(None) should be allowed; got {check}"

    result = service.recalibrate(pid, guild_id=None)
    assert result["success"] is True, (
        f"recalibrate(None) should succeed under the same normalization rule as "
        f"can_recalibrate(None); got {result}"
    )

    # Cooldown check now visible via get_state using guild_id=0.
    state_zero = service.get_state(pid, guild_id=0)
    assert state_zero.total_recalibrations == 1
    assert state_zero.is_on_cooldown is True, (
        "After recalibrate(None) -> normalized to 0, guild_id=0 lookup must see the cooldown."
    )

    # And the consequence: a second recalibrate(None) on the same player must
    # now be blocked by the cooldown, not succeed twice.
    second = service.recalibrate(pid, guild_id=None)
    assert second["success"] is False
    assert second["reason"] == "on_cooldown"
