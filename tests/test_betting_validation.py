import time

import pytest

from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def services(repo_db_path):
    """Create test services using centralized fast fixture."""
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

    yield {
        "match_service": match_service,
        "betting_service": betting_service,
        "player_repo": player_repo,
        "db_path": repo_db_path,
    }


def test_place_bet_requires_pending_state(services):
    with pytest.raises(ValueError, match="No pending match"):
        services["betting_service"].place_bet(1, 1001, "radiant", 5, None)


def test_bet_lock_enforced(services):
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    player_ids = list(range(1000, 1013))
    # Add all players to database before shuffling
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
    player_repo.add_balance(1001, TEST_GUILD_ID, 10)
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending["bet_lock_until"] = int(time.time()) - 1

    with pytest.raises(ValueError, match="closed"):
        betting_service.place_bet(TEST_GUILD_ID, 1001, "radiant", 5, pending)


def test_participant_can_only_bet_on_own_team(services):
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    player_ids = list(range(1000, 1010))
    # Add all players to database before shuffling
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
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    participant = pending["radiant_team_ids"][0]
    spectator = 2000
    player_repo.add(
        discord_id=spectator,
        discord_username="Spectator",
        dotabuff_url="https://dotabuff.com/players/1",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_balance(participant, TEST_GUILD_ID, 20)
    player_repo.add_balance(spectator, TEST_GUILD_ID, 20)

    # Ensure betting is still open
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600  # 10 minutes in the future

    with pytest.raises(ValueError, match="Participants on Radiant"):
        betting_service.place_bet(TEST_GUILD_ID, participant, "dire", 5, pending)

    # Spectator can bet on either team initially
    betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 5, pending)

    # Can add another bet on the same team
    betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 3, pending)

    # But cannot bet on the opposite team after betting
    with pytest.raises(ValueError, match="already have bets on Dire"):
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)


class TestBettingModeValidation:
    """Validation of betting mode argument to shuffle_players."""

    def test_shuffle_betting_mode_validation(self, services):
        """Invalid betting mode should raise an error."""
        match_service = services["match_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(9800, 9810))
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

        with pytest.raises(ValueError, match="betting_mode must be"):
            match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="invalid")


