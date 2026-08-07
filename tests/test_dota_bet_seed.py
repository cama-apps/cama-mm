import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from commands.betting_helpers.bet_actions import bets_action, mybets_action
from commands.draft import DraftCommands
from domain.models.draft import DraftState
from domain.models.pending_match_state import PendingMatchState
from domain.services.draft_service import DraftService
from repositories.bet_repository import BetRepository
from repositories.economy_event_repository import EconomyEventRepository
from repositories.loan_repository import LoanRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.draft_state_manager import DraftStateManager
from services.loan_service import LoanService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID
from utils.formatting import format_betting_display


def _add_players(player_repo, player_ids):
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )


@pytest.fixture
def services(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    loan_repo = LoanRepository(repo_db_path)
    loan_service = LoanService(loan_repo, player_repo)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        betting_service=betting_service,
        loan_service=loan_service,
    )
    return {
        "player_repo": player_repo,
        "loan_repo": loan_repo,
        "match_repo": match_repo,
        "betting_service": betting_service,
        "match_service": match_service,
    }


def test_pool_shuffle_reserves_split_seed_from_nonprofit_fund(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    player_ids = list(range(30000, 30010))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 17)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    assert pending.bet_seed_reserved == 17
    assert pending.bet_seed_radiant == 9
    assert pending.bet_seed_dire == 8
    assert pending.bet_seed_bonus == 0
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0


def test_first_pool_match_claims_stacked_seed_once_for_each_lobby(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    player_ids = list(range(30020, 30050))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 500)

    with patch("utils.game_date.get_game_date", return_value="2026-08-05"):
        open_first = match_service.shuffle_players(
            player_ids[:10],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )
        open_second = match_service.shuffle_players(
            player_ids[10:20],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )
        lowskill_first = match_service.shuffle_players(
            player_ids[20:],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="lowskill",
        )

    open_first_state = match_service.get_last_shuffle(
        TEST_GUILD_ID, open_first["pending_match_id"]
    )
    open_second_state = match_service.get_last_shuffle(
        TEST_GUILD_ID, open_second["pending_match_id"]
    )
    lowskill_first_state = match_service.get_last_shuffle(
        TEST_GUILD_ID, lowskill_first["pending_match_id"]
    )

    assert open_first_state.first_game_pool_reserved == 100
    assert open_first_state.bet_seed_reserved == 150
    assert open_first_state.bet_seed_radiant == 75
    assert open_first_state.bet_seed_dire == 75
    assert open_second_state.first_game_pool_reserved == 0
    assert open_second_state.bet_seed_reserved == 50
    assert lowskill_first_state.first_game_pool_reserved == 100
    assert lowskill_first_state.bet_seed_reserved == 150
    assert lowskill_first_state.bet_seed_radiant == 75
    assert lowskill_first_state.bet_seed_dire == 75
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 0,
        "lowskill": 0,
    }


def test_cancelled_first_pool_match_restores_pool_and_daily_claim(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30060, 30080))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 300)

    with patch("utils.game_date.get_game_date", return_value="2026-08-05"):
        first = match_service.shuffle_players(
            player_ids[:10],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )
        first_state = match_service.get_last_shuffle(
            TEST_GUILD_ID, first["pending_match_id"]
        )
        betting_service.refund_pending_bets(TEST_GUILD_ID, first_state)
        second = match_service.shuffle_players(
            player_ids[10:],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )

    second_state = match_service.get_last_shuffle(
        TEST_GUILD_ID, second["pending_match_id"]
    )
    assert second_state.first_game_pool_reserved == 100
    assert second_state.bet_seed_reserved == 150
    assert second_state.bet_seed_radiant == 75
    assert second_state.bet_seed_dire == 75
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 50
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 0,
        "lowskill": 100,
    }


def test_claimed_first_pool_stays_in_monetary_stock_while_match_is_pending(
    services,
    repo_db_path,
):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    economy_repo = EconomyEventRepository(repo_db_path)
    player_ids = list(range(30090, 30100))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 300)
    stock_before = economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"]

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
    ):
        match_service.shuffle_players(
            player_ids,
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )

    assert economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"] == stock_before


