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
    assert loan_repo.consume_next_match_pot(TEST_GUILD_ID) == 0


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

    payload = match_repo.get_pending_match_by_id(pending.pending_match_id)
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

    payload = match_repo.get_pending_match_by_id(pending.pending_match_id)
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