class TestMultipleBetsValidation:
    """Validation rules around multi-bet placement (team gates, balance, debt)."""

    def test_cannot_bet_opposite_team_after_betting(self, services):
        """Once bet on a team, cannot bet on the opposite team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(10200, 10210))
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

        spectator = 10300
        player_repo.add(
            discord_id=spectator,
            discord_username="OppositeTeamSpectator",
            dotabuff_url="https://dotabuff.com/players/10300",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Bet on radiant first
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending)

        # Try to bet on dire - should fail
        with pytest.raises(ValueError, match="already have bets on Radiant"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 10, pending)

    def test_multiple_bets_balance_enforced_each_bet(self, services):
        """Each bet checks balance independently, so multiple small bets can fail if balance runs out."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11400, 11410))
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

        spectator = 11500
        player_repo.add(
            discord_id=spectator,
            discord_username="LimitedBalanceSpectator",
            dotabuff_url="https://dotabuff.com/players/11500",
        guild_id=TEST_GUILD_ID,
    )
        # Only has 10 jopacoin (3 starting + 7 top-up)
        player_repo.add_balance(spectator, TEST_GUILD_ID, 7)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # First bet of 5 succeeds
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 5

        # Second bet of 3 succeeds
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 3, pending)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 2

        # Third bet of 5 fails (only 2 left)
        with pytest.raises(ValueError, match="Insufficient balance"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)

    def test_participant_can_place_multiple_bets_on_own_team(self, services):
        """Match participant can place multiple bets on their own team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11600, 11610))
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
            player_repo.add_balance(pid, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Get a participant from radiant team
        radiant_player = pending["radiant_team_ids"][0]

        # First bet on own team succeeds
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 10, pending)

        # Second bet on own team also succeeds
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 15, pending)

        # Third bet with leverage succeeds
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 5, pending, leverage=2)

        # Verify all bets recorded
        bets = betting_service.get_pending_bets(TEST_GUILD_ID, radiant_player, pending_state=pending)
        assert len(bets) == 3

        # Trying to bet on opposite team fails (participant restriction)
        with pytest.raises(ValueError, match="Participants on Radiant can only bet on Radiant"):
            betting_service.place_bet(TEST_GUILD_ID, radiant_player, "dire", 5, pending)

    def test_leverage_respects_max_debt(self, services):
        """Leverage bets cannot push you past MAX_DEBT."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11700, 11710))
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

        spectator = 11800
        player_repo.add(
            discord_id=spectator,
            discord_username="DebtSpectator",
            dotabuff_url="https://dotabuff.com/players/11800",
        guild_id=TEST_GUILD_ID,
    )
        # Start with 100 jopacoin (3 default + 97)
        player_repo.add_balance(spectator, TEST_GUILD_ID, 97)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Trying to bet 150 at 5x = 750 effective would go to -650 (past -500 MAX_DEBT)
        with pytest.raises(ValueError, match="exceed maximum debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 150, pending, leverage=5)

        # But 100 at 5x = 500 effective, goes to -400 (within limit)
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 100, pending, leverage=5)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == -400

        # Once in debt, cannot place any more bets
        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=2)

    def test_in_debt_user_cannot_place_any_bet(self, services):
        """User in debt cannot place any bets (1x or leverage)."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11900, 11910))
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

        spectator = 12000
        player_repo.add(
            discord_id=spectator,
            discord_username="DebtNoBets",
            dotabuff_url="https://dotabuff.com/players/12000",
        guild_id=TEST_GUILD_ID,
    )
        # Put them in debt: start with 3, then go negative
        player_repo.add_balance(spectator, TEST_GUILD_ID, 47)  # Has 50

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Place leverage bet to go into debt
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 100, pending, leverage=5)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == -450

        # Cannot place 1x bet while in debt
        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 1, pending)

        # Cannot place leverage bet while in debt either
        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=5)

    def test_spectator_bet_then_opposite_team_blocked(self, services):
        """Spectator who bet on one team cannot switch to opposite team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12100, 12110))
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

        spectator = 12200
        player_repo.add(
            discord_id=spectator,
            discord_username="SwitchAttempt",
            dotabuff_url="https://dotabuff.com/players/12200",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Bet on dire first
        betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 10, pending)

        # Can add more to dire
        betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 15, pending)

        # Cannot switch to radiant
        with pytest.raises(ValueError, match="already have bets on Dire"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)


class TestBlindBetsValidation:
    """Threshold and debt-skipping validation for blind bets."""

    def test_create_auto_blind_bets_threshold(self, services):
        """Players below threshold are skipped."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12400, 12410))
        for i, pid in enumerate(player_ids):
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
            # Alternate: some have 100, some have only 30 (below 50 threshold)
            if i % 2 == 0:
                player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total
            else:
                player_repo.add_balance(pid, TEST_GUILD_ID, 27)  # 30 total (below threshold)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # Only 5 players (those with 100) should have blind bets
        assert result["created"] == 5
        assert len(result["skipped"]) == 5

        # Check skipped reasons
        for skip in result["skipped"]:
            assert "threshold" in skip["reason"]

    def test_create_auto_blind_bets_in_debt(self, services):
        """Players in debt are skipped."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12600, 12610))
        for i, pid in enumerate(player_ids):
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
            if i < 5:
                player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total
            else:
                # Put in debt
                player_repo.add_balance(pid, TEST_GUILD_ID, -103)  # -100 balance

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # Only 5 non-debt players should have blind bets
        assert result["created"] == 5
        assert len(result["skipped"]) == 5