def test_settled_first_game_claim_cannot_be_restored_by_later_abort_retry(
    services,
    repo_db_path,
):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    economy_repo = EconomyEventRepository(repo_db_path)
    player_ids = list(range(30120, 30130))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 300)
    stock_before = economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"]

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
    ):
        result = match_service.shuffle_players(
            player_ids,
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )

    pending = match_service.get_last_shuffle(
        TEST_GUILD_ID,
        result["pending_match_id"],
    )
    betting_service.settle_bets(
        match_id=9001,
        guild_id=TEST_GUILD_ID,
        winning_team="radiant",
        pending_state=pending,
    )

    betting_service.refund_pending_bets(TEST_GUILD_ID, pending)

    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 0,
        "lowskill": 100,
    }
    assert economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"] == stock_before


def test_settled_daily_claim_stays_consumed_after_older_match_restores_pool(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30360, 30390))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 400)

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-04"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
    ):
        older = match_service.shuffle_players(
            player_ids[:10],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )
    older_state = match_service.get_last_shuffle(
        TEST_GUILD_ID,
        older["pending_match_id"],
    )

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
    ):
        current = match_service.shuffle_players(
            player_ids[10:20],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )
    current_state = match_service.get_last_shuffle(
        TEST_GUILD_ID,
        current["pending_match_id"],
    )

    betting_service.settle_bets(
        match_id=9003,
        guild_id=TEST_GUILD_ID,
        winning_team="radiant",
        pending_state=current_state,
    )
    betting_service.refund_pending_bets(TEST_GUILD_ID, current_state)
    betting_service.refund_pending_bets(TEST_GUILD_ID, older_state)

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
    ):
        later = match_service.shuffle_players(
            player_ids[20:],
            guild_id=TEST_GUILD_ID,
            betting_mode="pool",
            lobby_kind="open",
        )
    later_state = match_service.get_last_shuffle(
        TEST_GUILD_ID,
        later["pending_match_id"],
    )

    assert later_state.first_game_pool_reserved == 0
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID)["open"] == 100


def test_first_game_claim_retry_recovers_after_seed_state_persist_failure(services):
    loan_repo = services["loan_repo"]
    match_repo = services["match_repo"]
    match_service = services["match_service"]
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 200)
    pending = PendingMatchState(
        shuffle_timestamp=int(time.time()),
        betting_mode="pool",
        lobby_kind="open",
    )
    match_service._persist_match_state(TEST_GUILD_ID, pending)

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
        patch.object(
            match_service.state_service,
            "persist_state",
            side_effect=RuntimeError("simulated persistence failure"),
        ),
        pytest.raises(RuntimeError, match="simulated persistence failure"),
    ):
        match_service.reserve_betting_seed(TEST_GUILD_ID, pending)

    payload = match_repo.get_pending_match_by_id(
        pending.pending_match_id,
        TEST_GUILD_ID,
    )
    reloaded = PendingMatchState.from_dict(payload)
    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
    ):
        match_service.reserve_betting_seed(TEST_GUILD_ID, reloaded)

    assert reloaded.first_game_pool_reserved == 100
    assert reloaded.bet_seed_reserved == 100
    assert reloaded.bet_seed_radiant == 50
    assert reloaded.bet_seed_dire == 50


def test_settlement_recovers_first_game_claim_when_seed_state_never_persisted(
    services,
    repo_db_path,
):
    loan_repo = services["loan_repo"]
    match_repo = services["match_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    economy_repo = EconomyEventRepository(repo_db_path)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 200)
    stock_before = economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"]
    pending = PendingMatchState(
        shuffle_timestamp=int(time.time()),
        betting_mode="pool",
        lobby_kind="open",
    )
    match_service._persist_match_state(TEST_GUILD_ID, pending)

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch("services.match.shuffle_pending_mixin.DOTA_BET_SEED_AMOUNT", 0),
        patch.object(
            match_service.state_service,
            "persist_state",
            side_effect=RuntimeError("simulated persistence failure"),
        ),
        pytest.raises(RuntimeError, match="simulated persistence failure"),
    ):
        match_service.reserve_betting_seed(TEST_GUILD_ID, pending)

    payload = match_repo.get_pending_match_by_id(
        pending.pending_match_id,
        TEST_GUILD_ID,
    )
    reloaded = PendingMatchState.from_dict(payload)
    betting_service.settle_bets(
        match_id=9002,
        guild_id=TEST_GUILD_ID,
        winning_team="radiant",
        pending_state=reloaded,
    )

    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 100
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 0,
        "lowskill": 100,
    }
    assert economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"] == stock_before


