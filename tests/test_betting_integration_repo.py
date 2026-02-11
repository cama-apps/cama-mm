import time

import pytest

from config import HOUSE_PAYOUT_MULTIPLIER, JOPACOIN_WIN_REWARD
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService

TEST_GUILD_ID = 12345


def _seed_players(repo: PlayerRepository, ids, guild_id: int = TEST_GUILD_ID):
    for pid in ids:
        repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=guild_id,
            preferred_roles=["1", "2", "3", "4", "5"],
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )


def test_betting_flow_with_repos(repo_db_path):
    guild_id = 42
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=betting_service,
    )

    radiant_ids = [1000, 1001, 1002, 1003, 1004]
    dire_ids = [2000, 2001, 2002, 2003, 2004]
    all_ids = radiant_ids + dire_ids
    _seed_players(player_repo, all_ids, guild_id=guild_id)

    pending = match_service.shuffle_players(all_ids, guild_id=guild_id, betting_mode="house")
    assert pending["radiant_team"]

    shuffle_state = match_service.get_last_shuffle(guild_id)
    radiant_team_ids = shuffle_state["radiant_team_ids"]
    dire_team_ids = shuffle_state["dire_team_ids"]

    bettor_id = radiant_team_ids[0]
    bet_amount = 2
    betting_service.place_bet(
        guild_id=guild_id,
        discord_id=bettor_id,
        team="radiant",
        amount=bet_amount,
        pending_state=shuffle_state,
    )

    result = match_service.record_match("radiant", guild_id=guild_id)
    assert result["match_id"] > 0
    assert match_repo.get_pending_match(guild_id) is None

    # Bettor started with 3, spent bet_amount, then won payout and win bonus
    expected = (
        3 - bet_amount + int(bet_amount * (1 + HOUSE_PAYOUT_MULTIPLIER)) + JOPACOIN_WIN_REWARD
    )
    assert player_repo.get_balance(bettor_id, guild_id=guild_id) == expected

    # Losing side should have received participation (1 coin each)
    for pid in dire_team_ids:
        assert player_repo.get_balance(pid, guild_id=guild_id) == 4  # initial 3 + 1 participation


def test_place_bet_atomic_debits_and_allows_same_team(repo_db_path):
    """
    Ensure bet placement is atomic (debit + insert) and allows additional bets on the same team.
    Bets on the opposite team should be rejected.
    """
    guild_id = 42
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)

    pid = 12345
    player_repo.add(
        discord_id=pid,
        discord_username="Player12345",
        guild_id=guild_id,
        preferred_roles=["1", "2", "3", "4", "5"],
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    # Give player more balance for multiple bets
    player_repo.add_balance(pid, guild_id=guild_id, amount=10)  # Now has 13

    now_ts = int(time.time())
    since_ts = now_ts - 5
    bet_id = bet_repo.place_bet_atomic(
        guild_id=guild_id,
        discord_id=pid,
        team="radiant",
        amount=2,
        bet_time=now_ts,
        since_ts=since_ts,
    )
    assert bet_id > 0
    assert player_repo.get_balance(pid, guild_id=guild_id) == 11  # 13 - 2

    # Additional bet on same team should succeed
    bet_id2 = bet_repo.place_bet_atomic(
        guild_id=guild_id,
        discord_id=pid,
        team="radiant",
        amount=1,
        bet_time=now_ts + 1,
        since_ts=since_ts,
    )
    assert bet_id2 > 0
    assert player_repo.get_balance(pid, guild_id=guild_id) == 10  # 11 - 1

    # Bet on opposite team should be rejected
    try:
        bet_repo.place_bet_atomic(
            guild_id=guild_id,
            discord_id=pid,
            team="dire",
            amount=1,
            bet_time=now_ts + 2,
            since_ts=since_ts,
        )
        raise AssertionError("Expected opposite team bet to raise ValueError")
    except ValueError as exc:
        assert "already have bets on Radiant" in str(exc)

    # Balance should not have changed after rejected bet
    assert player_repo.get_balance(pid, guild_id=guild_id) == 10


def test_place_bet_against_pending_match_enforces_team_and_lock(repo_db_path):
    guild_id = 7
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=betting_service,
    )

    # Create a pending match via shuffle so it is persisted to pending_matches.
    player_ids = list(range(5000, 5010))
    _seed_players(player_repo, player_ids, guild_id=guild_id)

    match_service.shuffle_players(player_ids, guild_id=guild_id)
    state = match_service.get_last_shuffle(guild_id)
    assert state is not None

    # Ensure betting open for the first check (and persist the updated lock into DB)
    now_ts = int(time.time())
    state["bet_lock_until"] = now_ts + 600
    pending_match_id = state.get("pending_match_id")
    assert pending_match_id is not None, "Shuffle should set pending_match_id"
    match_repo.update_pending_match(pending_match_id, match_service._build_pending_match_payload(state))  # type: ignore[attr-defined]

    radiant_pid = state["radiant_team_ids"][0]
    # Radiant participant betting on Dire must be rejected (enforced via DB payload).
    try:
        bet_repo.place_bet_against_pending_match_atomic(
            guild_id=guild_id,
            discord_id=radiant_pid,
            team="dire",
            amount=1,
            bet_time=now_ts,
        )
        raise AssertionError("Expected team restriction to raise ValueError")
    except ValueError as exc:
        assert "Participants on Radiant" in str(exc)

    # Lock enforcement via DB payload (without relying on in-memory pending_state)
    state["bet_lock_until"] = now_ts - 1
    match_repo.update_pending_match(pending_match_id, match_service._build_pending_match_payload(state))  # type: ignore[attr-defined]
    try:
        bet_repo.place_bet_against_pending_match_atomic(
            guild_id=guild_id,
            discord_id=radiant_pid,
            team="radiant",
            amount=1,
            bet_time=now_ts,
        )
        raise AssertionError("Expected lock enforcement to raise ValueError")
    except ValueError as exc:
        assert "closed" in str(exc).lower()