class TestBombPotValidation:
    """Mandatory ante / threshold-bypass / debt rules for bomb pot."""

    def test_bomb_pot_mandatory_ante_no_threshold(self, services):
        """Bomb pot ante is mandatory - players below threshold still participate."""
        player_repo = services["player_repo"]
        betting_service = services["betting_service"]

        # Set up players with LOW balance (below normal threshold of 50)
        radiant_ids = [2001, 2002, 2003, 2004, 2005]
        dire_ids = [2006, 2007, 2008, 2009, 2010]
        for pid in radiant_ids + dire_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=3000,
                glicko_rating=1500.0,
                guild_id=TEST_GUILD_ID,
            )
            # Low balance: 20 JC (below 50 threshold)
            player_repo.add_balance(pid, TEST_GUILD_ID, 17)  # 20 total

        now_ts = int(time.time())

        # Normal mode: should skip all players (below threshold)
        normal_result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            shuffle_timestamp=now_ts,
            is_bomb_pot=False,
        )
        assert normal_result["created"] == 0
        assert len(normal_result["skipped"]) == 10

        # Bomb pot mode: should include all players (mandatory)
        bomb_pot_result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            shuffle_timestamp=now_ts + 1,
            is_bomb_pot=True,
        )
        assert bomb_pot_result["created"] == 10
        assert len(bomb_pot_result["skipped"]) == 0

        # Each player bets: 10% of 20 = 2 + 10 ante = 12 JC
        # Total: 10 * 12 = 120
        assert bomb_pot_result["total_radiant"] + bomb_pot_result["total_dire"] == 120

    def test_bomb_pot_zero_balance_still_antes(self, services):
        """Players with zero balance still ante in bomb pot (can go negative)."""
        player_repo = services["player_repo"]
        betting_service = services["betting_service"]

        # Player with exactly 0 balance
        player_id = 6001
        player_repo.add(
            discord_id=player_id,
            discord_username="ZeroPlayer",
            dotabuff_url="https://dotabuff.com/players/6001",
            guild_id=TEST_GUILD_ID,
        )
        # Remove the default 3 JC
        player_repo.add_balance(player_id, TEST_GUILD_ID, -3)
        assert player_repo.get_balance(player_id, TEST_GUILD_ID) == 0

        now_ts = int(time.time())

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=[player_id],
            dire_ids=[],
            shuffle_timestamp=now_ts,
            is_bomb_pot=True,
        )

        # Should create bet even with 0 balance
        assert result["created"] == 1
        # 10% of 0 = 0 + 10 ante = 10 JC
        assert result["total_radiant"] == 10
        # Balance should be negative now (-10)
        assert player_repo.get_balance(player_id, TEST_GUILD_ID) == -10

    def test_bomb_pot_player_already_in_debt_can_ante(self, services):
        """Players already in debt can still ante in bomb pot (up to max_debt)."""
        player_repo = services["player_repo"]
        betting_service = services["betting_service"]

        # Player already in debt (-100 balance)
        player_id = 7001
        player_repo.add(
            discord_id=player_id,
            discord_username="DebtPlayer",
            dotabuff_url="https://dotabuff.com/players/7001",
            guild_id=TEST_GUILD_ID,
        )
        # Set balance to -100 (3 default - 103 = -100)
        player_repo.add_balance(player_id, TEST_GUILD_ID, -103)
        assert player_repo.get_balance(player_id, TEST_GUILD_ID) == -100

        now_ts = int(time.time())

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=[player_id],
            dire_ids=[],
            shuffle_timestamp=now_ts,
            is_bomb_pot=True,
        )

        # Should create bet even with negative balance
        assert result["created"] == 1
        # 10% of -100 = 0 (negative balance treated as 0) + 10 ante = 10 JC
        assert result["total_radiant"] == 10
        # Balance should be -110 now
        assert player_repo.get_balance(player_id, TEST_GUILD_ID) == -110