def test_regular_seed_persist_failure_cannot_strand_or_double_deduct(services):
    """The regular seed must get the same durable treatment as the first-game
    claim: the deduction and the payload record commit together, so a persist
    failure cannot strand coins and a reloaded-state retry cannot deduct twice.
    """
    loan_repo = services["loan_repo"]
    match_repo = services["match_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 300)
    pending = PendingMatchState(
        shuffle_timestamp=int(time.time()),
        betting_mode="pool",
        lobby_kind="open",
    )
    match_service._persist_match_state(TEST_GUILD_ID, pending)

    with (
        patch("utils.game_date.get_game_date", return_value="2026-08-05"),
        patch.object(
            match_service.state_service,
            "persist_state",
            side_effect=RuntimeError("simulated persistence failure"),
        ),
        pytest.raises(RuntimeError, match="simulated persistence failure"),
    ):
        match_service.reserve_betting_seed(TEST_GUILD_ID, pending)

    # 300 - 200 daily funding - 50 regular seed: the deduction is recorded on
    # the pending match even though persist_state failed.
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 50
    payload = match_repo.get_pending_match_by_id(
        pending.pending_match_id,
        TEST_GUILD_ID,
    )
    reloaded = PendingMatchState.from_dict(payload)
    assert reloaded.bet_seed_reserved == 150
    assert reloaded.first_game_pool_reserved == 100
    assert reloaded.bet_seed_radiant == 75
    assert reloaded.bet_seed_dire == 75

    # Retrying on the reloaded state must not deduct the fund a second time.
    with patch("utils.game_date.get_game_date", return_value="2026-08-05"):
        match_service.reserve_betting_seed(TEST_GUILD_ID, reloaded)
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 50
    assert reloaded.bet_seed_reserved == 150

    # Abort reconciles the full reservation: regular seed back to the fund,
    # first-game claim back to the open pool.
    betting_service.refund_pending_bets(TEST_GUILD_ID, reloaded)
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 100
    assert loan_repo.get_first_game_pool_balances(TEST_GUILD_ID) == {
        "open": 100,
        "lowskill": 100,
    }


def test_reserve_bet_seed_atomic_retry_deducts_fund_exactly_once(services):
    """A crash-retry re-enters reserve_bet_seed_atomic itself: after a reload
    the in-memory ``bet_seed_reserved > 0`` short-circuit in the mixin may be
    gone, so the repo's payload-keyed idempotency branch must return the
    recorded fields without deducting the nonprofit fund a second time."""
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 300)
    pending = PendingMatchState(
        shuffle_timestamp=int(time.time()),
        betting_mode="pool",
        lobby_kind="open",
    )
    match_service._persist_match_state(TEST_GUILD_ID, pending)

    first = loan_repo.reserve_bet_seed_atomic(
        TEST_GUILD_ID,
        pending.pending_match_id,
        50,
        first_game_reserved=100,
        betting_mode="pool",
    )
    assert first == {
        "bet_seed_reserved": 150,
        "bet_seed_radiant": 75,
        "bet_seed_dire": 75,
        "bet_seed_bonus": 0,
        "first_game_pool_reserved": 100,
    }
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 250

    # Same pending match, fresh call (as a crash-retry would make): the fund
    # must stay deducted exactly once and the recorded fields come back
    # unchanged.
    second = loan_repo.reserve_bet_seed_atomic(
        TEST_GUILD_ID,
        pending.pending_match_id,
        50,
        first_game_reserved=100,
        betting_mode="pool",
    )
    assert second == first
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 250


