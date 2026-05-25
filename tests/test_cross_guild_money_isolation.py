"""Verify that money operations in guild 1 do not affect guild 2 balances.

Same discord_id can exist in two guilds. Bet settlement, win bonuses, and
balance mutations in one guild must leave the other guild's balance untouched.
"""

import pytest

from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService

GUILD_1 = 1001
GUILD_2 = 2002

PLAYER_A = 9001  # same discord_id in both guilds
SPECTATOR_1 = 9002  # only in guild 1
SPECTATOR_2 = 9003  # only in guild 1


def _add_player(player_repo, discord_id, guild_id, balance=100):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"P{discord_id}",
        guild_id=guild_id,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 100:
        player_repo.update_balance(discord_id, guild_id, balance)


@pytest.fixture
def setup(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        betting_service=betting_service,
    )

    # Register PLAYER_A in both guilds with the same balance
    _add_player(player_repo, PLAYER_A, GUILD_1, balance=200)
    _add_player(player_repo, PLAYER_A, GUILD_2, balance=200)

    # Register SPECTATOR_1 and SPECTATOR_2 only in guild 1 (for betting)
    _add_player(player_repo, SPECTATOR_1, GUILD_1, balance=200)
    _add_player(player_repo, SPECTATOR_2, GUILD_1, balance=200)

    return {
        "player_repo": player_repo,
        "betting_service": betting_service,
        "match_service": match_service,
    }


def test_balance_add_in_guild1_does_not_affect_guild2(setup):
    """Direct balance addition in guild 1 must not touch guild 2 row."""
    player_repo = setup["player_repo"]

    bal_g2_before = player_repo.get_balance(PLAYER_A, GUILD_2)
    player_repo.add_balance(PLAYER_A, GUILD_1, 50)
    bal_g2_after = player_repo.get_balance(PLAYER_A, GUILD_2)

    assert player_repo.get_balance(PLAYER_A, GUILD_1) == 250
    assert bal_g2_after == bal_g2_before, (
        f"Guild 2 balance changed from {bal_g2_before} to {bal_g2_after} "
        "after a guild-1-only add_balance"
    )


def test_win_bonus_in_guild1_does_not_touch_guild2(setup):
    """award_win_bonus in guild 1 must not credit the guild-2 row."""
    player_repo = setup["player_repo"]
    betting_service = setup["betting_service"]

    bal_g2_before = player_repo.get_balance(PLAYER_A, GUILD_2)
    betting_service.award_win_bonus([PLAYER_A], guild_id=GUILD_1)
    bal_g2_after = player_repo.get_balance(PLAYER_A, GUILD_2)

    assert bal_g2_after == bal_g2_before, (
        f"Guild 2 balance changed from {bal_g2_before} to {bal_g2_after} "
        "after award_win_bonus in guild 1"
    )


def test_bet_settlement_in_guild1_does_not_touch_guild2(setup):
    """Bet settlement (radiant win) in guild 1 must not touch guild-2 balance."""
    player_repo = setup["player_repo"]
    betting_service = setup["betting_service"]
    match_service = setup["match_service"]

    # Need enough players for a shuffle in guild 1 (minimum 2)
    extra_ids = list(range(9010, 9020))
    for pid in extra_ids:
        _add_player(player_repo, pid, GUILD_1, balance=100)

    # Only need 2 players for minimal shuffle
    player_ids_for_shuffle = [PLAYER_A, SPECTATOR_1] + extra_ids[:8]
    match_service.shuffle_players(player_ids_for_shuffle, guild_id=GUILD_1)
    pending = match_service.get_last_shuffle(GUILD_1)

    # Spectator bets in guild 1
    betting_service.place_bet(GUILD_1, SPECTATOR_2, "radiant", 20, pending)

    bal_g2_before = player_repo.get_balance(PLAYER_A, GUILD_2)

    betting_service.settle_bets(999, GUILD_1, "radiant", pending_state=pending)

    bal_g2_after = player_repo.get_balance(PLAYER_A, GUILD_2)
    assert bal_g2_after == bal_g2_before, (
        f"Guild 2 balance changed from {bal_g2_before} to {bal_g2_after} "
        "after bet settlement in guild 1"
    )
