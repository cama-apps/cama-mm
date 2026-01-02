import os
import tempfile

from config import HOUSE_PAYOUT_MULTIPLIER, JOPACOIN_WIN_REWARD
from services.match_service import MatchService
from services.betting_service import BettingService
from repositories.player_repository import PlayerRepository
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository


def _seed_players(repo: PlayerRepository, ids):
    for pid in ids:
        repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            preferred_roles=["1", "2", "3", "4", "5"],
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )


def test_betting_flow_with_repos():
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        player_repo = PlayerRepository(db_path)
        bet_repo = BetRepository(db_path)
        match_repo = MatchRepository(db_path)
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
        _seed_players(player_repo, all_ids)

        pending = match_service.shuffle_players(all_ids, guild_id=42)
        assert pending["radiant_team"]

        shuffle_state = match_service.get_last_shuffle(42)
        radiant_team_ids = shuffle_state["radiant_team_ids"]
        dire_team_ids = shuffle_state["dire_team_ids"]

        bettor_id = radiant_team_ids[0]
        bet_amount = 2
        betting_service.place_bet(
            guild_id=42,
            discord_id=bettor_id,
            team="radiant",
            amount=bet_amount,
            pending_state=shuffle_state,
        )

        result = match_service.record_match("radiant", guild_id=42)
        assert result["match_id"] > 0
        assert match_repo.get_pending_match(42) is None

        # Bettor started with 3, spent bet_amount, then won payout and win bonus
        expected = (
            3
            - bet_amount
            + int(bet_amount * (1 + HOUSE_PAYOUT_MULTIPLIER))
            + JOPACOIN_WIN_REWARD
        )
        assert player_repo.get_balance(bettor_id) == expected

        # Losing side should have received participation (1 coin each)
        for pid in dire_team_ids:
            assert player_repo.get_balance(pid) == 4  # initial 3 + 1 participation
    finally:
        try:
            os.unlink(db_path)
        except OSError:
            pass


def test_place_bet_atomic_debits_and_prevents_duplicate():
    """
    Ensure bet placement is atomic (debit + insert) and prevents double-bets for the same match window.
    """
    import time

    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        player_repo = PlayerRepository(db_path)
        bet_repo = BetRepository(db_path)

        pid = 12345
        player_repo.add(
            discord_id=pid,
            discord_username="Player12345",
            preferred_roles=["1", "2", "3", "4", "5"],
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        now_ts = int(time.time())
        since_ts = now_ts - 5
        bet_id = bet_repo.place_bet_atomic(
            guild_id=42,
            discord_id=pid,
            team="radiant",
            amount=2,
            bet_time=now_ts,
            since_ts=since_ts,
        )
        assert bet_id > 0
        assert player_repo.get_balance(pid) == 1  # default 3 - 2

        # Duplicate bet in same match window should be rejected (and not debit again).
        try:
            bet_repo.place_bet_atomic(
                guild_id=42,
                discord_id=pid,
                team="radiant",
                amount=1,
                bet_time=now_ts + 1,
                since_ts=since_ts,
            )
            raise AssertionError("Expected duplicate bet to raise ValueError")
        except ValueError as exc:
            assert "already have a bet" in str(exc)

        assert player_repo.get_balance(pid) == 1
    finally:
        try:
            os.unlink(db_path)
        except OSError:
            pass


def test_place_bet_against_pending_match_enforces_team_and_lock():
    import time

    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        player_repo = PlayerRepository(db_path)
        bet_repo = BetRepository(db_path)
        match_repo = MatchRepository(db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        # Create a pending match via shuffle so it is persisted to pending_matches.
        player_ids = list(range(5000, 5010))
        _seed_players(player_repo, player_ids)

        match_service.shuffle_players(player_ids, guild_id=7)
        state = match_service.get_last_shuffle(7)
        assert state is not None

        # Ensure betting open for the first check (and persist the updated lock into DB)
        now_ts = int(time.time())
        state["bet_lock_until"] = now_ts + 600
        match_repo.save_pending_match(7, match_service._build_pending_match_payload(state))  # type: ignore[attr-defined]

        radiant_pid = state["radiant_team_ids"][0]
        # Radiant participant betting on Dire must be rejected (enforced via DB payload).
        try:
            bet_repo.place_bet_against_pending_match_atomic(
                guild_id=7,
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
        match_repo.save_pending_match(7, match_service._build_pending_match_payload(state))  # type: ignore[attr-defined]
        try:
            bet_repo.place_bet_against_pending_match_atomic(
                guild_id=7,
                discord_id=radiant_pid,
                team="radiant",
                amount=1,
                bet_time=now_ts,
            )
            raise AssertionError("Expected lock enforcement to raise ValueError")
        except ValueError as exc:
            assert "closed" in str(exc).lower()
    finally:
        try:
            os.unlink(db_path)
        except OSError:
            pass