def test_settlement_recovers_regular_seed_when_seed_state_never_persisted(
    services,
    repo_db_path,
):
    """A house-mode regular seed whose persist_state failed must still flow
    back through settlement instead of being destroyed."""
    loan_repo = services["loan_repo"]
    match_repo = services["match_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    economy_repo = EconomyEventRepository(repo_db_path)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 60)
    stock_before = economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"]
    pending = PendingMatchState(
        shuffle_timestamp=int(time.time()),
        betting_mode="house",
        lobby_kind="open",
    )
    match_service._persist_match_state(TEST_GUILD_ID, pending)

    with (
        patch.object(
            match_service.state_service,
            "persist_state",
            side_effect=RuntimeError("simulated persistence failure"),
        ),
        pytest.raises(RuntimeError, match="simulated persistence failure"),
    ):
        match_service.reserve_betting_seed(TEST_GUILD_ID, pending)

    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 10
    payload = match_repo.get_pending_match_by_id(
        pending.pending_match_id,
        TEST_GUILD_ID,
    )
    reloaded = PendingMatchState.from_dict(payload)
    assert reloaded.bet_seed_reserved == 50
    assert reloaded.bet_seed_bonus == 50

    betting_service.settle_bets(
        match_id=9004,
        guild_id=TEST_GUILD_ID,
        winning_team="radiant",
        pending_state=reloaded,
    )

    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 60
    assert economy_repo.capture_balance_sheet(TEST_GUILD_ID)["monetary_stock"] == stock_before


def test_pool_shuffle_consumes_entire_queued_next_match_pot(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    player_ids = list(range(30100, 30110))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 200)
    with loan_repo.connection() as conn:
        conn.execute(
            "UPDATE nonprofit_fund SET total_collected = 25, next_match_pot = 175 WHERE guild_id = ?",
            (TEST_GUILD_ID,),
        )

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    assert pending.bet_seed_reserved == 175
    assert pending.bet_seed_radiant == 88
    assert pending.bet_seed_dire == 87
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 25
    with loan_repo.connection() as conn:
        row = conn.execute(
            "SELECT COALESCE(next_match_pot, 0) AS amount FROM nonprofit_fund WHERE guild_id = ?",
            (TEST_GUILD_ID,),
        ).fetchone()
    assert row["amount"] == 0


def test_house_shuffle_reserves_partial_seed_as_bonus_pool(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    player_ids = list(range(30050, 30060))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 17)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    assert pending.bet_seed_reserved == 17
    assert pending.bet_seed_radiant == 0
    assert pending.bet_seed_dire == 0
    assert pending.bet_seed_bonus == 17
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0


@pytest.mark.parametrize("betting_mode", ["pool", "house"])
def test_shuffle_with_empty_reserve_stores_zero_seed(services, betting_mode):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    player_ids = list(range(30070, 30080))
    _add_players(player_repo, player_ids)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode=betting_mode)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    assert pending.bet_seed_reserved == 0
    assert pending.bet_seed_radiant == 0
    assert pending.bet_seed_dire == 0
    assert pending.bet_seed_bonus == 0
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0


@pytest.mark.asyncio
async def test_draft_pending_match_reserves_pool_seed_from_nonprofit_fund(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_service = services["match_service"]
    player_ids = list(range(30090, 30100))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 50)
    state = DraftState(
        guild_id=TEST_GUILD_ID,
        radiant_player_ids=player_ids[:5],
        dire_player_ids=player_ids[5:],
        excluded_player_ids=[],
        radiant_hero_pick_order=1,
        dire_hero_pick_order=2,
        draft_message_id=111,
        draft_channel_id=222,
    )
    cog = DraftCommands(
        bot=MagicMock(),
        player_repo=player_repo,
        lobby_manager=MagicMock(),
        draft_state_manager=DraftStateManager(),
        draft_service=DraftService(),
        match_service=match_service,
    )

    with patch("commands.draft.random.random", return_value=1.0):
        pending_match_id = await cog._create_pending_match(TEST_GUILD_ID, state)

    pending = match_service.get_last_shuffle(TEST_GUILD_ID, pending_match_id)
    assert pending is not None
    assert pending.is_draft is True
    assert pending.bet_seed_reserved == 50
    assert pending.bet_seed_radiant == 25
    assert pending.bet_seed_dire == 25
    assert pending.bet_seed_bonus == 0
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0


def test_pool_settlement_pays_losing_seed_and_returns_winning_seed(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30100, 30110))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 100)
    radiant_bettor = 30150
    dire_bettor = 30151
    _add_players(player_repo, [radiant_bettor, dire_bettor])
    player_repo.add_balance(radiant_bettor, TEST_GUILD_ID, 100)
    player_repo.add_balance(dire_bettor, TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_lock_until = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, radiant_bettor, "radiant", 10, pending)
    betting_service.place_bet(TEST_GUILD_ID, dire_bettor, "dire", 10, pending)

    distributions = betting_service.settle_bets(
        301, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    assert sum(w["payout"] for w in distributions["winners"]) == 45
    assert player_repo.get_balance(radiant_bettor, TEST_GUILD_ID) == 138
    assert player_repo.get_balance(dire_bettor, TEST_GUILD_ID) == 93
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 75


def test_settlement_returns_all_seed_when_match_has_no_bets(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30170, 30180))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    distributions = betting_service.settle_bets(
        306, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    assert distributions["seed_returned"] == [{"amount": 50}]
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 100


def test_settlement_zeroes_persisted_seed_before_retry(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_repo = services["match_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30180, 30190))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    betting_service.settle_bets(308, TEST_GUILD_ID, "radiant", pending_state=pending)
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 100

    payload = match_repo.get_pending_match_by_id(pending.pending_match_id, TEST_GUILD_ID)
    reloaded = PendingMatchState.from_dict(payload)
    assert reloaded.bet_seed_reserved == 0
    assert reloaded.bet_seed_radiant == 0
    assert reloaded.bet_seed_dire == 0
    assert reloaded.bet_seed_bonus == 0

    betting_service.settle_bets(309, TEST_GUILD_ID, "radiant", pending_state=reloaded)
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 100


def test_pool_settlement_burns_losers_when_only_seed_is_on_winning_side(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30200, 30210))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 100)
    dire_bettor = 30250
    _add_players(player_repo, [dire_bettor])
    player_repo.add_balance(dire_bettor, TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_lock_until = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, dire_bettor, "dire", 10, pending)

    distributions = betting_service.settle_bets(
        302, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    assert distributions["winners"] == []
    assert distributions["losers"][0]["discord_id"] == dire_bettor
    assert "refunded" not in distributions["losers"][0]
    assert player_repo.get_balance(dire_bettor, TEST_GUILD_ID) == 93
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 100


def test_house_settlement_returns_seed_bonus_when_no_real_winners(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30270, 30280))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 100)
    bettor = 30290
    _add_players(player_repo, [bettor])
    player_repo.add_balance(bettor, TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_lock_until = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, bettor, "dire", 10, pending)

    distributions = betting_service.settle_bets(
        307, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    assert distributions["winners"] == []
    assert distributions["seed_returned"] == [{"amount": 50}]
    assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 93
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 100


def test_house_settlement_adds_seed_bonus_to_winning_bettors(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30300, 30310))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 100)
    bettor = 30350
    _add_players(player_repo, [bettor])
    player_repo.add_balance(bettor, TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_lock_until = int(time.time()) + 600
    assert pending.bet_seed_bonus == 50
    betting_service.place_bet(TEST_GUILD_ID, bettor, "radiant", 10, pending)

    distributions = betting_service.settle_bets(
        303, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    assert sum(w["payout"] for w in distributions["winners"]) == 70
    assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 163
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 50


def test_abort_refunds_reserved_seed_even_without_bets(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30400, 30410))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 40)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    assert pending.bet_seed_reserved == 40
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0

    refunded = betting_service.refund_pending_bets(TEST_GUILD_ID, pending)

    assert refunded == 0
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 40


def test_abort_refund_zeroes_persisted_seed_before_retry(services):
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    match_repo = services["match_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30420, 30430))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 40)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    refunded = betting_service.refund_pending_bets(TEST_GUILD_ID, pending)
    assert refunded == 0
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 40

    payload = match_repo.get_pending_match_by_id(pending.pending_match_id, TEST_GUILD_ID)
    reloaded = PendingMatchState.from_dict(payload)
    assert reloaded.bet_seed_reserved == 0
    assert reloaded.bet_seed_radiant == 0
    assert reloaded.bet_seed_dire == 0
    assert reloaded.bet_seed_bonus == 0

    betting_service.refund_pending_bets(TEST_GUILD_ID, reloaded)
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 40


def test_pool_betting_display_includes_seeded_odds():
    field_name, field_value = format_betting_display(
        10,
        0,
        "pool",
        seed_radiant=25,
        seed_dire=25,
    )

    assert field_name == "💰 Pool Betting"
    assert "Radiant: 10" in field_value
    assert "(3.50x)" in field_value
    assert "Dire: 0" in field_value
    assert "Seed: Radiant 25" in field_value
    assert "Dire 25" in field_value


def test_house_betting_display_includes_seed_bonus():
    field_name, field_value = format_betting_display(
        10,
        0,
        "house",
        seed_bonus=50,
    )

    assert field_name == "💰 House Betting (1:1)"
    assert "Winner bonus: 50" in field_value


@pytest.mark.asyncio
async def test_mybets_pool_potential_payout_uses_seeded_pool(monkeypatch):
    monkeypatch.setattr(
        "commands.betting_helpers.bet_actions.safe_defer",
        AsyncMock(return_value=True),
    )
    user_id = 30650
    pending = SimpleNamespace(
        pending_match_id=501,
        betting_mode="pool",
        bet_seed_radiant=25,
        bet_seed_dire=25,
        bet_seed_bonus=0,
    )
    cog = MagicMock()
    cog.betting_service.bet_repo.get_all_player_pending_bets.return_value = [
        {
            "pending_match_id": 501,
            "team_bet_on": "radiant",
            "amount": 10,
            "leverage": 1,
            "bet_time": 1_700_000_000,
        }
    ]
    cog.match_service.state_service.get_all_pending_matches.return_value = [pending]
    cog.betting_service.get_pot_odds.return_value = {"radiant": 10, "dire": 0}
    interaction = MagicMock()
    interaction.guild.id = TEST_GUILD_ID
    interaction.user.id = user_id
    interaction.followup.send = AsyncMock()
    interaction.response.send_message = AsyncMock()

    await mybets_action(cog, interaction)

    message = interaction.followup.send.call_args.args[0]
    assert "If Radiant wins: ~35" in message
    assert "(2.50:1)" in message


@pytest.mark.asyncio
async def test_mybets_house_potential_payout_includes_seed_bonus_share(monkeypatch):
    monkeypatch.setattr(
        "commands.betting_helpers.bet_actions.safe_defer",
        AsyncMock(return_value=True),
    )
    user_id = 30651
    pending = SimpleNamespace(
        pending_match_id=502,
        betting_mode="house",
        bet_seed_radiant=0,
        bet_seed_dire=0,
        bet_seed_bonus=50,
    )
    cog = MagicMock()
    cog.betting_service.bet_repo.get_all_player_pending_bets.return_value = [
        {
            "pending_match_id": 502,
            "team_bet_on": "radiant",
            "amount": 10,
            "leverage": 1,
            "bet_time": 1_700_000_000,
        }
    ]
    cog.match_service.state_service.get_all_pending_matches.return_value = [pending]
    cog.betting_service.get_pot_odds.return_value = {"radiant": 20, "dire": 0}
    interaction = MagicMock()
    interaction.guild.id = TEST_GUILD_ID
    interaction.user.id = user_id
    interaction.followup.send = AsyncMock()
    interaction.response.send_message = AsyncMock()

    await mybets_action(cog, interaction)

    message = interaction.followup.send.call_args.args[0]
    assert "If Radiant wins: 45" in message
    assert "1:1 + ~25 bonus" in message


@pytest.mark.asyncio
async def test_bets_house_display_shows_bonus_not_pool_odds(monkeypatch):
    monkeypatch.setattr(
        "commands.betting_helpers.bet_actions.safe_defer",
        AsyncMock(return_value=True),
    )
    monkeypatch.setattr(
        "commands.betting_helpers.bet_actions.has_admin_permission",
        lambda interaction: True,
    )
    pending = SimpleNamespace(
        pending_match_id=503,
        betting_mode="house",
        bet_seed_radiant=0,
        bet_seed_dire=0,
        bet_seed_bonus=50,
        bet_lock_until=None,
    )
    cog = MagicMock()
    cog.match_service.state_service.get_all_pending_matches.return_value = [pending]
    cog.betting_service.get_all_pending_bets.return_value = [
        {
            "discord_id": 30652,
            "team_bet_on": "radiant",
            "amount": 10,
            "leverage": 1,
            "is_blind": 0,
        }
    ]
    cog.betting_service.get_pot_odds.return_value = {"radiant": 10, "dire": 0}
    interaction = MagicMock()
    interaction.guild.id = TEST_GUILD_ID
    interaction.user.id = 30652
    interaction.followup.send = AsyncMock()
    interaction.response.send_message = AsyncMock()

    await bets_action(cog, interaction, None)

    embed = interaction.followup.send.call_args.kwargs["embed"]
    assert "House Bets" in embed.title
    current = embed.fields[0].value
    assert "(1:1)" in current
    assert "Winner bonus: 50" in current
    assert "Pool" not in embed.title
    assert "3.50x" not in current


def test_roll_and_russianroulette_extensions_are_removed():
    import bot

    assert "commands.roll" not in bot.EXTENSIONS
    assert "commands.russianroulette" not in bot.EXTENSIONS
    assert "commands.mafia" in bot.EXTENSIONS


def test_house_shuffle_routes_queued_pot_to_bonus_and_pays_it_out(services):
    """A queued next-match pot on a HOUSE match must land in bet_seed_bonus.

    Regression: the pot used to be split radiant/dire even in house mode, but
    house settlement only pays the bonus field — the pot was consumed without
    being paid or returned (coins destroyed).
    """
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30500, 30510))
    _add_players(player_repo, player_ids)
    loan_repo.add_to_nonprofit_fund(TEST_GUILD_ID, 200)
    with loan_repo.connection() as conn:
        conn.execute(
            "UPDATE nonprofit_fund SET total_collected = 25, next_match_pot = 175 WHERE guild_id = ?",
            (TEST_GUILD_ID,),
        )
    bettor = 30550
    _add_players(player_repo, [bettor])
    player_repo.add_balance(bettor, TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    assert pending.bet_seed_reserved == 175
    assert pending.bet_seed_radiant == 0
    assert pending.bet_seed_dire == 0
    assert pending.bet_seed_bonus == 175

    pending.bet_lock_until = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, bettor, "radiant", 10, pending)
    assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 93

    distributions = betting_service.settle_bets(
        310, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    # 1:1 house payout (20) + full queued pot (175): every reserved coin is
    # paid out; the fund keeps exactly what was never queued.
    assert sum(w["payout"] for w in distributions["winners"]) == 195
    assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 288
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 25


def test_house_settlement_folds_legacy_split_seed_into_bonus(services):
    """A persisted house state with radiant/dire-split seeds (written before
    mode-aware routing) must still pay the full pot at settlement."""
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30600, 30610))
    _add_players(player_repo, player_ids)
    bettor = 30650
    _add_players(player_repo, [bettor])
    player_repo.add_balance(bettor, TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_seed_reserved = 175
    pending.bet_seed_radiant = 88
    pending.bet_seed_dire = 87
    pending.bet_seed_bonus = 0
    match_service._persist_match_state(TEST_GUILD_ID, pending)

    pending.bet_lock_until = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, bettor, "radiant", 10, pending)

    distributions = betting_service.settle_bets(
        311, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    assert sum(w["payout"] for w in distributions["winners"]) == 195
    assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 288
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 0


def test_house_settlement_returns_legacy_split_seed_when_no_winners(services):
    """Same legacy split-seed shape, but with no winning bets the folded pot
    must flow back to the nonprofit fund instead of vanishing."""
    player_repo = services["player_repo"]
    loan_repo = services["loan_repo"]
    betting_service = services["betting_service"]
    match_service = services["match_service"]
    player_ids = list(range(30700, 30710))
    _add_players(player_repo, player_ids)
    bettor = 30750
    _add_players(player_repo, [bettor])
    player_repo.add_balance(bettor, TEST_GUILD_ID, 100)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_seed_reserved = 175
    pending.bet_seed_radiant = 88
    pending.bet_seed_dire = 87
    pending.bet_seed_bonus = 0
    match_service._persist_match_state(TEST_GUILD_ID, pending)

    pending.bet_lock_until = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, bettor, "dire", 10, pending)

    distributions = betting_service.settle_bets(
        312, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    assert distributions["winners"] == []
    assert distributions["seed_returned"] == [{"amount": 175}]
    assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 93
    assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == 175
